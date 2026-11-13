# Servers

Hallow runs two kinds of process: **proxies** and **servers**. Proxies are Vine, all identical, scaled out behind the load balancer. Servers are Pumpkin, and every lobby, game and hub is one. What makes a SkyWars game different from the main hub is the **server repo** it was built from.

## Server repos

Each kind of server lives in its own git repo, for example:

| Repo | Role | What it is |
|---|---|---|
| `hub` | lobby | The main hub every player lands in |
| `skywars-lobby` | lobby | Where SkyWars players queue, see stats and pick a mode |
| `skywars` | game | One SkyWars match per instance |
| `bedwars-lobby` | lobby | BedWars queueing and cosmetics |
| `bedwars` | game | One BedWars match per instance |

A server repo holds everything that kind of server needs:

```text
skywars/
├── hallow.toml          # what this server is and how it scales
├── plugin/              # the game's Pumpkin plugin, a Rust crate built to WASM
├── maps/                # worlds, tracked with Git LFS
├── config/pumpkin.toml  # Pumpkin settings that differ from Hallow's defaults
└── .github/workflows/   # builds and publishes the server image
```

And `hallow.toml` says how Hallow should treat it:

```toml
name = "skywars"
role = "game"                # "lobby" or "game"
lobby = "skywars-lobby"      # where players go when a game ends

[players]
min = 2                      # start when this many are queued
max = 12                     # one match per instance

[scaling]
warm = 1                     # idle instances kept ready while players are online
max_instances = 200

[maps]
pick = "random"              # or "vote"
```

A lobby's file has `role = "lobby"`, a `players.max` per instance, and the games it queues for (`queues = ["skywars"]`).

## From repo to running server

1. A push to a server repo's main branch, or a tag, runs its CI. CI builds the plugin to WASM and bakes it, the maps and the config onto Hallow's Pumpkin base image, then publishes the image to GHCR as `ghcr.io/xena-studios/<name>:<version>`.
2. The panel lists every server kind and its versions. Staff choose which version is live; that's the deploy.
3. New instances start on the live version. Running games finish on the version they started on, and old lobby instances drain once new ones are up. No restarts and no kicked players.
4. Rolling back means choosing the previous version.

Hallow's base image carries Pumpkin, the multiversion plugin and Hallow's own server plugin (presence, stats, the link to the control plane), so a server repo only holds what's specific to it.

## One match per instance

A game server runs one match and then stops. Resetting a map means starting a fresh container from the image, so no state leaks between matches, a crash takes down one match, and the plugin never has to undo the world. Pumpkin started in about a second in our first tests, which is what makes this affordable; Phase 0 measures it properly. Several arenas per server can come later for very small games if the numbers say so.

## Lifecycle

The control plane owns every proxy and server instance. It creates them through Wings, registers servers with every Vine, routes players by role and repo, and deletes them when they're done or no longer needed. The panel shows all of them live, with consoles and logs through Wings. Only the platform services on the floor node (PostgreSQL, Redis, control plane, panel, load balancer) run outside this, under Docker Compose.

## The SDK

The Hallow monorepo publishes `hallow-sdk`, a Rust crate that server repos depend on. It holds the minigame library (arenas, teams, kits, scoreboards, spectating, stats) and the client for the control plane. A template repo, `hallow-server-template`, is the starting point for a new server kind.
