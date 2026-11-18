# Dev network

One proxy and two servers on one machine, built from the pinned submodules.

```sh
git submodule update --init
servers/build-plugins.sh
docker compose -f deploy/compose/compose.yaml up --build -d
cargo run --release -p netcheck
```

`netcheck` joins through the proxy as a headless 26.3 player, switches to `game-1` and back, and exits non-zero if any step fails:

```text
netcheck: joining 127.0.0.1:25565 as netcheck (Java 777)
  ok  joined the default server (34 ms)
  ok  switched to game-1 (177 ms)
  ok  switched to lobby (25 ms)
netcheck: passed in 292 ms
```

Options: `--proxy <addr>`, `--name <player>`, `--route game-1,lobby,…`.

Load test: `netcheck --bots 100 --ramp 20 --stay 40` (raise Vine's `max_connections_per_sec`, `connection_burst` and `max_players` first; every bot connects from one IP).

Older Java clients (1.7.2 to 26.2) join too: Vine translates them, and the servers only see 26.3. The bots in [`tools/matrix`](../../tools/matrix) check each version.

To play, connect a 26.3 client to `localhost:25565` and use `/server game-1` or `/server lobby`. To try a change to a server, edit its `hallow.toml` and run `up --build` again. The proxy runs in offline mode, so any username works.

| Service | Image | What it is |
|---|---|---|
| `proxy` | `hallow-proxy:dev` from `templates/proxy` | Vine from our fork, config in `config/vine.toml` |
| `lobby` | `hallow-hub:dev` from [`servers/hub`](../../servers/hub) | The hub, built the way a server repo builds: `FROM` the server base image plus its `hallow.toml` and its plugin (the game menu; it lists no modes without a control plane) |
| `game-1` | `hallow-test-game:dev` from [`servers/test-game`](../../servers/test-game) | A test game, built the same way |
| `server-base` | `hallow-server-base:dev` from `templates/server` | Build only (scale 0): the server base image, with Pumpkin, the `hallow_server` plugin loaded but idle (no control plane here) and `hallow-manifest` |

The servers' Dockerfiles start `FROM ghcr.io/xena-studios/hallow-server-base:0.2`; compose passes a build context of that name pointing at the `server-base` service, so they build on the base image from this tree instead of pulling one. On start, the base image's entrypoint applies each server's `hallow.toml` (kind, role, max players, Pumpkin settings); `HALLOW_SERVER_NAME` sets the instance name. The forwarding secret is a dev default; set `HALLOW_VINE_SECRET` and change it in `config/vine.toml` for anything reachable from outside.

The first build compiles Pumpkin and Vine for Linux and takes a while; later builds reuse the cargo cache.

Image tags default to `dev`; set `HALLOW_IMAGE_TAG` to build and run your own set side by side with someone else's (`hallow-proxy:$HALLOW_IMAGE_TAG`, and so on).

## Dynamic games

[`compose.instances.yaml`](compose.instances.yaml), over [`compose.hallow.yaml`](compose.hallow.yaml) (two proxies, the control plane, Valkey and PostgreSQL), lets the control plane start and delete game instances itself through the local Docker Engine, the way it does through Wings on real nodes ([Instances](../../docs/INSTANCES.md)):

- `game-1` is built but not started;
- the hub (`lobby`) stays a static service, the network's floor;
- the control plane gets the Docker socket and registers `test-game` and `test-game-lobby` from [`config/hallow/kinds.toml`](config/hallow/kinds.toml) (each an image plus its `hallow.toml`), and keeps one `test-game-lobby` instance warm;
- test-game matches last `TEST_GAME_MATCH_SECS` (default 30, through `HALLOW_INSTANCE_ENV`), then send their players back to `test-game-lobby`;
- `test-game-lobby` is built but not started by compose (the control plane runs it).

Mounting the Docker socket gives the control plane root on the host, so this override is for development only.

```sh
servers/build-plugins.sh
export HALLOW_IMAGE_TAG=gatetest HALLOW_DOCKER_NETWORK=gatetest
files="-f deploy/compose/compose.yaml -f deploy/compose/compose.hallow.yaml -f deploy/compose/compose.instances.yaml"
docker compose -p gatetest $files up --build -d
cargo run --release -p netcheck -- --proxy 127.0.0.1:25566 --name gatebot --route "/play test-game,~15"
curl -s -H "Authorization: Bearer $(cat deploy/compose/config/hallow/control-plane.token)" localhost:18090/v1/instances
```

The bot joins the hub through the second proxy and queues. The control plane starts a `test-game-xxxxxx` container (`test-game` keeps one warm instance while anyone is online, so it may already be up), its server registers, and the proxy moves the bot there. Ten seconds after the bot leaves (`HALLOW_INSTANCE_EMPTY_SECS`), the instance is deleted. `GET /v1/instances` shows each one's state as it goes.

### Hub to lobby to game and back

The whole path a player takes, through the hub's game menu commands (Vine's `/lobby`, which the menu and its clickable text also use):

```sh
export HALLOW_IMAGE_TAG=hubtest HALLOW_DOCKER_NETWORK=hubtest TEST_GAME_MATCH_SECS=20
docker compose -p hubtest $files up --build -d
cargo run --release -p netcheck -- --name hubbot --stay 0 \
  --route "~3,/lobby test-game,~5,/play test-game,~15,~30"
```

The bot lands in the hub (the static `lobby`), `/lobby test-game` sends it to the warm `test-game-lobby-xxxxxx` instance, `/play test-game` queues it and the matchmaker moves it into a `test-game-xxxxxx` instance. The match ends after 20 s: the server turns `draining`, its server plugin sends the bot back to the lobby, and the emptied game instance is deleted. `/hub` brings it back to the hub from anywhere. `GET /v1/lobbies` shows the modes and their player counts, as the hub's menu does.

Instances aren't compose services, so `down` doesn't remove them. Stop the control plane first, so it doesn't start a new warm lobby meanwhile, then delete them:

```sh
docker compose -p gatetest $files stop control-plane
docker rm -f $(docker ps -aq --filter label=hallow.owner=$HALLOW_DOCKER_NETWORK)
docker compose -p gatetest $files down -v
```

## Fleet manager in dry run

[`compose.fleet.yaml`](compose.fleet.yaml), over the three files above, turns the fleet manager on with [`config/hallow/fleet.toml`](config/hallow/fleet.toml) ([Fleet](../../docs/FLEET.md#the-fleet-manager)). The floor is the local Docker Engine with 32 slots. Queue past that and the fleet manager "buys" a dry-run node; with the Docker orchestrator its instances run on the same engine. Once the games end it is drained and cancelled. No provider is ever called: dry run is on by default and no token is set.

```sh
files="$files -f deploy/compose/compose.fleet.yaml"
docker compose $files up --build -d
curl -s -H "Authorization: Bearer $(cat deploy/compose/config/hallow/control-plane.token)" localhost:18090/v1/fleet
curl -s -H "Authorization: Bearer $(cat deploy/compose/config/hallow/control-plane.token)" "localhost:18090/v1/fleet/events?limit=20"
```

Idle proxies are drained too: after ten quiet minutes the second proxy stops taking new players. Set `players_per_proxy` to `0` in the fleet settings to leave them alone.

