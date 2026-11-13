# Upstream

What each project Hallow builds on can do, from reading its source on 2026-11-13. Nothing here has been run yet; [Phase 0](ROADMAP.md#phase-0-validation-spike) runs it.

| Project | Licence | Age and activity | Does today | Gaps for Hallow |
|---|---|---|---|---|
| [Pumpkin](https://github.com/Pumpkin-MC/Pumpkin) | GPL-3.0 | 0.2.0 targets Java 26.3, Bedrock 26.51 | Java and Bedrock servers, WASM plugins, Velocity, BungeeCord and Vine forwarding | Pre-1.0; missing vanilla features; worlds can break between versions; small plugin ecosystem |
| [Vine](https://github.com/Pumpkin-MC/Vine) | GPL-3.0 | First commit 2026-09-07, 7 commits, last 2026-09-12, no README | Mojang auth, Ed25519-signed forwarding, server switching (config phase on 1.20.2+), `/send` `/server` `/glist`, PROXY protocol, WASM plugins | Server list fixed in config; no shared state between proxies; Bedrock listener answers pings only; no version translation |
| [pumpkin-java-multiversion](https://github.com/Pumpkin-MC/pumpkin-java-multiversion) | MIT or Apache-2.0 (`Cargo.toml`) | First commit 2026-09-20, 3 commits | Packet ID tables for 1.7.2 – 26.2; block, item, sound, entity remap tables from ViaVersion data | Rewrites packet bodies only for entity spawns; other packets get a new ID and the 26.3 body; chunk translators (`src/chunk/`) are excluded from the WASM build and never called |
| [Calagopus Panel](https://github.com/calagopus/panel) | MIT | v1.2.10, commits daily | Pterodactyl-style panel in Rust and React; backend, schema and frontend extension systems; Wings API client | Built for hosting, not players: Hallow adds the network features as extensions |
| [Calagopus Wings](https://github.com/calagopus/wings) | MIT | Active | Pterodactyl-compatible Wings in Rust: Docker containers from eggs, HTTP API | Speed of creating a server from a template, and behaviour with hundreds of short-lived servers per node, are unknown |
| [Geyser](https://github.com/GeyserMC/Geyser) | MIT | Mature | Bedrock to Java translation, standalone mode | Only if Bedrock route B is chosen |

## Details worth knowing

### Vine

- Plugin events: ping, pre-login, login, join, server-connect (cancellable, redirectable), server-connected, disconnect, chat, command, plugin message.
- Plugin calls: list players and servers, `connect-player`, `send-plugin-message`, broadcast, kick. No call adds or removes a server.
- Server switching resets the configuration phase only for 1.20.2+ clients (`src/backend/bridge.rs`). Switching for older clients through the multiversion plugin is untested.
- Messages Vine writes itself (chat, command suggestions) go straight to the client, not through a backend's translation.

### Builds

Vine and multiversion depend on Pumpkin crates by relative path (`../Pumpkin/crates/...`). Hallow keeps all of them as submodules side by side in `upstream/`, pinned to commits that build together.

### Rejected: Raptor

[Raptor](https://github.com/xena-studios/raptor), our own panel, is SaaS-only and its Wings runs no HTTP server, so a matchmaker can't drive it. It stays the product for people running their own servers. See [Decisions](DECISIONS.md).

## Contributions

Fixes and reports we have for upstream projects. Each is offered upstream before or alongside using it in a fork.

| Project | What | Kind | Status |
|---|---|---|---|
| Vine | Doesn't build against Pumpkin `main`, so Vine's own CI fails: `rsa` pre-release pin, a second `wasmtime` version, `StoreExecutor` API changes, and Pumpkin's removal of RakNet ([Pumpkin-MC/Pumpkin#4427](https://github.com/Pumpkin-MC/Pumpkin/pull/4427)) | Fix ready: [xena-studios/Vine `fix/build-against-pumpkin-main`](https://github.com/xena-studios/Vine/tree/fix/build-against-pumpkin-main) | Not yet sent |
| Vine | Defaults still say 26.2 (protocol 776) while Pumpkin is on 26.3 (777) | Small fix | Not yet written |
| Vine | No README | Docs | Not yet written |
| pumpkin-java-multiversion | No LICENSE file, though `Cargo.toml` says MIT OR Apache-2.0; it bundles ViaBackwards and ViaRewind data, both GPL-3.0 | Issue for the maintainers to decide | Not yet sent |
| pumpkin-java-multiversion | README says chunks are adapted for 1.7, 1.8, 1.9 and 1.18, but `src/chunk/` is left out of the WASM build and never called | Issue | Not yet sent |
