# tools/wings-e2e

Runs the Wings path end to end on one machine, with no cloud machines: the real panel, the control plane with `HALLOW_ORCHESTRATOR=wings`, a node joined with the real `deploy/node/install.sh` and `wings configure --enroll`, Wings (the pinned 1.2.10 release) creating instances from a local registry, a bot routed to a Wings instance through Vine, and staff using the node, a panel server and an instance's console through the panel's own API. Not in CI: it needs privileged containers, host networking and a panel binary.

```sh
# build the panel fork (panel/calagopus, see its DEVELOPMENT.md), or set HALLOW_E2E_PANEL_BIN
tools/wings-e2e/run.sh          # build, up, test, down
tools/wings-e2e/run.sh up       # leave it running; then `test`, and `down` to clean up
```

Each step of `up` also runs on its own (`build infra push panel control-plane node wings proxy panel-server`), which helps when fixing one part. A failed run is left up for a look. `down` removes every container labelled `hallow.e2e=<name>` or `hallow.owner=<tag>`, every container on the e2e network or from the console image (the panel server), the network, Wings' files on the Docker host and `target/wings-e2e`.

## What it sets up

```
this machine                      Docker host (OrbStack VM, or this machine on Linux)
  panel-rs :8100 ──────────────────> Wings 192.168.139.2:18480 (the node's private IP)
    /wings-proxy/<node>/… ─────────┘
    <───────────────────────────── control plane 10.213.0.10 ── Postgres .5, Valkey .6
  netcheck ──> 127.0.0.1:25590 ──> Vine proxy 10.213.0.20
  console.mjs ─> panel ws          node container (Debian 13, --network host, privileged,
                                     Docker socket): install.sh, then Wings
                                   instances and the panel server (made by Wings on
                                     network hwe2e), ports bound on the private IP
                                   registry localhost:15000
```

- **The node's private IP is an address of the Docker host that this machine reaches.** `infra` takes the Docker host's own source address (`ip route get` in a host-network container: on OrbStack the VM's address on its link to the Mac, 192.168.139.2; on Linux the machine's own) and checks that this machine connects to Wings' port there, or stops. Set `HALLOW_E2E_NODE_IP` to choose one. This is how a real deployment looks: the panel and the control plane reach Wings at the node's WireGuard address, and Docker binds game ports on it. Docker Desktop for Mac has no such address, so the run needs OrbStack there.
- **install.sh** runs for real, with `--skip-docker --skip-sysctl --no-systemd --no-firewall` (Docker is the host's socket, there is no systemd in a container, and the kernel and firewall belong to the Docker host). It installs its packages, downloads and checks Wings 1.2.10, sends the join to the control plane, which creates the node, its allocations and an enrollment code in the panel, and runs `wings configure --enroll`.
- **Browsers reach Wings through the panel.** The panel runs with `APP_ENABLE_WINGS_PROXY=true`, and the control plane sets each new node's public URL to the panel's Wings proxy (`HALLOW_NODE_PUBLIC_URL=<panel>/wings-proxy/{uuid}`). The panel's node record keeps `http://<private IP>:<port>` for its own calls.
- **Wings** then starts with three environment overrides, needed only because this node shares the Docker host with everything else: its own data directories (`/var/lib/hallow-wings-<name>`, `/run/hallow-wings-<name>`, mounted at the same paths in the node container, since Wings hands them to Docker as bind mounts) and the e2e network for its containers.
- **A panel server**, made the way staff make long-lived servers: a nest and an egg for an echo console (a small Alpine image that runs the egg's startup command), an allocation outside the game ports, and the server, with the installer skipped. Wings fetches its configuration from its remote, the control plane, which passes the call to the panel.
- **Images** are pushed to the local registry and the local copies dropped, so Wings has to pull them.
- **API keys.** The control plane's key has the permissions [deploy/node](../../deploy/node/README.md#joining) lists. The checks use a second, "staff" key: `nodes.read`, `nodes.read-token`, `servers.read`, `hallow.instances.read` and `hallow.instances.console`, and on servers `control.*` and `files.*` (no admin session).

## What it checks

1. The node is in `GET /v1/nodes`, and in the panel with its private URL and the Wings proxy as its public URL.
2. **The panel shows the node online**, both ways it asks: Wings' `/api/system` with the node's token at the node's public URL (what the node list does in the browser), and the panel's own `GET /api/admin/nodes/{node}/system/overview`.
3. The hub kind's warm instance: Wings fetches its configuration from the control plane (the remote shim), pulls the image, starts the container read-only as its own user with the game port on the private IP, and the server registers with its own scoped token. The script checks the container's `HALLOW_CONTROL_PLANE_TOKEN` is a `hit_` token that gets 403 from `GET /v1/instances`.
4. A bot joins through Vine, lands on the hub (on Wings), runs `/play test-game`, and the control plane sends it to a test-game instance on Wings.
5. A second bot switches to that instance by name with `/server` and back.
6. `DELETE /v1/instances/{name}`: the instance stops, Wings deletes the server and its container.
7. **The panel server's console** (`console.mjs`, Wings' websocket protocol as the panel's console uses it): `GET /api/client/servers/{server}/websocket` answers a token and a URL on the panel's Wings proxy; over it, `auth`, the console history, a command sent over the socket and one through `POST …/command`, both echoed back live; then `POST …/power {"action": "restart"}` with the status going `stopping`, `offline`, `starting`, `running`, and a command after the restart.
8. **Files**: `POST …/files/write`, `GET …/files/list` and `GET …/files/contents` on the panel server, and the file in the server's volume on the node.
9. **The Instances page**: `GET /api/admin/hallow/instances` lists the hub.
10. **An instance's console**: `GET /api/admin/hallow/instances/{name}/websocket` (the Hallow extension signs a Wings token for the instance, which has no panel server), then the hub's history and a command echoed live, through the Wings proxy.

## Settings

| Variable | Default | |
|---|---|---|
| `HALLOW_E2E_PANEL_BIN` | `panel/calagopus/target/debug/panel-rs` | The panel fork, built for this machine |
| `HALLOW_E2E_SKIP_BUILD` | `0` | `1` uses images already tagged `<tag>` |
| `HALLOW_E2E_PROXY_IMAGE` | `hallow-proxy:<tag>` | Any Vine image; the plugin is mounted |
| `HALLOW_E2E_KEEP` | `0` | `1` leaves everything up after `run.sh` |
| `HALLOW_E2E_NAME`, `HALLOW_E2E_TAG` | `hwe2e`, `wingse2e` | Container names and labels; image tag, Redis prefix and instance owner. Change both to run beside another copy |
| `HALLOW_E2E_NET` | `10.213.0` | A free /24 |
| `HALLOW_E2E_NODE_IP` | the Docker host's address | The node's private IP; must be an address of the Docker host this machine reaches |
| `HALLOW_E2E_PANEL_PORT`, `_CP_PORT`, `_PROXY_PORT`, `_REGISTRY_PORT`, `_PG_PORT`, `_REDIS_PORT` | 8100, 18490, 25590, 15000, 55490, 56390 | Host ports |
| `HALLOW_E2E_WINGS_PORT`, `_SFTP_PORT`, `_GAME_PORTS`, `_STATIC_PORT` | 18480, 12022, 30100-30119, 30150 | Ports on the Docker host; the static port is the panel server's |
| `HALLOW_E2E_HOST` | `host.docker.internal` | How containers reach this machine (the panel) |

## Results

2026-11-16, OrbStack on arm64, a clean `run.sh` (build, up, test): every check above passed. The run found:

- **Browsers couldn't reach Wings.** The control plane created nodes with only their private URL, so the node list's online check, the console websocket and file downloads went from the browser to the node's WireGuard address. Nodes now get the panel's Wings proxy as their public URL (`HALLOW_NODE_PUBLIC_URL`).
- **Instances had no console in the panel.** The Hallow extension now signs Wings console tokens for them ([Instances](../../docs/INSTANCES.md#consoles)).
- **Wings sends a socket live output only after its first permission check**, up to a second after `auth success`; output in that second reaches it only through `send logs`. `console.mjs` waits for it.
- In this script: `run.sh` wasn't executable, `build` didn't build the in-tree servers' plugins (a fresh checkout couldn't build the hub image), and Wings' data directories were shared by every copy of the run.

## Limits

- **No WireGuard, firewall or sysctl.** Those steps are skipped; they need a real machine (and `nft -c` was never run against the real ruleset here). The node's private IP stands in for a WireGuard address.
- **One node, arm64.** The pinned x86_64 Wings build and placement across nodes aren't exercised.
- **The panel's API, not a browser.** The checks call the same routes the panel's pages call, and speak Wings' websocket protocol as its console does, but no page is rendered. SFTP isn't tried.
- **Plain HTTP.** The panel and Wings talk over HTTP, as they would inside WireGuard; the panel's public URL would be HTTPS in production, making the console `wss://`.
