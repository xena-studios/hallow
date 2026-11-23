# crates

Hallow's Cargo workspace.

| Crate | What it is |
|---|---|
| `proto/` | `hallow-proto`: HTTP/JSON API types shared by the control plane, every plugin and the player website; builds for the host and `wasm32-wasip2`. The endpoint table is in `src/routes.rs`, the public API's in `src/public.rs` |
| `control-plane/` | Server and proxy registry, presence and cross-proxy commands over HTTP/JSON, state in Redis; later the matchmaker, orchestration through the Wings API and the fleet manager. See its [README](control-plane/README.md) |
| `db/` | `hallow-db`: PostgreSQL schema in its own `hallow` schema (accounts, ranks, sessions, punishments, stats, games), migrations, and the queries the control plane runs at login. See its [README](db/README.md) |
| `fleet/` | `hallow-fleet`: the fleet manager's provider trait, its scaling planner (a pure function from fleet state and policy to buy, drain, cancel and hold decisions) and dry run; not yet wired into the control plane. See its [README](fleet/README.md) |
| `providers/hetzner-cloud/` | `hallow-provider-hetzner-cloud`: hourly burst nodes through the Hetzner Cloud API, limited to servers labelled `hallow=node`. See its [README](providers/hetzner-cloud/README.md) |
| `providers/*` | One crate per machine provider behind `hallow-fleet`'s provider trait; Hetzner Robot and OVHcloud later |
| `vine-plugin/` | `hallow-vine`: presence, global ping, duplicate-login check, server-list sync and cross-proxy commands, linking each proxy to the control plane (WASM); parties and queues later. See its [README](vine-plugin/README.md) |
| `hallow-sdk/` | For server repos: minigame library (arenas, teams, kits, scoreboards, spectating, stats) and control-plane client (WASM) |
| `server-plugin/` | `hallow-server`: Hallow's Pumpkin plugin in every server image. Registers the server with the control plane, heartbeats its state and players, reports joins and leaves, sends player stats, game results and anticheat signals, shows rank prefixes in chat and the tab list, and unregisters it on shutdown (WASM). See its [README](server-plugin/README.md) |
| `server-manifest/` | `hallow-manifest`: parses and validates a server repo's `hallow.toml`, and the `hallow-manifest` CLI that checks it in CI and applies it in the server base image. See its [README](server-manifest/README.md) |
| `netcheck/` | Smoke test and load test: joins through the proxy as a bot and switches servers; `--stay` keeps it connected; `--bots N` runs N wandering bots and reports join times |
| `tickwatch/` | Pumpkin plugin that logs tick time, TPS and players once a second, for load tests |
| `vineprobe/` | Vine plugin for tests: commands that register and unregister servers, drain the proxy, and make a `wasi:http` request (to check plugin permissions) |
| `apiprobe/` | Pumpkin plugin that checks the plugin API calls every game needs; see [Validation](../docs/VALIDATION.md#plugin-api-findings) |

Games and lobbies are not crates here; each is its own server repo (see [Servers](../docs/SERVERS.md)).
