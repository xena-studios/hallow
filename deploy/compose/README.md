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

To play, connect a 26.3 client to `localhost:25565` and use `/server game-1` or `/server lobby`. The proxy runs in offline mode, so any username works.

| Service | Image | What it is |
|---|---|---|
| `proxy` | `hallow-proxy:dev` from `templates/proxy` | Vine from our fork, config in `config/vine.toml` |
| `lobby`, `game-1` | `hallow-server:dev` from `templates/server` | The server base image: Pumpkin with the multiversion plugin |

Lobbies and games run the same image; only `HALLOW_SERVER_NAME` differs. The forwarding secret is a dev default; set `HALLOW_VINE_SECRET` and change it in `config/vine.toml` for anything reachable from outside.

The first build compiles Pumpkin and Vine for Linux and takes a while; later builds reuse the cargo cache.
