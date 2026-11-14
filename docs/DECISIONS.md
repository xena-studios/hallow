# Decisions

Every locked decision and why. Change one with a pull request that edits its row.

| # | Decision | Why |
|---|---|---|
| 1 | **Pumpkin for every game server.** | The point of Hallow is to show Pumpkin can run a network. |
| 2 | **Vine as the proxy, forked.** Fork adds a runtime server registry and drain mode; changes go upstream where accepted. | It is Pumpkin's own proxy and built on the same crates. Its server list is fixed in config today. |
| 3 | **Old Java clients through pumpkin-java-multiversion on each server.** The launch version range is whatever passes the Phase 0 matrix: 26.3 and recent versions are required, 1.8 is a goal, not a gate. | Vine passes packets through untranslated, so translation belongs on the server. The plugin only rewrites entity-spawn packets today, so old clients may need work we can't schedule yet. Fallbacks are in [Risks](RISKS.md). |
| 4 | **Bedrock route chosen in Phase 0** (native with Transfer, or Geyser). | Needs real tests. Vine can't carry Bedrock play traffic either way. |
| 5 | **Multi-proxy from the start.** Shared state lives in the control plane and Redis. | Retrofitting it later touches every plugin. |
| 6 | **Rust everywhere except the player website.** Plugins compile to WASM. | Pumpkin's and Vine's plugin APIs are Rust crates; one toolchain for the platform. |
| 7 | **One platform monorepo, plus one repo per server kind.** The monorepo holds the control plane, Vine plugins, SDK, panel, base images and pinned upstream submodules; each lobby and game is its own repo. | Vine and multiversion expect Pumpkin beside them, so the platform builds in one place; games ship on their own schedule and can be owned by different people. |
| 8 | **Calagopus Wings is the node agent.** | MIT, Rust, a Pterodactyl-compatible HTTP API the matchmaker can call. |
| 9 | **One panel for everything: a Calagopus Panel fork.** Hallow features are extensions; core edits only for hooks. | Staff manage nodes, servers, players and bans in one place; extensions keep upstream merges cheap. |
| 10 | **The control plane owns every proxy and server instance; the panel only shows them.** Platform services run under Docker Compose on the floor node. | Everything scales, so nothing is long-lived enough to be a panel record; hundreds of them would flood the panel. |
| 11 | **Not Raptor.** | Raptor's panel is SaaS-only and its Wings runs no HTTP server, so nothing outside it can start servers. |
| 12 | **gRPC (tonic) between the control plane and plugins.** | Typed contracts shared from `crates/proto`. Plain HTTP and JSON would also work. |
| 13 | **Redis for live state, PostgreSQL for everything kept.** | Standard pairing; PostgreSQL is also the panel's database. |
| 14 | **AGPL-3.0.** | Matches Raptor; compatible with the GPL-3.0 upstreams; network services stay open. |
| 15 | **1.8-style combat: open.** Decide before Phase 2. | Decides whether the minigame library carries a legacy combat module. |
| 16 | **The network buys and returns its own machines.** A fleet manager in the control plane uses provider APIs, Hetzner and OVHcloud first, behind one provider trait. | Peaks are short and hourly machines are cheap; buying by hand doesn't keep up. One trait keeps new providers to one crate. |
| 17 | **Base load on monthly dedicated machines, peaks on hourly cloud.** Burst nodes scale automatically; monthly orders and base-node cancellations need staff approval. | Dedicated is cheapest per core but can't be returned within the hour. |
| 18 | **Spend caps, cooldowns and dry run by default.** | A scaling bug must not run up a bill. |
| 19 | **Scale down to one floor node.** With no players, one machine runs the platform services, one Vine and one `hub`; game servers, lobbies, proxies and nodes all scale from their minimums. The floor is configurable for redundancy. | Pay for almost nothing when nobody plays, and the first player still gets in at once. |
| 20 | **Budgets limit growth, never running games.** Monthly budget, daily cap, forecast; at the cap the network queues instead of buying. | Players already in a game are never cut to save money; growth is what costs. |
| 21 | **Two kinds of process: proxies and servers.** Every lobby and game is a server, defined by a server repo with a `hallow.toml` and published as an image to GHCR. See [Servers](SERVERS.md). | One build, deploy and scaling path for everything players play on; a new game is a new repo, not a platform change. |
| 22 | **One match per game instance.** The instance stops when the match ends. | Map reset is a fresh container, nothing leaks between matches, a crash costs one match. Several arenas per server only if the numbers demand it. |
| 23 | **Servers stay proxy-agnostic; proxy plugins stay thin.** Network logic lives in the control plane and Hallow's server plugin. | Vine is young: in its first real test we found it didn't build against Pumpkin and couldn't switch servers. Pumpkin also accepts Velocity forwarding, so switching proxies stays possible. |
| 24 | **Telemetry and update checks off in every Hallow config.** | Pumpkin and Vine both send telemetry to market.pumpkinmc.org by default. A network decides that on purpose. |
| 25 | **Upstream changes are opened by a person, with AI use disclosed.** Agents prepare the branch and description; a person reviews, runs and opens it. | Pumpkin's CONTRIBUTING.md and AGENTS.md require it, and they close PRs that don't follow it. |
| 26 | **A smoke test gates every change.** `netcheck` joins through Vine and switches servers on the Compose network in CI. | Both Vine bugs found so far would have been caught by it. |
| 27 | **Server kinds are versioned by git tags on their own repo.** Tags become image versions; `main` builds are dev-only; `live` and `canary` channels per kind; `hallow.toml` declares the base image range it supports. See [Servers](SERVERS.md#versions). | Each game ships and rolls back on its own, and a platform upgrade can't silently break an old game. |
| 28 | **The multiversion plugin ships off by default** (`HALLOW_MULTIVERSION=1` to load it). | With it loaded, Pumpkin deadlocks at about 50 moving players ([Validation](VALIDATION.md#the-multiversion-plugin-deadlocks-the-server)). Until that is fixed upstream, launch is 26.3 only; old clients come back with the fix or through ViaProxy. |
| 29 | **Old clients: filtered packet events upstream first, translation in Vine as the fallback.** Propose that packet plugins can subscribe to old-version clients only; if that doesn't land, translate old clients in our Vine fork using the multiversion plugin's tables as a library. | Measured: the per-packet observation for 26.3 clients is the whole cost (join p95 166 s down to 226 ms at 200 players). Translating on the proxy is how Java networks run ViaVersion today and keeps the cost on proxies, which scale out. |
