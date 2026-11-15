# crates

Hallow's Cargo workspace.

| Crate | What it is |
|---|---|
| `proto/` | `hallow-proto`: HTTP/JSON API types shared by the control plane and every plugin; builds for the host and `wasm32-wasip2`. The endpoint table is in `src/routes.rs` |
| `control-plane/` | Server and proxy registry, presence and cross-proxy commands over HTTP/JSON, state in Redis; later the matchmaker, orchestration through the Wings API and the fleet manager. See its [README](control-plane/README.md) |
| `db/` | `hallow-db`: PostgreSQL schema in its own `hallow` schema (accounts, ranks, sessions, punishments), migrations, and the queries the control plane runs at login. See its [README](db/README.md) |
| `providers/*` | One crate per machine provider (Hetzner Cloud, Hetzner Robot, OVHcloud) behind the fleet manager's provider trait |
| `vine-plugin/` | `hallow-vine`: presence, global ping, duplicate-login check, server-list sync and cross-proxy commands, linking each proxy to the control plane (WASM); parties and queues later. See its [README](vine-plugin/README.md) |
| `hallow-sdk/` | For server repos: minigame library (arenas, teams, kits, scoreboards, spectating, stats) and control-plane client (WASM) |
| `server-plugin/` | Hallow's plugin in every server image: presence, stats, the link to the control plane (WASM) |
| `netcheck/` | Smoke test and load test: joins through the proxy as a bot and switches servers; `--stay` keeps it connected; `--bots N` runs N wandering bots and reports join times |
| `tickwatch/` | Pumpkin plugin that logs tick time, TPS and players once a second, for load tests |
| `vineprobe/` | Vine plugin for tests: commands that register and unregister servers, drain the proxy, and make a `wasi:http` request (to check plugin permissions) |
| `apiprobe/` | Pumpkin plugin that checks the plugin API calls every game needs; see [Validation](../docs/VALIDATION.md#plugin-api-findings) |

Games and lobbies are not crates here; each is its own server repo (see [Servers](../docs/SERVERS.md)).
