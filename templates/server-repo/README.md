# example

A Hallow server kind. This repo is made from `hallow-server-template`: rename it, then change `name` in `hallow.toml` and the image name in the `docker build` line below.

| File | What it is |
|---|---|
| `hallow.toml` | What this server is and how Hallow runs it: kind name, role, return lobby, players, scaling, base image range, plugins, world, Pumpkin settings and environment. The format is in Hallow's [Servers](https://github.com/xena-studios/hallow/blob/main/docs/SERVERS.md#hallowtoml) doc |
| `Dockerfile` | `FROM` Hallow's server base image, plus `hallow.toml`, `plugins/` and (when you add one) `world/` |
| `plugins/` | The game's built `.wasm` plugins. List each in `hallow.toml` under `[[plugins]]` |
| `.github/workflows/image.yml` | Calls Hallow's reusable image workflow |

## Build and run

```sh
docker build -t example:dev .
docker run --rm -e HALLOW_VINE_SECRET=dev -e HALLOW_SERVER_NAME=example-1 -p 25565:25565 example:dev
```

The image build runs `hallow-manifest check`, so an invalid `hallow.toml` or a base tag outside its `base` range fails it. The base image's entrypoint applies `hallow.toml` and starts Pumpkin; it sits behind a Vine proxy (Vine forwarding is on), so join through one.

## Releases

CI publishes `ghcr.io/<owner>/<name>`:

- a `vX.Y.Z` tag publishes `X.Y.Z`, a version the panel can make live;
- a push to `main` publishes `sha-<commit>` and `dev`, for staging only;
- pull requests build and check without publishing.

To move to a new base image, bump the `FROM` tag and `base` together.
