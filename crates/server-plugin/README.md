# server-plugin

`hallow-server`, Hallow's Pumpkin plugin (`hallow_server.wasm`). It links each lobby and game server to the [control plane](../control-plane/README.md), so proxies can route players to it.

- **Register.** About a second after the server starts, it registers with `POST /v1/servers`: name, kind, version, role, address and player limit.
- **Heartbeat.** Every 5 s it sends the player count and state to `POST /v1/servers/{name}/heartbeat`.
  - If the control plane answers `404` (the server expired, or Redis was reset), it registers again.
  - A state change is sent within a second, without waiting for the next heartbeat.
- **Unregister.** On unload, which Pumpkin does on shutdown after kicking players, it sends `DELETE /v1/servers/{name}`. If that fails, the control plane drops the server after 15 s without heartbeats.

Presence and stats come later.

## Server state

The state starts as `ready`. Game code changes it over Pumpkin's plugin IPC by sending `hallow_server` the state name as the message:

| Message | Effect | Reply |
|---|---|---|
| `ready`, `in_game` or `draining` | Sets the state, reported within a second | The new state |
| empty | Nothing | The current state |

```rust
use pumpkin_plugin_api::wit::pumpkin::plugin::ipc::send_ipc_message;
send_ipc_message("hallow_server", b"in_game")??;
```

The Hallow SDK will wrap this. Within this crate, the seam is `state::set` in [`src/state.rs`](src/state.rs).

## How it runs

The plugin targets Pumpkin's plugin API v0.1, the version `pumpkin-plugin-api` builds against.

- **Off the tick.** A repeating task runs every 20 ticks and heartbeats when one is due. Pumpkin runs plugin tasks on Tokio tasks (`server.spawn_task`), not inside the tick, and its `wasi:http` is async on the host. So a slow control plane holds up only this plugin, never the tick.
- **Timeouts.** Every HTTP call has a 2 s timeout (connect, first byte, between bytes).
- **Failures.** A failed call is logged once and retried on the next heartbeat. A missing or unreachable control plane never stops the server.
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

They cover the config (environment, file, overrides and errors) and the IPC state messages.
