# crates

Hallow's Cargo workspace.

| Crate | What it is |
|---|---|
| `proto/` | gRPC definitions shared by the control plane and every plugin |
| `control-plane/` | Server and proxy registry, matchmaker, presence, orchestration through the Wings API, fleet manager |
| `providers/*` | One crate per machine provider (Hetzner Cloud, Hetzner Robot, OVHcloud) behind the fleet manager's provider trait |
| `vine-plugins/` | Presence, global ping, duplicate-login check, parties, queues, cross-proxy commands (WASM) |
| `hallow-sdk/` | For server repos: minigame library (arenas, teams, kits, scoreboards, spectating, stats) and control-plane client (WASM) |
| `server-plugin/` | Hallow's plugin in every server image: presence, stats, the link to the control plane (WASM) |
| `netcheck/` | Smoke test: joins through the proxy as a bot, switches servers, and can stay connected (`--stay`) while plugins act on it |
| `apiprobe/` | Pumpkin plugin that checks the plugin API calls every game needs; see [Validation](../docs/VALIDATION.md#plugin-api-findings) |

Games and lobbies are not crates here; each is its own server repo (see [Servers](../docs/SERVERS.md)).
