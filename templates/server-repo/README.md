# example

A Hallow server kind. This repo is made from `hallow-server-template`: rename it, then change `name` in `hallow.toml`, the image name in the `docker build` line below, and the plugin's name (`plugin/Cargo.toml`, `NAME` in `plugin/src/lib.rs`, the `.wasm` file in `hallow.toml` and the workflow).

| File | What it is |
|---|---|
| `hallow.toml` | What this server is and how Hallow runs it: kind name, role, return lobby, players, scaling, base image range, plugins, world, Pumpkin settings and environment. The format is in Hallow's [Servers](https://github.com/xena-studios/hallow/blob/main/docs/SERVERS.md#hallowtoml) doc |
| `Dockerfile` | `FROM` Hallow's server base image, plus `hallow.toml`, `plugins/` and (when you add one) `world/` |
| `plugin/` | The game's Pumpkin plugin, a Rust crate on [`hallow-sdk`](https://github.com/xena-studios/hallow/blob/main/crates/hallow-sdk/README.md), which runs the match, teams, kits, sidebar, spectating and stats. A lobby or hub with no gameplay of its own can delete it, with its `[[plugins]]` entry and the workflow's `prepare` |
| `plugins/` | The game's built `.wasm` plugins. List each in `hallow.toml` under `[[plugins]]` |
| `maps/` | Optional: one folder per map, a Pumpkin world plus its `map.toml` (spawns, spectator point, void height), tracked with Git LFS. Name it in `hallow.toml`'s `[maps]` and copy it in the Dockerfile |
| `.github/workflows/image.yml` | Calls Hallow's reusable image workflow |

## Build and run

```sh
(cd plugin && cargo build --release --target wasm32-wasip2 \
  && cp target/wasm32-wasip2/release/example.wasm ../plugins/)
docker build -t example:dev .
docker run --rm -e HALLOW_VINE_SECRET=dev -e HALLOW_SERVER_NAME=example-1 -p 25565:25565 example:dev
```

The image build runs `hallow-manifest check`, so an invalid `hallow.toml` or a base tag outside its `base` range fails it. The base image's entrypoint applies `hallow.toml` and starts Pumpkin; it sits behind a Vine proxy (Vine forwarding is on), so join through one.

## Releases

CI publishes `ghcr.io/<owner>/<name>`:

- a `vX.Y.Z` tag publishes `X.Y.Z`, a version the panel can make live;
- a push to `main` publishes `sha-<commit>` and `dev`, for staging only;
- pull requests build and check without publishing.

To move to a new base image, bump the `FROM` tag, `base` and the `hallow-sdk` revision in `plugin/Cargo.toml` together.
