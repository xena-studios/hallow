# deploy/node

Turns a blank machine into a Hallow node with one command (dedicated) or one cloud-init file (cloud). See [Fleet](../../docs/FLEET.md#setting-up-a-new-node).

- `install.sh`: the setup, for Debian 12, Debian 13 and Ubuntu 24.04 (x86_64 and aarch64). It refuses any other OS.
- `cloud-init.yaml`: a template that writes `install.sh` and its settings to the machine and runs it on first boot. [`hallow_fleet::bootstrap::render_cloud_init`](../../crates/fleet/src/bootstrap.rs) fills it in, and the fleet manager passes the result to the provider as user data.
- `test-install.sh`: dry-run tests for `install.sh` (CI runs it with `shellcheck`).

## What install.sh does

Every step checks what is already there, so running it again is safe. It changes only what differs and restarts only what it changed.

1. **Packages:** `curl`, `jq`, `nftables`, `wireguard-tools` and a few more.
2. **Docker** from Docker's own apt repository (`download.docker.com`), with the `local` log driver and log rotation.
3. **Kernel tuning** in `/etc/sysctl.d/90-hallow.conf`:
   - BBR and `fq`;
   - bigger socket buffers and backlogs;
   - room for many connections, containers, files and inotify watches;
   - outgoing ports from 40000 up, so they never collide with game ports.
4. **WireGuard** `wg0` at `--wg-address`, with the peers from `--wg-peer`. The private key is made on the machine and never leaves it; the join sends only the public key.
5. **Firewall:** its own nftables table, `inet hallow`, loaded by `hallow-firewall.service`. It doesn't flush the ruleset, so Docker's rules stay. An active `ufw` is turned off, since the two would fight.
   - **Input** is dropped except:
     - SSH, from `--ssh-allow` CIDRs or from anywhere;
     - WireGuard's port;
     - Wings' API (8080) and SFTP (2022), from the private interfaces and `--wings-allow`;
     - the game ports, from the private interfaces;
     - ICMP and DHCP.
   - **Game ports** (`--game-ports`, default 30000-30999) are dropped at prerouting, before Docker's DNAT, unless they arrive on `lo`, `wg0`, a `--private-if`, or a Docker bridge. Docker publishes container ports past the input chain, so blocking them there wouldn't work.

   The control plane also makes every allocation on the node's private IP, so Docker binds game ports only there. Players reach games through the load balancer and Vine over the private network ([Architecture](../../docs/ARCHITECTURE.md)); nothing on a node is public but SSH.
6. **Wings**, the pinned Calagopus release, to `/usr/local/bin/wings`:
   - the SHA-256 is checked against GitHub's published digest before installing;
   - the script writes `wings.service` itself: the unit `wings service-install` makes, ordered after `wg0` and the firewall, with a higher file limit. It doesn't call `service-install`, because that also starts Wings, and with no config yet Wings would start in pairing mode.
7. **Registration** with a one-time token (see below), then `wings configure --enroll`, then `/etc/hallow/node.json` (role, region, labels). Once the token is spent, its line is removed from the env file.
8. **Wings starts.** If Wings already has a config, registration is skipped unless you pass `--rejoin`.

`--dry-run` prints every command and every file it would write, and changes nothing. It doesn't need root, and it prints neither the join token nor the WireGuard private key.

## Settings

Every flag has a `HALLOW_*` environment variable, and `--env-file` reads them from a file. Flags beat the environment, and the environment beats the file. The file is parsed, never sourced: only `HALLOW_*=value` lines are accepted. `install.sh --help` lists them all.

| Flag | Variable | |
|---|---|---|
| `--control-plane URL` | `HALLOW_CONTROL_PLANE_URL` | Where the join token is exchanged |
| `--join-token TOKEN` | `HALLOW_JOIN_TOKEN` | From `POST /v1/nodes/join-tokens` |
| `--panel-url URL` | `HALLOW_PANEL_URL` | With `--enroll-code`, for a node made in the panel by hand |
| `--enroll-code CODE` | `HALLOW_ENROLL_CODE` | The panel's own one-time Wings enrollment code |
| `--role base\|burst` | `HALLOW_ROLE` | Default `burst` |
| `--region NAME` | `HALLOW_REGION` | e.g. `fsn1` |
| `--label K=V` (repeat) | `HALLOW_LABELS` (space-separated) | |
| `--private-ip IP` | `HALLOW_PRIVATE_IP` | Default: the WireGuard address |
| `--wg-address CIDR` | `HALLOW_WG_ADDRESS` | e.g. `10.42.1.7/16` |
| `--wg-port PORT` | `HALLOW_WG_PORT` | Default 51820 |
| `--wg-peer SPEC` (repeat) | `HALLOW_WG_PEERS` (space-separated) | `PUBKEY\|ENDPOINT\|CIDR[,CIDR]\|KEEPALIVE`; the endpoint may be empty |
| `--private-if NAME` (repeat) | `HALLOW_PRIVATE_IFS` | Hetzner vSwitch or Cloud Network, OVHcloud vRack |
| `--ssh-allow CIDR` (repeat) | `HALLOW_SSH_ALLOW` | Default: anywhere |
| `--ssh-port PORT` | `HALLOW_SSH_PORT` | Default 22 |
| `--wings-allow CIDR` (repeat) | `HALLOW_WINGS_ALLOW` | Extra sources for Wings' API and SFTP, such as the panel's address if it isn't on the private network |
| `--game-ports LO-HI` | `HALLOW_GAME_PORTS` | Default 30000-30999, below 40000; must match the control plane's `HALLOW_NODE_GAME_PORTS` |
| `--wings-version`, `--wings-url`, `--wings-sha256` | `HALLOW_WINGS_*` | Another Wings build; the URL may use `{version}` and `{arch}`. Anything but the pinned build needs its SHA-256 |
| `--skip-register`, `--rejoin`, `--no-firewall`, `--dry-run` | | |
| `--skip-docker` | `HALLOW_SKIP_DOCKER=1` | Docker already runs (it must answer on `/var/run/docker.sock`) |
| `--skip-sysctl` | `HALLOW_SKIP_SYSCTL=1` | Leave kernel settings alone |
| `--no-systemd` | `HALLOW_NO_SYSTEMD=1` | No units and no `systemctl`: the firewall and WireGuard are loaded directly, and you start `/usr/local/bin/wings` yourself. Needs `--skip-docker`. For containers, such as the end-to-end run |

## Joining

The panel can mint single-use Wings enrollment codes, but only for a node that already exists, and they last 30 minutes. A cloud machine's address isn't known until it boots, and a dedicated one can take hours to deliver. So the control plane issues its own tokens and creates the panel node when the machine shows up:

1. The fleet manager, or a staff member, calls `POST /v1/nodes/join-tokens` with the control plane's bearer token: `{"role": "burst", "region": "fsn1", "name": "burst-7", "labels": {...}, "ttl_seconds": 86400}`. The answer holds `hjt_…`, shown only this once; PostgreSQL keeps its SHA-256 (`hallow.node_join_tokens`). The default lifetime is 24 h, at most 7 days.
2. The token goes into the cloud-init or the install command.
3. On first boot `install.sh` sends `POST /v1/nodes/join` with `Authorization: Bearer hjt_…`: the hostname, private IP, public IP, WireGuard public key, memory, disk and architecture.
4. The control plane claims the token for two minutes (one atomic `UPDATE`, so two machines can't both win). Then it calls the panel's admin API with its own key:
   - creates the node, at `http://<private ip>:8080`, in the panel location mapped to the token's region;
   - adds the game-port allocations on the private IP;
   - mints a Wings enrollment code.

   In one transaction it records the node in `hallow.nodes` and marks the token used. It answers with the panel URL and the enrollment code.
5. `install.sh` runs `wings configure --enroll <code> --panel-url <url>`. Wings redeems the code with the panel (`POST /api/remote/enroll`), which rotates the node's token and hands it to Wings directly. When the control plane runs instances on Wings (`HALLOW_ORCHESTRATOR=wings`), the code also tells Wings to use the control plane as its remote (`HALLOW_NODE_WINGS_REMOTE`). The control plane then answers for its instances, passes every other call to the panel, and reads the node's token from the panel (`nodes.read-token`) to call Wings ([Instances](../../docs/INSTANCES.md)).
6. Wings starts and the panel shows the node online.

If a panel call fails half way, the control plane deletes the half-made panel node and gives the token back. It answers 503, so the script's retries (curl, 10 attempts) try again with the same token. A bad request or a conflict (a node of that name exists) doesn't spend the token either. Only a finished join spends it. A used, expired or unknown token gets 401.

**By hand, without the control plane:** create the node in the panel, ask it for an enrollment code (the node's page, or `POST /api/admin/nodes/{uuid}/enrollment`), and run:

```sh
curl -fsSLO https://raw.githubusercontent.com/xena-studios/hallow/main/deploy/node/install.sh
less install.sh   # read it first
sudo bash install.sh --panel-url https://panel.example.net --enroll-code <code> \
  --role base --region fsn1 --wg-address 10.42.0.20/16 \
  --wg-peer '<hub public key>|hub.example.net:51820|10.42.0.0/16|25'
```

Add the game-port allocations on the node's private IP in the panel yourself.

**A dedicated machine with a control-plane token:**

```sh
sudo bash install.sh --control-plane https://cp.example.net --join-token hjt_... \
  --role base --region fsn1 --wg-address 10.42.0.20/16 \
  --wg-peer '<hub public key>|hub.example.net:51820|10.42.0.0/16|25' \
  --ssh-allow 203.0.113.0/24
```

## Cloud-init

```rust
let user_data = hallow_fleet::render_cloud_init(&NodeBootstrap {
    control_plane_url: "https://cp.example.net".into(),
    join_token: token.token,
    role: NodeKind::Burst,
    region: "fsn1".into(),
    wireguard: Some(WireGuard { address: "10.42.1.7/16".into(), listen_port: None, peers: hub_peers }),
    ..
})?;
provider.create(&NodeSpec { user_data, .. }).await?;
```

- **What it writes:** `/etc/hallow/node.env` (mode 0600) with the `HALLOW_*` settings, and `install.sh` to `/usr/local/sbin/hallow-node-install` (gzipped and base64-encoded, about 11 KB).
- **What it runs:** the script once, from `runcmd`, with `--env-file`. Output goes to `/var/log/cloud-init-output.log`.
- **Checks:** `NodeBootstrap::validate` checks every value against what the script accepts. A value with a space or line break can't add settings. The render is refused if it's over 32 KiB, Hetzner's user-data limit.
- **Always in sync:** the script and template are compiled into `hallow-fleet`, so the user data always matches the control plane that sends it.

## Control plane settings

| Variable | |
|---|---|
| `HALLOW_DATABASE_URL` | Required for tokens |
| `HALLOW_PANEL_URL` | Where the control plane calls the panel's admin API |
| `HALLOW_PANEL_API_KEY` or `_FILE` | Admin API key with `nodes.create`, `nodes.allocations`, `nodes.reset-token` and `nodes.delete` |
| `HALLOW_PANEL_PUBLIC_URL` | The URL Wings uses for the panel (default `HALLOW_PANEL_URL`) |
| `HALLOW_PANEL_LOCATIONS` | `fsn1=<location uuid>,nbg1=<uuid>,*=<uuid>` |
| `HALLOW_NODE_GAME_PORTS` | Allocations per node, default `30000-30999` |
| `HALLOW_NODE_WINGS_PORT`, `HALLOW_NODE_SFTP_PORT` | Default 8080 and 2022 |
| `HALLOW_JOIN_TOKEN_TTL_HOURS` | Default 24, at most 168 |

`GET /v1/nodes` lists joined nodes with their private IP and WireGuard public key.

## Tests

```sh
shellcheck deploy/node/*.sh
deploy/node/test-install.sh                       # dry runs: OS detection, firewall, WireGuard, env file, refusals
cargo test -p hallow-fleet bootstrap              # render, YAML, script round trip, injection, size
cargo test -p hallow-fleet --test bootstrap_dry_run   # rendered cloud-init drives install.sh --dry-run
HALLOW_TEST_REDIS_URL=redis://127.0.0.1:6379 HALLOW_TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/postgres \
  cargo test -p hallow-control-plane --test nodes  # issue and join against a mock panel
```

## End to end, locally

`tools/wings-e2e/run.sh` ([README](../../tools/wings-e2e/README.md)) runs `install.sh` for real in a Debian 13 container on the Docker host (host networking, the Docker socket mounted), against the real panel and control plane:

```sh
docker exec hwe2e-node bash /root/install.sh \
  --control-plane http://10.213.0.10:8080 --join-token hjt_... \
  --role burst --region local --private-ip 10.213.0.1 --game-ports 30100-30119 \
  --no-firewall --skip-docker --skip-sysctl --no-systemd
```

Result (2026-11-16, OrbStack on arm64): packages installed; Wings 1.2.10 downloaded and its SHA-256 checked; the join created the node, 20 game-port allocations and an enrollment code in the panel through the control plane's panel client (`nodes.create`, `nodes.allocations`, `nodes.reset-token`, then `nodes.read-token` for Wings' token), with the control plane as Wings' remote; `wings configure --enroll` wrote the config (API port and SFTP port from the panel node); Wings started, and the control plane ran hub and game instances on it. The run found and fixed a remote-shim bug that stopped Wings booting ([Instances](../../docs/INSTANCES.md#wings-without-a-panel-record-per-instance)).

## Not done yet

- **Run on real machines.** The script has run for real only in a container, with Docker, the kernel tuning, WireGuard, the firewall and systemd skipped. A real Debian or Ubuntu machine should still check:
  - Docker installs from its repository and Wings runs under `wings.service`;
  - `nft -c` accepts the ruleset;
  - Wings shows online in the panel (in the local run the panel couldn't reach the node's address);
  - game ports are unreachable from the public address.
- **The WireGuard hub learning new peers.** A node sends its public key and the control plane stores it (`GET /v1/nodes`). Nothing yet adds it to the hub's `wg0`. Until then a node's WireGuard peers must already accept it: add it by hand, or give the hub a sync job that reads `GET /v1/nodes`. Picking a free WireGuard address per node is the fleet manager's job and isn't built either.
- **TLS to Wings.** The panel talks to Wings over plain HTTP on the private address, inside WireGuard.
- **The fleet loop** that issues a token, renders the cloud-init and calls `Provider::create` isn't wired up (the fleet manager loop doesn't exist yet). The Hetzner provider already sends `NodeSpec::user_data` as is, and a test checks the rendered cloud-init gets through unchanged.
- **Removing a node.** Draining and cancelling don't delete the panel node or the `hallow.nodes` row yet.
- **A Hallow Wings build.** `install.sh` pins Calagopus' release 1.2.10, which matches the panel fork (v1.2.10). `--wings-url` and `--wings-sha256` switch to our own build once we publish one.
