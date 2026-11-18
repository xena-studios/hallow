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

[`templates/server-repo`](../templates/server-repo) is the starting point (it becomes the `hallow-server-template` repo), and [`servers/hub`](../servers/hub), [`servers/test-game-lobby`](../servers/test-game-lobby) and [`servers/test-game`](../servers/test-game) are in-tree examples the dev network runs ([servers](../servers/README.md)).

## hallow.toml

`hallow.toml` says what the server is and how Hallow should treat it:

```toml
name = "skywars"
role = "game"                # "hub", "lobby" or "game"
lobby = "skywars-lobby"      # where players go when a game ends
base = "^0.2"                # Hallow base image versions this repo works with
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
| `scaling.warm` | no | Games: idle instances kept ready while players are online. Hubs and lobbies: the fewest instances kept up; more start as players fill them ([Hubs and lobbies](#hubs-and-lobbies)). Default 1 for a hub (the network's floor, Decisions 19), else 0 |
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

Variables the container already sets win, so the control plane sets what differs per instance: `HALLOW_SERVER_NAME`, `HALLOW_SERVER_ADDRESS`, `HALLOW_CONTROL_PLANE` and its token, and on Wings `HALLOW_SERVER_PORT` and `HALLOW_SERVER_DIR` (see [Instances](INSTANCES.md)). A repo's image also sets `HALLOW_SERVER_VERSION` from its git tag at build time. The control plane reads `role`, `lobby`, `players` and `scaling` when the kind is registered ([Instances](INSTANCES.md#kinds)). The server plugin sends players to `HALLOW_RETURN_LOBBY` when a game ends ([Hubs and lobbies](#hubs-and-lobbies)). `queues`, `players.min` and `maps` are for the matchmaker and `hallow-sdk`, which don't read them yet.

## Hubs and lobbies

Players land in the **hub** (`role = "hub"`), pick a **game mode**, and go to that mode's **lobby** (`role = "lobby"`, such as `skywars-lobby`), where they queue for its **games**. A game mode is a lobby kind; its name is the kind without `-lobby` (`skywars`). Hubs and lobbies are server kinds like any other, so the control plane runs them as instances: at least `scaling.warm` of each, and more as players fill them past 80 % on average ([Instances](INSTANCES.md#lifecycle)).

**Getting there.** Everything goes through the control plane's `POST /v1/lobbies/send`, which picks the least full ready instance of the lobby kind and has the player's proxy move them, so a player never lands on an instance that is ending:

| From | How |
|---|---|
| Anywhere | `/lobby <mode>` (Vine's command, so it works on every server), by mode name (`skywars`), lobby kind (`skywars-lobby`) or a game kind whose `lobby` names one |
| Anywhere | `/hub`, or `/lobby` with no mode: back to a hub |
| The hub | The **Game Menu** compass in the hotbar: right-click for a chest listing every mode with how many are playing, click one to go. The welcome message lists the modes as clickable text that runs `/lobby <mode>` |
| A finished game | Its server goes `draining`; Hallow's server plugin sends everyone to `HALLOW_RETURN_LOBBY` (`hallow.toml`'s `lobby`), or to a hub if none of those is up. The control plane ends the instance once it is empty |
| An instance ending early | Staff ended it, or it timed out: the control plane sends anyone still on it to its kind's `lobby` (or a hub), waits up to 5 s, then stops it |

`GET /v1/lobbies` lists the modes with their players (in the mode's lobbies and its games) and ready lobby instances, plus the network's player count. The modes are the lobby kinds of registered servers and of the kinds registry, so a mode shows while its first instance starts.

**The hub's plugin.** Hub-only gameplay lives in the hub's own WASM plugin ([`servers/hub/plugin`](../servers/hub/plugin), listed in its `hallow.toml`), not in Hallow's server plugin, because it is the hub repo's business: when the hub moves to its own repo the plugin goes with it, the way a game's plugin lives in the game's repo. What every server needs (the control plane link, the mode list, moving players, the way back from a game) stays in Hallow's server plugin, which the hub's plugin asks over IPC (`lobbies`, `send <uuid> <mode>`), so the hub's plugin needs no network access or token. The hub plugin gives:

- the game menu and the clickable mode list;
- a title on join with the network's player count, and a tab list header and footer with the count and the instance's name;
- spawn at the world spawn in adventure mode (creative and spectator staff keep theirs), no damage or hunger, the menu item fixed in place, and back to spawn after a fall out of the world.

Pumpkin's plugin API (WIT v0.1) has what this needs: GUIs (`gui` with `generic-9x3` chests), item stacks with names and lore, inventory click and close events, interact events, titles, tab list header and footer, clickable text, and cancellable damage, hunger, drop and swap-hand events. One gap: there is no call to close a player's open chest, so the menu stays open until the proxy moves the player.

## Stats and game results

Every server counts play time and joins for its kind. A game adds its own numbers in one of two ways, both over IPC to Hallow's server plugin, so a game plugin needs no network access or token ([server plugin](../crates/server-plugin/README.md#game-results)):

| Message | When | Counts |
|---|---|---|
| `stat <uuid> <name> [amount]` | Any time | Adds to the player's total for this kind |
| `game_result <json>` | Once, when a game ends, before `draining` | `games` +1 for everyone, `wins` +1 for winners, and each player's stats; kept for players' recent games |

Count a stat one way or the other, not both. `hallow-sdk` will wrap these. The types are in `hallow_proto::stats` (`GameReport`, `GamePlayer`).

Players see the numbers with `/stats` and `/leaderboard` on any server (all time, this week and this month), and on the [player website](../web/README.md). Rank prefixes from the panel show in chat and the tab list on every server.

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

Hallow's server base image, `ghcr.io/xena-studios/hallow-server-base`, carries Pumpkin, Hallow's own server plugin (presence, stats, game results, rank prefixes, the link to the control plane) and `hallow-manifest`, so a server repo only holds what's specific to it. Its Dockerfile is [`templates/server`](../templates/server), and its version is in [`templates/server/VERSION`](../templates/server/VERSION):

- a `server-base-vX.Y.Z` tag on the monorepo publishes `X.Y.Z` and `X.Y` ([workflow](../.github/workflows/server-base.yml));
- pushes to main that touch it publish `dev` and `sha-<commit>`.

A server repo's Dockerfile is `FROM ghcr.io/xena-studios/hallow-server-base:0.2` plus `COPY` lines for its `hallow.toml`, plugins and world; the base tag must sit inside the repo's `base` range. The in-tree servers build on the local base image instead (see [Dev network](../deploy/compose/README.md)).

## One match per instance

A game server runs one match and then stops. Resetting a map means starting a fresh container from the image, so no state leaks between matches, a crash takes down one match, and the plugin never has to undo the world. Pumpkin started in about a second in our first tests, which is what makes this affordable; Phase 0 measures it properly. Several arenas per server can come later for very small games if the numbers say so.

## Lifecycle

The control plane owns every proxy and server instance. It creates them through Wings, registers servers with every Vine, routes players by role and repo, and deletes them when they're done or no longer needed ([Instances](INSTANCES.md)). The panel shows all of them live, with consoles and logs through Wings. Only the platform services on the floor node (PostgreSQL, Redis, control plane, panel, load balancer) run outside this, under Docker Compose.

## The SDK

The Hallow monorepo publishes `hallow-sdk`, a Rust crate that server repos depend on. It holds the minigame library (arenas, teams, kits, scoreboards, spectating, stats) and the client for the control plane. A template repo, `hallow-server-template` (the contents of [`templates/server-repo`](../templates/server-repo)), is the starting point for a new server kind.
