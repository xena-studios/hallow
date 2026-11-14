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

Fixes and reports we have for upstream projects. Each is offered upstream before or alongside using it in our fork.

**How we contribute:** Pumpkin's [CONTRIBUTING.md](https://github.com/Pumpkin-MC/Pumpkin/blob/master/CONTRIBUTING.md#ai-generated-contributions) and [AGENTS.md](https://github.com/Pumpkin-MC/Pumpkin/blob/master/AGENTS.md#pull-requests) apply across Pumpkin-MC. A person opens every PR and issue after reading and running the change, and AI use is disclosed in the description. Agents prepare the branch and the draft, never post. Commits use Conventional Commits with a scope, like `fix(bridge): ...`.

| Project | What | Kind | Status |
|---|---|---|---|
| Vine | Doesn't build against Pumpkin `master`, so Vine's own CI fails: `rsa` pre-release pin, a second `wasmtime` version, `StoreExecutor` API changes, and Pumpkin's removal of RakNet ([Pumpkin-MC/Pumpkin#4427](https://github.com/Pumpkin-MC/Pumpkin/pull/4427)) | Fix | Sent: [Pumpkin-MC/Vine#9](https://github.com/Pumpkin-MC/Vine/pull/9) |
| Vine | Switching servers disconnects every 1.20.2+ client: Vine relays the client's Configuration Acknowledged to a backend still in the login state, which closes the connection | Fix, stacked on #9 | Sent: [Pumpkin-MC/Vine#10](https://github.com/Pumpkin-MC/Vine/pull/10) |
| Vine | Server list fixed in `vine.toml`; no way to add servers at runtime or to drain a proxy | Feature ready: [xena-studios/Vine `feat/runtime-server-registry`](https://github.com/xena-studios/Vine/tree/feat/runtime-server-registry) (`register-server`, `unregister-server`, `set-draining`), stacked on #10 | Not yet sent |
| Vine | Closes the old backend without a disconnect packet on a switch, so Pumpkin logs "Broken pipe" | Small fix | Not yet written |
| Vine | Defaults still say 26.2 (protocol 776) while Pumpkin is on 26.3 (777) | Small fix | Not yet written |
| Vine | No README | Docs | Not yet written |
| Pumpkin | Kicks a client for confirming a stale teleport; vanilla ignores it. A plugin teleporting a player on join kicks them | Already fixed upstream in [Pumpkin-MC/Pumpkin#4540](https://github.com/Pumpkin-MC/Pumpkin/pull/4540) (our equivalent: [xena-studios/Pumpkin `fix/teleport-confirm-vanilla`](https://github.com/xena-studios/Pumpkin/tree/fix/teleport-confirm-vanilla)) | Support #4540 with our reproduction |
| Pumpkin | `World::set_block_state` silently drops writes to unloaded chunks; vanilla loads the chunk | Issue | Not yet sent |
| Pumpkin | `pumpkin-plugin-api` (Rust) only targets the v0.1 WIT, so Rust plugins can't use v0.2 | Question | Not yet sent |
| Pumpkin | Deadlock: with a `PacketSentEvent` handler (the multiversion plugin), `translate_outgoing` `block_on`s plugin calls from the tick while holding locks the async runtime needs; about 50 moving players freeze the server | Issue first; fix ready: [xena-studios/Pumpkin `fix/defer-packet-sent-event`](https://github.com/xena-studios/Pumpkin/tree/fix/defer-packet-sent-event) (on master `bc5f7e7bd`, with the new extra-packet hooks) | Issue not yet sent |
| Pumpkin | `PacketSentEvent` observation for every 26.3 client makes packet plugins cost N squared calls per tick; proposal: let plugins subscribe to old-version clients only | Proposal, on the deadlock issue | Not yet sent |
| pumpkin-java-multiversion | No LICENSE file, though `Cargo.toml` says MIT OR Apache-2.0; it bundles ViaBackwards and ViaRewind data, both GPL-3.0 | Issue | Sent: [#10](https://github.com/Pumpkin-MC/pumpkin-java-multiversion/issues/10) |
| pumpkin-java-multiversion | README says chunks are adapted for 1.7, 1.8, 1.9 and 1.18, but `src/chunk/` is left out of the WASM build and never called | Issue | Sent: [#11](https://github.com/Pumpkin-MC/pumpkin-java-multiversion/issues/11) |
