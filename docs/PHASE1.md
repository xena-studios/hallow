# Phase 1 report

Phase 1 built the network core: the control plane, Vine plugins, the load balancer, the matchmaker and instances, server repos, the server plugin, parties and friends, accounts and moderation in PostgreSQL, the panel fork, monitoring, node bootstrap and anticheat signal collection. **Every task is done and the gate passed on 2026-11-16, on the dev network and with bots. Phase 1 is closed**; Phase 2 starts. What still needs a real machine, a real client or its own repo moves to Phase 2 (below).

## The gate

The gate was "with two proxies running, a player joins the `hub`, queues, the matchmaker starts a fresh instance of a test game repo, and the player lands in it; staff see it all in the panel".

| Part | Status |
|---|---|
| Two proxies | **Pass**: `proxy-1` and `proxy-2` registered and heartbeating with the control plane (`compose.hallow.yaml`) |
| Join the hub, queue, fresh instance, land in it | **Pass**: a `netcheck` bot joined through the second proxy (`:25566`), landed in the hub, ran `/play test-game` and was moved into a `test-game` instance the matchmaker had just started through the Docker orchestrator (no `test-game` instance existed when it joined). A second bot landed in the warm instance and the matchmaker started the next one |
| Staff see it all in the panel | **Pass**: while the bot was in the game, the control-plane endpoints behind the panel's Overview, Players and Instances pages showed both proxies, every server with its state and players, the network count (1 online), the player's proxy and server, and every instance. `tools/wings-e2e` drove the real panel against a real Wings: the node online, instances listed on the Instances page, an instance's console live through the panel. Not checked: the panel's own Players page with a player online (it reads `GET /v1/players`, which was checked) |
| Same path through Wings | **Pass locally**: `tools/wings-e2e` ran the same queue-to-instance path with `HALLOW_ORCHESTRATOR=wings`, on a node joined with the real `install.sh`, in a container. Not yet on a cloud or dedicated machine |

How to run it again: [Dev network, dynamic games](../deploy/compose/README.md#dynamic-games) and [tools/wings-e2e](../tools/wings-e2e/README.md).

## What was built

| Area | What | Where |
|---|---|---|
| Control plane | Server and proxy registry, presence and cross-proxy commands over HTTP/JSON, state in Redis, any number of instances | #82, #83 |
| Vine | Runtime server registry and drain, plugin scheduler and permissions, per-version packet ids, old Java clients translated on the proxy | #76, #79, #93, #118 |
| Old clients | 1.7.10 to 26.1 bots join, chat, walk and switch twice against 26.3 servers with no plugin on them ([Validation](VALIDATION.md#client-versions), Decisions 33). Old versions are back in reach for launch; the range is set once real clients confirm it | #118 |
| Vine plugin | Presence, global ping, duplicate-login check, server-list sync; `/msg`, `/r`, `/find`, `/alert`, `/gkick` across proxies | #84, #85 |
| Load balancer | HAProxy with PROXY protocol and drain-aware health checks | #87 |
| Matchmaker | Queues that fill ready servers, parties kept together; warm instances, start and delete through Docker or Wings, per-instance tokens | #88, #112, #117 |
| Server repos | `hallow.toml`, the server base image (0.2.0), the server repo template, `hub`, `test-game` and `test-game-lobby` | #106, #119, #122 |
| Server plugin | Registers with the control plane, heartbeats state, players and tick time; play time, joins and game stats | #90, #107 |
| Social | Parties (invite, warp, chat, queue together); friends with online status | #88, #96 |
| Accounts and moderation | PostgreSQL schema; accounts and sessions at login, ranks and staff permissions; `/ban`, `/mute`, `/warn`, `/history` on every proxy, and from the panel | #86, #89, #91, #113 |
| Panel | Calagopus fork with Hallow branding: Overview, Players, Punishments, Anticheat, Ranks, Instances and game kinds, Fleet settings; staff reach Wings and instance consoles through the panel | #94, #111, #114, #121 |
| Monitoring | Control plane `/metrics`, Prometheus, Grafana and Loki, with every server's tick time | #98 |
| Nodes | `deploy/node`: install script and cloud-init, WireGuard, firewall, one-time join tokens, `wings configure --enroll` | #110 |
| Anticheat | Movement and combat samples from the server plugin, timing from Vine, stored and served by the control plane, looked up in the panel | #99, #100 |
| Fleet (ahead of plan) | Provider trait, scaling planner and dry run, a Hetzner Cloud provider against a mock API, runtime fleet settings with audit history | #95, #103 |

## Phase 0's gaps

1. **Old Java clients:** closed. Vine translates them (Decisions 33).
2. **Multiple proxies:** closed. The control plane and the Vine plugin share state across any number of proxies, and the gate ran across two.
3. **Proxy CPU:** still open. Vine hasn't been profiled, and its CPU with many old clients (chunk translation) isn't measured. This moves to Phase 2, before the closed beta (#124).

## Moving to Phase 2

| What | Why it isn't done | Issue |
|---|---|---|
| Bedrock players' own identity behind Vine (Floodgate handoff) | Not written. Today Geyser runs without Floodgate in the dev network. Needed before Bedrock players join the closed beta | #72 |
| A blank real machine becoming a node | `install.sh` and Wings have only run in a container (`tools/wings-e2e`). Runs for real with the Hetzner Cloud provider | #48, #42 |
| Server repos on GitHub and image versions | The template and `hub` are still in this repo; no base image tag is published; `live` and `canary` channels, promote and roll back aren't built (Decisions 27) | #20 |
| Packet timing for anticheat | Vine's collector needs a packet hook in Vine; movement and combat samples already flow | #123 |
| Real clients | Every check so far used headless bots. A real 26.3 client switching servers, a real Bedrock client over NetherNet, and old clients for rendering, combat and building | Closed beta, #33 |
| Vine CPU | See gap 3 | #124 |

## Recommendation

Start Phase 2. First the things the closed beta can't run without: the Floodgate handoff (#72), the Hetzner Cloud provider and a real node (#42, #48), and server repos published with versions (#20); alongside them `hallow-sdk` (#27) and choosing the two launch games (#29). Profile Vine before the beta (#124). Nothing found in Phase 1 blocks the architecture.
