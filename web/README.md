# web

Hallow's player website: profiles, stats and leaderboards. It reads the control plane's public API (`/public/v1`, read-only, no token, rate-limited per client) and renders every page on the server.

## Pages

| Path | What it shows |
|---|---|
| `/` | Players online, total players and games played, a search box, and the top 5 of each game's headline stat (wins, else kills, else games) |
| `/search?q=` | Players whose names start with `q`. An exact match goes straight to the profile |
| `/player/{name}` | Avatar, name with rank prefix in Minecraft colours, edition, online, first and last seen, total play time, a card per game with its stats (plus K/D and win rate), and the last 10 games |
| `/leaderboards` | Every game and its stats |
| `/leaderboards/{kind}/{stat}?window=` | The top 100 for one stat: all time, this month or this week |
| `/healthz` | `204`, for the container health check |

A player that never joined gets a 404 page. If the API is down the site says so (`503`); if a visitor is over the API's rate it shows a "slow down" page (`429`).

## Why it's built this way

The site is a small Rust server ([axum](https://docs.rs/axum) and [maud](https://maud.lambda.xyz) templates), not a JavaScript app.

- **One language.** The rest of Hallow is Rust, so the same people can work on it with the same tools.
- **It can't drift from the API.** It deserializes the API's own types from [`hallow-proto`](../crates/proto/src/public.rs). Change a field there and the site stops compiling until it's updated.
- **Nothing in the browser.** The browser only talks to the site. The site calls the API itself and passes the visitor's address on in `X-Forwarded-For`, so the rate limit still applies per visitor.
- **Fast on phones.** Pages are plain HTML and one cached stylesheet, with no JavaScript. They work with JavaScript off and on slow connections.
- **One small container.** One binary with the stylesheet built in. No Node, no build step, no CDN.
- **The API stays open.** It is public and rate-limited on purpose, so anyone can build their own stats page, bot or overlay on it.

It keeps API answers for 15 seconds, so a busy page doesn't hit the API on every view. Player heads come from [mc-heads.net](https://mc-heads.net); Bedrock players, or a head that fails to load, get their first letter instead.

## Settings

| Variable | Default | What it does |
|---|---|---|
| `HALLOW_PUBLIC_API` | required | The control plane's address, such as `http://control-plane:8080` |
| `HALLOW_WEB_LISTEN` | `0.0.0.0:8080` | Where the site listens |
| `HALLOW_WEB_NETWORK_NAME` | `Hallow` | The network's name in titles and on the home page |
| `RUST_LOG` | `info` | Log level |

## Run it locally

With the dev network up (the control plane on `127.0.0.1:18090`):

```sh
HALLOW_PUBLIC_API=http://127.0.0.1:18090 HALLOW_WEB_LISTEN=127.0.0.1:8080 cargo run -p hallow-web
```

Then open <http://127.0.0.1:8080>. Tests render each page from fixture data:

```sh
cargo test -p hallow-web
```

The code is in three parts: [`src/api.rs`](src/api.rs) calls the API and caches, [`src/views.rs`](src/views.rs) turns data into pages (pure functions, so tests don't need a server), and [`src/fmt.rs`](src/fmt.rs) formats durations, times, stat names and colour codes. The stylesheet is [`src/style.css`](src/style.css): dark by default, light when the system asks for it.

## In the dev network

The two-proxy override runs the site as `web`, next to the control plane:

```sh
docker compose -f deploy/compose/compose.yaml -f deploy/compose/compose.hallow.yaml up --build -d
```

It's on <http://localhost:18095> (`HALLOW_WEB_PORT` to change it). The image builds from the repo root with [`Dockerfile`](Dockerfile), or with `docker buildx bake web`.
