# deploy

- `compose/`: Docker Compose for a full local network on one machine (Phase 0).
- Production configs: load balancer, Redis, PostgreSQL, monitoring (Phase 1).
- `node/`: cloud-init and install scripts that turn a new machine into a Hallow node (Phase 1). See [Fleet](../docs/FLEET.md).
- `control-plane/`: the control plane's image (`docker buildx bake control-plane`).

## Two-proxy dev network

`compose/compose.hallow.yaml` is an override for the dev network that adds Valkey, the control plane and a second proxy. Both proxies load the `hallow_vine` plugin, which registers them with the control plane, checks logins (a player can't be online on two proxies at once) and mirrors the server list into Vine, so their `vine.toml` has no static servers. The plain `compose.yaml` network and CI are unchanged.

```sh
git submodule update --init
cargo build --release --target wasm32-wasip2 -p hallow-vine   # build the plugin first
docker compose -f deploy/compose/compose.yaml -f deploy/compose/compose.hallow.yaml up --build -d
cargo run --release -p netcheck -- --proxy 127.0.0.1:25565    # proxy-1
cargo run --release -p netcheck -- --proxy 127.0.0.1:25566    # proxy-2
docker compose -f deploy/compose/compose.yaml -f deploy/compose/compose.hallow.yaml down
```

| Service | Host port | What it is |
|---|---|---|
| `proxy` (alias `proxy-1`), `proxy-2` | 25565, 25566 | Vine with `hallow_vine`; config in `compose/config/hallow/` |
| `control-plane` | 127.0.0.1:18090 | `hallow-control-plane:dev` from `control-plane/Dockerfile` |
| `valkey` | none | The control plane's Redis |
| `postgres` | none | The control plane's PostgreSQL (accounts, bans, ranks, sessions). `HALLOW_ADMINS=netstaff` makes the netcheck bot `netstaff` an admin |
| `registrar` | none | Dev only: registers `lobby` and `game-1` and heartbeats them every 5 s, until the server plugin does |

To see what the control plane knows:

```sh
curl -H "Authorization: Bearer $(cat deploy/compose/config/hallow/control-plane.token)" localhost:18090/v1/servers
```

The token and forwarding secret in `compose/config/hallow/` are checked-in dev defaults. If you set `HALLOW_VINE_SECRET`, change `forwarding.secret` to match. Build the plugin before `up`: if `hallow_vine.wasm` is missing, Docker creates an empty directory in its place, and you have to delete it before cargo can write the file.

## Load balancer

`compose/compose.lb.yaml`, layered on the two-proxy override, puts HAProxy (`compose/config/hallow/haproxy.cfg`) in front of both proxies.

- **Spreading:** leastconn over the proxies.
- **Real IPs:** passed to Vine with PROXY protocol v2.
- **Health:** every 2 s HAProxy asks the control plane's agent port (8081) about each proxy:
  - a drained proxy gets weight 0, so new players go elsewhere while its players stay;
  - an expired proxy is taken out.
- **Ports:** the proxies use `compose/config/hallow/vine-lb.toml` (`proxy_protocol = true`), so they're only reachable through HAProxy.

```sh
docker compose -f deploy/compose/compose.yaml -f deploy/compose/compose.hallow.yaml \
  -f deploy/compose/compose.lb.yaml up --build -d
cargo run --release -p netcheck -- --proxy 127.0.0.1:25565 --bots 4 --stay 60   # through HAProxy
TOKEN=$(cat deploy/compose/config/hallow/control-plane.token)
curl -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"draining":true}' localhost:18090/v1/proxies/proxy-2/drain              # drain proxy-2
curl -H "Authorization: Bearer $TOKEN" localhost:18090/v1/proxies               # players per proxy
```

HAProxy's stats page, with each proxy's state (`UP`, `DRAIN (agent)`), is at <http://localhost:8404/>. In production the same agent check works with any HAProxy; the agent port carries no token, so keep it on the private network.
