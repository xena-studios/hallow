# Dev network

One proxy and two servers on one machine, built from the pinned submodules.

```sh
git submodule update --init
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

Older Java clients need the multiversion plugin: `HALLOW_MULTIVERSION=1 docker compose ... up`. It deadlocks the server at about 50 moving players today, so keep it to small tests; see [Validation](../../docs/VALIDATION.md#load-test).

To play, connect a 26.3 client to `localhost:25565` and use `/server game-1` or `/server lobby`. To try a change to a server, edit its `hallow.toml` and run `up --build` again. The proxy runs in offline mode, so any username works.

| Service | Image | What it is |
|---|---|---|
| `proxy` | `hallow-proxy:dev` from `templates/proxy` | Vine from our fork, config in `config/vine.toml` |
| `lobby` | `hallow-hub:dev` from [`servers/hub`](../../servers/hub) | The hub, built the way a server repo builds: `FROM` the server base image plus its `hallow.toml` |
| `game-1` | `hallow-test-game:dev` from [`servers/test-game`](../../servers/test-game) | A test game, built the same way |
| `server-base` | `hallow-server-base:dev` from `templates/server` | Build only (scale 0): the server base image, with Pumpkin, the multiversion plugin available but off, the `hallow_server` plugin loaded but idle (no control plane here) and `hallow-manifest` |

The servers' Dockerfiles start `FROM ghcr.io/xena-studios/hallow-server-base:0.1`; compose passes a build context of that name pointing at the `server-base` service, so they build on the base image from this tree instead of pulling one. On start, the base image's entrypoint applies each server's `hallow.toml` (kind, role, max players, Pumpkin settings); `HALLOW_SERVER_NAME` sets the instance name. The forwarding secret is a dev default; set `HALLOW_VINE_SECRET` and change it in `config/vine.toml` for anything reachable from outside.

The first build compiles Pumpkin and Vine for Linux and takes a while; later builds reuse the cargo cache.
