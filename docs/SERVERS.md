# Servers

Hallow runs two kinds of process: **proxies** and **servers**. Proxies are Vine, all identical, scaled out behind the load balancer. Servers are Pumpkin, and every lobby, game and hub is one. What makes a SkyWars game different from the main hub is the **server repo** it was built from.

## Server repos

Each kind of server lives in its own git repo, for example:

| Repo | Role | What it is |
|---|---|---|
| `hub` | hub | The main hub every player lands in |
| `skywars-lobby` | lobby | Where SkyWars players queue, see stats and pick a mode |
| `skywars` | game | One SkyWars match per instance |
| `bedwars-lobby` | lobby | BedWars queueing and cosmetics |
| `bedwars` | game | One BedWars match per instance |

A server repo holds everything that kind of server needs:

```text
skywars/
├── hallow.toml          # what this server is and how it scales
├── Dockerfile           # FROM the server base image, plus the files below
├── plugin/              # the game's Pumpkin plugin, a Rust crate built to WASM
├── plugins/             # the built .wasm files, copied into the image
├── world/               # the server's world, tracked with Git LFS
├── maps/                # map worlds for hallow-sdk, tracked with Git LFS
└── .github/workflows/   # calls Hallow's reusable image workflow
```

[`templates/server-repo`](../templates/server-repo) is the starting point (it becomes the `hallow-server-template` repo), and [`servers/hub`](../servers/hub) and [`servers/test-game`](../servers/test-game) are in-tree examples the dev network runs.

## hallow.toml

`hallow.toml` says what the server is and how Hallow should treat it:

```toml
name = "skywars"
role = "game"                # "hub", "lobby" or "game"
lobby = "skywars-lobby"      # where players go when a game ends
base = "^0.1"                # Hallow base image versions this repo works with
world = "world"              # Pumpkin's world folder; generated if unset

[players]
min = 2                      # start when this many are queued
max = 12                     # one match per instance

[scaling]
warm = 1                     # idle instances kept ready while players are online
max_instances = 200

[maps]
dir = "maps"
pick = "random"              # or "vote"

[[plugins]]
file = "plugins/skywars.wasm"
permissions = ["fs.read.data"]

[pumpkin]                    # Pumpkin settings that differ from Hallow's defaults
default_gamemode = "Adventure"

[env]
SKYWARS_MODE = "solo"
```

A lobby's file has `role = "lobby"`, a `players.max` per instance, and the games it queues for (`queues = ["skywars"]`). The hub's has `role = "hub"`.

| Key | Required | What it does |
|---|---|---|
| `name` | yes | The server kind: the image name (`ghcr.io/xena-studios/<name>`) and the kind the server registers with. Up to 40 lowercase letters, digits and single dashes, starting with a letter |
| `role` | yes | `hub`: where players land after login; one hub kind per network, found by name (the control plane's `HALLOW_HUB_KIND`, default `hub`), registered as a lobby. `lobby`: a lobby players move to, with its queues. `game`: one match per instance |
| `lobby` | games | The kind players return to when a game ends. A hub returns players to itself (leave it out or name the hub); a lobby may name one |
| `base` | yes | The base image versions the repo works with, as a semver range (`^0.1`, `>=0.1, <0.3`). The base image refuses to start a server outside it, and image builds check it |
| `world` | no | Pumpkin's world folder, relative to `hallow.toml` (Pumpkin's `default_level_name`). Pumpkin generates a world when unset |
| `queues` | no | Lobbies and hubs: the game kinds they queue players for |
| `players.max` | yes | Players per instance (Pumpkin's `networking.java.max_players`, and the capacity the control plane sees) |
| `players.min` | no | Games: players needed to start a match. Default 1 |
| `scaling.warm` | no | Idle instances kept ready while players are online. Default 1 for a hub (the network's floor, Decisions 19), else 0 |
| `scaling.max_instances` | no | The most instances of the kind at once |
| `maps.dir`, `maps.pick` | no | Games: the folder of map worlds and how one is picked (`random` or `vote`), read by `hallow-sdk` |
| `[[plugins]]` | no | The repo's WASM plugins: `file` (a `.wasm` path), `name` (Pumpkin's plugin name, default the file name without `.wasm`) and `permissions`, pre-approved so Pumpkin doesn't ask on the console. `hallow_server` and the multiversion plugin come with the base image |
| `[pumpkin]` | no | Merged over the base image's [`pumpkin.toml`](../templates/server/pumpkin.toml). The keys that would cut the server off the network (`networking.proxy`, `networking.java.address`, `networking.java.online_mode`), telemetry and update checks (Decisions 24), and Hallow's plugin's overrides are fixed; `max_players` and `default_level_name` come from `players.max` and `world` |
| `[env]` | no | Environment variables for the server, as strings. `HALLOW_*` names are reserved. The container's own environment wins |

Paths are relative to `hallow.toml` and stay inside the repo. Unknown keys are errors, so a typo fails the build instead of being ignored.

`hallow-manifest`, from [`crates/server-manifest`](../crates/server-manifest), checks a file and the files it names: `hallow-manifest check [path] [--base-version 0.1.0]`. It runs in every server image build and in the image workflow, and ships in the base image.

### What the base image does with it

The base image's entrypoint runs `hallow-manifest apply` on every start. It:

1. checks `base` against the image's own version (`/etc/hallow/base-version`);
2. merges `[pumpkin]`, `players.max`, `world` and the plugins' permissions into `pumpkin.toml`;
3. copies the plugins into Pumpkin's `plugins/` folder;
4. sets the environment Hallow's server plugin reads:
   - `HALLOW_SERVER_KIND`: from `name`;
   - `HALLOW_SERVER_ROLE`: `lobby` for a hub or lobby, `game` for a game;
   - `HALLOW_MAX_PLAYERS`: from `players.max`;
   - `HALLOW_RETURN_LOBBY`: from `lobby`;
   - then `[env]`.

Variables the container already sets win, so the control plane sets what differs per instance: `HALLOW_SERVER_NAME`, `HALLOW_SERVER_ADDRESS`, `HALLOW_CONTROL_PLANE` and its token, and on Wings `HALLOW_SERVER_PORT` and `HALLOW_SERVER_DIR` (see [Instances](INSTANCES.md)). A repo's image also sets `HALLOW_SERVER_VERSION` from its git tag at build time. The control plane reads `role`, `lobby`, `players` and `scaling` when the kind is registered ([Instances](INSTANCES.md#kinds)). The plugin doesn't read `HALLOW_RETURN_LOBBY` yet, and `queues`, `players.min` and `maps` are for the matchmaker and `hallow-sdk`, which don't read them yet either.

## From repo to running server

1. A push to a server repo's main branch, or a tag, runs its CI: Hallow's reusable [image workflow](../.github/workflows/server-image.yml). It builds the plugin to WASM (the repo's `prepare` step), checks `hallow.toml` with the base image's `hallow-manifest`, bakes the plugin, worlds and `hallow.toml` onto Hallow's server base image, then publishes the image to GHCR as `ghcr.io/xena-studios/<name>:<version>`.
2. The panel lists every server kind and its versions. Staff choose which version is live; that's the deploy. See [Versions](#versions).
3. New instances start on the live version. Running games finish on the version they started on, and old lobby instances drain once new ones are up. No restarts and no kicked players.
4. Rolling back means making the previous version live again.

## Versions

Every server kind is versioned by its own repo, independently of the others and of Hallow.

- **A version is a git tag** on the server repo, `v1.4.0` style. CI turns the tag into the image `ghcr.io/xena-studios/skywars:1.4.0`, labelled with the commit, the base image version and the `hallow-sdk` version it was built with.
- **Pushes to `main` are dev builds**, tagged with the commit (`skywars:sha-3f2a9c1`). They can be started on a staging network but never go live on production.
- **Channels per server kind:** `live` is what new instances start on, `canary` optionally takes a share of new instances (say 10 %) before it becomes `live`. Promoting, canarying and rolling back are panel actions, each in the audit log with who did it.
- **Compatibility is declared, not guessed:** `hallow.toml` names the base image range it supports (`base = "^0.3"`). The panel refuses to make a version live on a network whose base image is outside that range, so a Pumpkin or protocol upgrade can't silently break an old game.
- **Mixed versions are normal.** During a rollout, old and new instances of the same kind run side by side: running matches finish on their version, new matches start on the new one, and lobbies drain old instances once new ones are up. The panel shows which version every instance runs.
- **Hallow itself** (base image, SDK, control plane, Vine fork) is versioned the same way from tags on the monorepo; a base image release says which Pumpkin commit and protocol version it carries.

## The base image

Hallow's server base image, `ghcr.io/xena-studios/hallow-server-base`, carries Pumpkin, the multiversion plugin (off by default, Decisions 28), Hallow's own server plugin (presence, stats, the link to the control plane) and `hallow-manifest`, so a server repo only holds what's specific to it. Its Dockerfile is [`templates/server`](../templates/server), and its version is in [`templates/server/VERSION`](../templates/server/VERSION):

- a `server-base-vX.Y.Z` tag on the monorepo publishes `X.Y.Z` and `X.Y` ([workflow](../.github/workflows/server-base.yml));
- pushes to main that touch it publish `dev` and `sha-<commit>`.

A server repo's Dockerfile is `FROM ghcr.io/xena-studios/hallow-server-base:0.1` plus `COPY` lines for its `hallow.toml`, plugins and world; the base tag must sit inside the repo's `base` range. No base version has been published yet: the first `server-base-v0.1.0` tag does that. Until then, the in-tree servers build on the local base image (see [Dev network](../deploy/compose/README.md)).

## One match per instance

A game server runs one match and then stops. Resetting a map means starting a fresh container from the image, so no state leaks between matches, a crash takes down one match, and the plugin never has to undo the world. Pumpkin started in about a second in our first tests, which is what makes this affordable; Phase 0 measures it properly. Several arenas per server can come later for very small games if the numbers say so.

## Lifecycle

The control plane owns every proxy and server instance. It creates them through Wings, registers servers with every Vine, routes players by role and repo, and deletes them when they're done or no longer needed ([Instances](INSTANCES.md)). The panel shows all of them live, with consoles and logs through Wings. Only the platform services on the floor node (PostgreSQL, Redis, control plane, panel, load balancer) run outside this, under Docker Compose.

## The SDK

The Hallow monorepo publishes `hallow-sdk`, a Rust crate that server repos depend on. It holds the minigame library (arenas, teams, kits, scoreboards, spectating, stats) and the client for the control plane. A template repo, `hallow-server-template` (the contents of [`templates/server-repo`](../templates/server-repo)), is the starting point for a new server kind.
