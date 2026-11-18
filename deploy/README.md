# deploy

- `compose/`: Docker Compose for a full local network on one machine (Phase 0).
- Production configs: load balancer, Redis, PostgreSQL, monitoring (Phase 1).
- `node/`: the install script and cloud-init template that turn a blank Debian or Ubuntu machine into a Hallow node and join it with a one-time token. See [node/README.md](node/README.md) and [Fleet](../docs/FLEET.md#setting-up-a-new-node).
- `control-plane/`: the control plane's image (`docker buildx bake control-plane`).

## Two-proxy dev network

`compose/compose.hallow.yaml` is an override for the dev network that adds Valkey, the control plane and a second proxy. Both proxies load the `hallow_vine` plugin, which registers them with the control plane, checks logins (a player can't be online on two proxies at once) and mirrors the server list into Vine, so their `vine.toml` has no static servers. The `lobby` and `game-1` servers register themselves through the `hallow_server` plugin. The plain `compose.yaml` network and CI are unchanged.

```sh
git submodule update --init
servers/build-plugins.sh   # build the plugins first
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
| `lobby`, `game-1` | none | The `hub` and `test-game` server images (from `servers/`) with the dev build of `hallow_server` mounted, registering with the control plane as kinds `hub` and `test-game`. Kind, role and max players come from each `hallow.toml`; name, address and the control plane from `compose/config/hallow/` |

To see what the control plane knows:

```sh
curl -H "Authorization: Bearer $(cat deploy/compose/config/hallow/control-plane.token)" localhost:18090/v1/servers
```

The token and forwarding secret in `compose/config/hallow/` are checked-in dev defaults. If you set `HALLOW_VINE_SECRET`, change `forwarding.secret` to match. Build the plugins before `up`: if `hallow_vine.wasm` or `hallow_server.wasm` is missing, Docker creates an empty directory in its place, and you have to delete it before cargo can write the file.

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

## Bedrock through Geyser

`compose/compose.geyser.yaml` adds Geyser Standalone in front of the proxy, so Bedrock players can join:

- **Image:** `geyser/Dockerfile` pins Geyser 2.12.1 build 1302 and checks the jar's published SHA-256.
- **Ports:** Bedrock clients connect to `localhost:19132`, over RakNet or NetherNet (signaling on 19132/tcp, WebRTC on 19133/udp).
- **Path:** Geyser joins Vine's Geyser-only listener (`proxy:25567`, not published) with Floodgate's encrypted handoff (`auth-type: floodgate`). Vine verifies it with the shared key and gives the player Floodgate's Java identity: `.` + gamertag (cut to 16), UUID `00000000-0000-0000-xxxx-xxxxxxxxxxxx` from the xuid, and the player's own IP, forwarded to the servers like any other player. A login on 25567 without a valid handoff is refused; on 25565 a forged one is refused and a Java player needs none.
- **Key:** `compose/config/floodgate/key.pem`, 16 raw bytes (Floodgate's format, not actually PEM), mounted into Geyser and every proxy. Make a new one with `head -c 16 /dev/urandom > key.pem`.

```sh
docker compose -f deploy/compose/compose.yaml -f deploy/compose/compose.geyser.yaml up --build -d
docker run --rm --network compose_default hallow-bedrockbot geyser 19132 Steve 2535400000000001   # tools/matrix bot: gamertag, xuid
```

Rules for now:

- **Dev only:** the key is checked in, and Geyser's config (`compose/config/geyser/config.yml`) doesn't validate Bedrock logins, so a bot can claim any gamertag and xuid. A real network generates its own key, keeps it with the other secrets, and leaves `validate-bedrock-login` on, so the xuid is the one Xbox Live signed.
- **No load balancer:** Geyser connects to the proxy directly, so don't combine it with `compose.lb.yaml` yet.

## Monitoring

`compose/compose.monitoring.yaml`, layered on the two-proxy override, adds:

- **Prometheus** (`localhost:19090`): scrapes the control plane's `/metrics` every 5 s.
- **Loki**, fed by **Alloy**: keeps the logs of every container in the compose project.
- **Grafana** (`localhost:13000`): anonymous viewer, `admin` / `hallow-dev`.

```sh
docker compose -f deploy/compose/compose.yaml -f deploy/compose/compose.hallow.yaml \
  -f deploy/compose/compose.monitoring.yaml up --build -d
```

The provisioned **Hallow network** dashboard shows:

- players online, the limit, proxies, ready lobbies, game instances and queued players;
- players over time, per proxy and per server;
- instances by kind and state;
- tick time per server, against the 50 ms budget;
- queues by game;
- warnings and errors from the logs.

Each server's `hallow_server` plugin reports its tick time in its heartbeat.

Alert rules (`config/monitoring/alerts.yml`) cover:

- the control plane being down;
- no proxies, or no ready lobby;
- a server over its tick budget;
- the network nearly full.

They show on Prometheus' and Grafana's alert pages; production needs an Alertmanager for paging. Everything here is dev-only (open dashboards, a fixed password).
