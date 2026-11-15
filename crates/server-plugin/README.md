# server-plugin

`hallow-server`, Hallow's Pumpkin plugin (`hallow_server.wasm`). It links each lobby and game server to the [control plane](../control-plane/README.md), so proxies can route players to it.

- **Register.** About a second after the server starts, it registers with `POST /v1/servers`: name, kind, version, role, address and player limit.
- **Heartbeat.** Every 5 s it sends the player count and state to `POST /v1/servers/{name}/heartbeat`.
  - If the control plane answers `404` (the server expired, or Redis was reset), it registers again.
  - A state change is sent within a second or two, without waiting for the next heartbeat.
- **Signals.** Every 30 s it sends each player's movement and combat numbers to `POST /v1/signals`, for the anticheat (see [Anticheat signals](#anticheat-signals)).
- **Unregister.** On unload, which Pumpkin does on shutdown after kicking players, it sends `DELETE /v1/servers/{name}`. If that fails, the control plane drops the server after 15 s without heartbeats.

Presence and stats come later.

## Server state

The state starts as `ready`. Game code changes it over Pumpkin's plugin IPC by sending `hallow_server` the state name as the message:

| Message | Effect | Reply |
|---|---|---|
| `ready`, `in_game` or `draining` | Sets the state, reported within a second or two | The new state |
| empty | Nothing | The current state |

```rust
use pumpkin_plugin_api::wit::pumpkin::plugin::ipc::send_ipc_message;
send_ipc_message("hallow_server", b"in_game")??;
```

The Hallow SDK will wrap this. Within this crate, the seam is `state::set` in [`src/state.rs`](src/state.rs).

## Anticheat signals

Phase 1 of the [anticheat](../../docs/ANTICHEAT.md): the plugin measures, nothing acts on it. It listens to Pumpkin's player events, sums them per player over a 30 s window, and sends one `SignalBatch` per window with `source` set to the server name. Players who did nothing in a window get no sample. Batches that fail are logged and dropped, not retried; the window running at shutdown is dropped too.

| Metric | Event | Meaning |
|---|---|---|
| `moves` | `PlayerMoveEvent` | Position changes Pumpkin accepted. A client sends at most 20 a second while moving, so a higher rate hints at timer abuse |
| `horizontal_speed_max` | `PlayerMoveEvent` | Most horizontal distance covered in any one second of the window, blocks/s. Sprint-jumping is about 7; elytra, riptide, knockback and creative flight go well past that |
| `vertical_speed_max` | `PlayerMoveEvent` | Most upward distance in any one second, blocks/s. Falling doesn't count. A jump is about 1.25 |
| `swings` | `PlayerAnimationEvent` | Main-hand arm swings: left clicks and attacks, plus some item and block uses |
| `cps_max` | `PlayerAnimationEvent` | Most main-hand swings in any one second |
| `attacks` | `PlayerInteractEntityEvent` | Attacks on any entity |
| `attack_reach_max` | `PlayerInteractEntityEvent` | Longest attack on another player: the attacker's eyes to the nearest point of a standing player's hitbox, blocks. Only present when the player attacked a player |

Seconds are counted from the start of the window rather than between events, because Pumpkin handles packets in the tick and moves can arrive bunched after lag. `protocol` is left empty: behind Vine every client looks like the server's version, and Vine reports the real one.

What Pumpkin's plugin API (v0.1) can't give, and so isn't measured here:

- **Rotation snaps.** The move event fires only when the position changes and has no yaw or pitch; look-only packets fire nothing. Vine measures rotation from packets.
- **Air time.** Neither the move event nor the player exposes whether the player is on the ground.
- **Reach on mobs.** There's no entity lookup by id and no hitbox size, so reach is only measured against players.
- **Reach beyond Pumpkin's limit.** Pumpkin drops attacks farther than the interaction range plus one block (4 in survival) before firing the event, so `attack_reach_max` never exceeds that.

The handlers run inside the tick, since that's where Pumpkin handles packets and fires these events. Each does a map update and one or two calls back into Pumpkin, nothing more.

## How it runs

The plugin targets Pumpkin's plugin API v0.1, the version `pumpkin-plugin-api` builds against.

- **Off the tick.** A repeating task runs every 20 ticks and heartbeats, or sends signals, when due. Pumpkin runs plugin tasks on Tokio tasks, not inside the tick. But it runs one call into a plugin at a time, and the signal handlers are called from the tick, so the task never waits on the network: it starts a request and picks up the answer on its next run. A slow control plane can't hold up the tick.
- **Timeouts.** Every HTTP call has a 2 s timeout (connect, first byte, between bytes), and the plugin gives up on one after 10 s regardless.
- **Failures.** A failed heartbeat is logged once and retried on the next heartbeat; failed signal batches are logged once and dropped. A missing or unreachable control plane never stops the server.
- **Unload.** `on_unload` waits for the `DELETE`, up to that 2 s timeout, before Pumpkin moves on with shutdown.

## Setup

The [server base image](../../templates/server/Dockerfile) already has the plugin in `plugins/`, with its permissions pre-approved in [`pumpkin.toml`](../../templates/server/pumpkin.toml). It does nothing until `HALLOW_CONTROL_PLANE` is set.

To build it yourself:

```sh
cargo build --release --target wasm32-wasip2 -p hallow-server
cp target/wasm32-wasip2/release/hallow_server.wasm <pumpkin>/plugins/
```

It asks for `http.outbound`, `fs.read.data`, and `sys.env.<NAME>` for each variable below. Pumpkin asks on the console unless `pumpkin.toml` pre-approves them:

```toml
[plugins.overrides.hallow_server]
allowed_permissions = ["http.outbound", "fs.read.data", "sys.env.HALLOW_CONTROL_PLANE", ...]
```

## Settings

Environment variables come first. Any setting a variable leaves unset comes from `plugins/data/hallow_server/config.toml`. A token file is read from the same folder.

| Variable | `config.toml` key | Default |
|---|---|---|
| `HALLOW_CONTROL_PLANE` | `control_plane` | none: the plugin stays idle. `host:port`, plain HTTP on the private network |
| `HALLOW_CONTROL_PLANE_TOKEN` | `token` | |
| `HALLOW_CONTROL_PLANE_TOKEN_FILE` | `token_file` | a file name in the data folder |
| `HALLOW_SERVER_NAME` | `name` | required; the base image sets it |
| `HALLOW_SERVER_KIND` | `kind` | the name |
| `HALLOW_SERVER_VERSION` | `version` | `dev` |
| `HALLOW_SERVER_ROLE` | `role` | `game` (or `lobby`) |
| `HALLOW_SERVER_ADDRESS` | `address` | `<name>:25565` |
| `HALLOW_MAX_PLAYERS` | `max_players` | `100` |

Lobbies that new players should land on need the kind the control plane uses for hubs (`HALLOW_HUB_KIND`, `hub` by default). The two-proxy dev network ([`deploy/compose/compose.hallow.yaml`](../../deploy/compose/compose.hallow.yaml)) configures `lobby` and `game-1` through `config.toml` files in `deploy/compose/config/hallow/`.

## Tests

```sh
cargo test -p hallow-server
```

They cover the config (environment, file, overrides and errors), the IPC state messages, and the signal windows and metrics.
