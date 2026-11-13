# Decisions

Every locked decision and why. Change one with a pull request that edits its row.

| # | Decision | Why |
|---|---|---|
| 1 | **Pumpkin for every game server.** | The point of Hallow is to show Pumpkin can run a network. |
| 2 | **Vine as the proxy, forked.** Fork adds a runtime server registry and drain mode; changes go upstream where accepted. | It is Pumpkin's own proxy and built on the same crates. Its server list is fixed in config today. |
| 3 | **Old Java clients through pumpkin-java-multiversion on each backend.** | Vine passes packets through untranslated, so translation belongs on the backend. Fallbacks are in [Risks](RISKS.md). |
| 4 | **Bedrock route chosen in Phase 0** (native with Transfer, or Geyser). | Needs real tests. Vine can't carry Bedrock play traffic either way. |
| 5 | **Multi-proxy from the start.** Shared state lives in the control plane and Redis. | Retrofitting it later touches every plugin. |
| 6 | **Rust everywhere except the player website.** Plugins compile to WASM. | Pumpkin's and Vine's plugin APIs are Rust crates; one toolchain for the platform. |
| 7 | **One monorepo.** Upstream projects are pinned submodules in `upstream/`. | Vine and multiversion already expect Pumpkin beside them; one place to build and test. |
| 8 | **Calagopus Wings is the node agent.** | MIT, Rust, a Pterodactyl-compatible HTTP API the matchmaker can call. |
| 9 | **One panel for everything: a Calagopus Panel fork.** Hallow features are extensions; core edits only for hooks. | Staff manage nodes, servers, players and bans in one place; extensions keep upstream merges cheap. |
| 10 | **Game instances are not panel servers.** The matchmaker owns them; the panel shows them live. | Hundreds of short-lived records would flood the panel. |
| 11 | **Not Raptor.** | Raptor's panel is SaaS-only and its Wings runs no HTTP server, so nothing outside it can start servers. |
| 12 | **gRPC (tonic) between the control plane and plugins.** | Typed contracts shared from `crates/proto`. Plain HTTP and JSON would also work. |
| 13 | **Redis for live state, PostgreSQL for everything kept.** | Standard pairing; PostgreSQL is also the panel's database. |
| 14 | **AGPL-3.0.** | Matches Raptor; compatible with the GPL-3.0 upstreams; network services stay open. |
| 15 | **1.8-style combat: open.** Decide before Phase 2. | Decides whether the minigame library carries a legacy combat module. |
| 16 | **The network buys and returns its own machines.** A fleet manager in the control plane uses provider APIs, Hetzner and OVHcloud first, behind one provider trait. | Peaks are short and hourly machines are cheap; buying by hand doesn't keep up. One trait keeps new providers to one crate. |
| 17 | **Base load on monthly dedicated machines, peaks on hourly cloud.** Burst nodes scale automatically; monthly orders and base-node cancellations need staff approval. | Dedicated is cheapest per core but can't be returned within the hour. |
| 18 | **Spend caps, cooldowns and dry run by default.** | A scaling bug must not run up a bill. |
