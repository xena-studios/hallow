# tools/wings-e2e

Runs the Wings path end to end on one machine, with no cloud machines: the real panel, the control plane with `HALLOW_ORCHESTRATOR=wings`, a node joined with the real `deploy/node/install.sh` and `wings configure --enroll`, Wings (the pinned 1.2.10 release) creating instances from a local registry, and a bot routed to a Wings instance through Vine. Not in CI: it needs privileged containers, host networking and a panel binary.

```sh
# build the panel fork (panel/calagopus, see its DEVELOPMENT.md), or set HALLOW_E2E_PANEL_BIN
tools/wings-e2e/run.sh          # build, up, test, down
tools/wings-e2e/run.sh up       # leave it running; then `test`, and `down` to clean up
```

Each step of `up` also runs on its own (`build infra push panel control-plane node wings proxy`), which helps when fixing one part. `down` removes every container labelled `hallow.e2e=hwe2e` or `hallow.owner=wingse2e`, the network, Wings' files on the Docker host and `target/wings-e2e`.

## What it sets up

```
this machine                    Docker host (OrbStack / Docker Desktop VM)
  panel-rs :8100  <──────────── control plane 10.213.0.10 ── Postgres .5, Valkey .6
  netcheck ──> 127.0.0.1:25590 ─> Vine proxy 10.213.0.20
                                  node container (Debian 13, --network host, privileged,
                                    Docker socket): install.sh, then Wings on :18480
                                  instances (made by Wings on network hwe2e),
                                    game ports bound on 10.213.0.1:30100-30119
                                  registry localhost:15000
```

- **The node's private IP is 10.213.0.1**, the e2e bridge's gateway. That is an address of the Docker host itself, where Wings runs (host networking), so the control plane reaches Wings' API there and Docker can bind game ports on it, just as a real node binds them on its WireGuard address.
- **install.sh** runs for real, with `--skip-docker --skip-sysctl --no-systemd --no-firewall` (Docker is the host's socket, there is no systemd in a container, and the kernel and firewall belong to the Docker host). It installs its packages, downloads and checks Wings 1.2.10, sends the join to the control plane, which creates the node, its allocations and an enrollment code in the panel, and runs `wings configure --enroll`.
- **Wings** then starts with three environment overrides, needed only because this node shares the Docker host with everything else: its own data directories (`/var/lib/hallow-wings-e2e`, `/run/hallow-wings-e2e`, mounted at the same paths in the node container, since Wings hands them to Docker as bind mounts) and the e2e network for its containers.
- **Images** are pushed to the local registry and the local copies dropped, so Wings has to pull them.

## What it checks

1. The node is in `GET /v1/nodes` and in the panel.
2. The hub kind's warm instance: Wings fetches its configuration from the control plane (the remote shim), pulls the image, starts the container read-only as its own user with the game port on the private IP, and the server registers with its own scoped token. The script checks the container's `HALLOW_CONTROL_PLANE_TOKEN` is a `hit_` token that gets 403 from `GET /v1/instances`.
3. A bot joins through Vine, lands on the hub (on Wings), runs `/play test-game`, and the control plane sends it to a test-game instance on Wings.
4. A second bot switches to that instance by name with `/server` and back.
5. `DELETE /v1/instances/{name}`: the instance stops, Wings deletes the server and its container.

## Settings

| Variable | Default | |
|---|---|---|
| `HALLOW_E2E_PANEL_BIN` | `panel/calagopus/target/debug/panel-rs` | The panel fork, built for this machine |
| `HALLOW_E2E_SKIP_BUILD` | `0` | `1` uses images already tagged `wingse2e` |
| `HALLOW_E2E_PROXY_IMAGE` | `hallow-proxy:wingse2e` | Any Vine image; the plugin is mounted |
| `HALLOW_E2E_KEEP` | `0` | `1` leaves everything up after `run.sh` |
| `HALLOW_E2E_NET` | `10.213.0` | A free /24 |
| `HALLOW_E2E_PANEL_PORT`, `_CP_PORT`, `_PROXY_PORT`, `_REGISTRY_PORT`, `_PG_PORT`, `_REDIS_PORT` | 8100, 18490, 25590, 15000, 55490, 56390 | Host ports |
| `HALLOW_E2E_WINGS_PORT`, `_SFTP_PORT`, `_GAME_PORTS` | 18480, 12022, 30100-30119 | Ports on the Docker host |
| `HALLOW_E2E_HOST` | `host.docker.internal` | How containers reach this machine (the panel) |

## Limits

- **The panel can't reach Wings.** The panel runs on this machine and the node's address is inside the Docker host, so the panel shows the node offline. Nothing in the run needs the panel to call Wings; console and file access from the panel are untested.
- **No WireGuard, firewall or sysctl.** Those steps are skipped; they need a real machine (and `nft -c` was never run against the real ruleset here).
- **One node, arm64.** The pinned x86_64 Wings build and placement across nodes aren't exercised.
