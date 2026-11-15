# Roadmap

Four phases, each ending at a gate. Nothing in a phase starts until the gate before it passes. Every task below has a matching issue in its [milestone](https://github.com/xena-studios/hallow/milestones).

```mermaid
flowchart LR
    p0["Phase 0<br/>Validation spike<br/>~2 weeks"] --> g0{{"1.8, 26.3 and Bedrock<br/>join and switch"}}
    g0 --> p1["Phase 1<br/>Network core"] --> g1{{"Queue lands players in<br/>a fresh instance, 2 proxies"}}
    g1 --> p2["Phase 2<br/>First games"] --> g2{{"Closed beta"}}
    g2 --> p3["Phase 3<br/>Harden and launch"] --> g3{{"Public launch"}}
```

## Phase 0: validation spike

Prove the stack before writing games. About two weeks.

- [x] Add Pumpkin, Vine, multiversion and Wings as submodules in `upstream/` at commits that build together
- [x] Docker Compose for one Vine and two Pumpkin servers with the multiversion plugin
- [x] `netcheck` smoke test (join through Vine, switch servers) running in CI against the Compose network
- [x] Test plugin against Pumpkin's WASM API: create a world, load a map, teleport, scoreboard, inventory
- [x] Run the client version matrix in [Validation](VALIDATION.md)
- [x] Test server switching through Vine on 1.8.9, 1.12.2 and 26.3
- [x] Check that Vine's own messages render on old clients
- [x] Find out how Bedrock clients reach Pumpkin now that it uses NetherNet, then test both routes and pick one (Geyser in front of Vine)
- [x] Measure how fast Calagopus Wings creates and deletes a server from a template
- [x] Check what the Calagopus extension API can add: tables, routes, pages
- [x] Bot load test: players per Pumpkin backend and per Vine
- [x] Time a Hetzner Cloud node from API call to taking games, and check provider billing terms (estimated from published measurements; timed for real with the provider in Phase 2)
- [x] Ask the Pumpkin team which changes they would take upstream (asked; answers tracked in [Upstream](UPSTREAM.md))
- [x] Write the Phase 0 report and update [Decisions](DECISIONS.md) and [Risks](RISKS.md)

**Gate: passed 2026-11-14** for 26.3 and Bedrock (bots; real clients confirm in Phase 1). Old versions fail in Vine and move to #66. See the [Phase 0 report](PHASE0.md).

Original gate: a 26.3 and a Bedrock client each join, play and switch servers twice, and the version matrix is filled in. The matrix sets the launch version range (1.8 is a goal, not a gate; see [Decisions](DECISIONS.md) 3).

## Phase 1: network core

- [x] Vine fork: runtime `register-server` / `unregister-server`, drain mode
- [x] Vine fork: plugin scheduler (delayed and repeating tasks), so plugins can heartbeat and poll
- [ ] Vine: translate old Java clients on the proxy, using the multiversion plugin's tables (Decisions 29)
- [x] `crates/proto`: HTTP/JSON API types for the control plane
- [x] Control plane: server registry, proxy registry, presence in Redis
- [x] Vine plugin: presence, global ping, duplicate-login check, server-list sync
- [x] Vine plugin: cross-proxy commands (`/find`, `/msg` and `/r`, staff `/alert` and `/gkick`); party warp comes with parties
- [x] Load balancer with PROXY protocol and health checks (HAProxy, drain-aware agent checks against the control plane)
- [x] Matchmaker: queues that fill ready game servers, parties kept together
- [ ] Matchmaker: warm instances, start and delete server images through the Wings API
- [ ] Server repos: `hallow.toml` format, server base image, `hallow-server-template`, image build workflow
- [ ] Hallow server plugin: presence, stats and the link to the control plane, in the base image
- [ ] `hub` server repo
- [x] Parties: invite, accept, leave, kick, disband, warp, chat; parties queue together
- [ ] Friends and online status (the schema is in; the commands come next)
- [x] PostgreSQL schema: accounts, ranks, sessions, punishments
- [ ] Panel fork with Hallow branding and the extension scaffold
- [ ] Panel: Network page and Players page
- [ ] Panel: punishments and staff roles
- [ ] Monitoring: Prometheus, Grafana, Loki
- [ ] Node bootstrap: cloud-init and install script for Docker, Wings, WireGuard and firewall; one-time join tokens
- [ ] Anticheat signal collection: Vine and the server plugin record timing, movement and combat data to the verdict service ([Anticheat](ANTICHEAT.md))

**Gate:** with two proxies running, a player joins the `hub`, queues, the matchmaker starts a fresh instance of a test game repo, and the player lands in it; staff see it all in the panel.

## Phase 2: first games

- [ ] `hallow-sdk`: minigame library (arenas, teams, kits, scoreboards, spectating, stats) for server repos
- [ ] Anticheat core checks (movement, reach, click rate, timer) in observe-only mode, panel review page
- [ ] Cheating bots in `netcheck` (speed, fly, reach, auto-clicker, timer) as a CI suite
- [ ] 1.8-style combat module, if [Decisions](DECISIONS.md) keeps it
- [ ] Two launch games (to be chosen), each with its lobby, as server repos
- [ ] Stats, leaderboards and ranks
- [ ] Player website: profiles and leaderboards
- [ ] Panel: reports, games and stats pages
- [ ] Fleet manager: provider trait, scaling rules, drain and cancel, spend caps, dry run
- [ ] Hetzner Cloud provider: burst nodes on demand
- [ ] Scale to one: floor node, per-layer scaling for game instances, lobbies and proxies
- [ ] Budgets: monthly budget, daily cap, forecast, queueing at the cap
- [ ] Panel: Fleet page
- [ ] Closed beta with real players

**Gate:** a closed beta runs a week without data loss or a network-wide outage.

## Phase 3: harden and launch

- [ ] Anticheat actions on: setbacks, kicks, bans with review; thresholds tuned from beta data
- [ ] Load tests at target size across several proxies and nodes
- [ ] DDoS protection in front of the load balancer
- [ ] Backups and a restore drill for PostgreSQL
- [ ] Runbooks: proxy drain, node loss, upstream update
- [ ] OVHcloud Public Cloud provider
- [ ] Dedicated ordering with approval: Hetzner Robot and OVHcloud dedicated
- [ ] Cross-provider private network and multi-region scaling
- [ ] Public launch

**Gate:** public launch.
