# crates

Hallow's Cargo workspace. Crates arrive in Phase 1.

| Crate | What it is |
|---|---|
| `proto/` | gRPC definitions shared by the control plane and every plugin |
| `control-plane/` | Server and proxy registry, matchmaker, presence, orchestration through the Wings API |
| `vine-plugins/` | Presence, global ping, duplicate-login check, parties, queues, cross-proxy commands (WASM) |
| `minigame-lib/` | Arenas, teams, kits, scoreboards, map reset, stats (WASM) |
| `games/*` | One crate per game (WASM) |
