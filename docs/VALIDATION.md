# Validation

The Phase 0 test matrix. Each row gets a result and a note before the Phase 0 gate.

## Client versions

Each client joins through Vine, then plays 10 minutes: loads chunks, moves, breaks and places blocks, chats, opens an inventory, fights a mob and a player, and switches servers twice.

**Run 2026-11-14** with headless clients, not real game clients: mineflayer 4.39 (1.8.9 to 26.1), minecraft-protocol 1.68 (1.7.10), `netcheck` (26.3) and bedrock-protocol 3.61 through Geyser 2.12.1-b1302. Backends ran Pumpkin `bc5f7e7bd` with the multiversion plugin `f750eeba` loaded; the proxy was Vine `hallow` `a6f3630`. Bots join, load chunks, chat, get an inventory, try Vine's tab completion and switch servers twice; they don't fight or build. Scripts: [`tools/matrix`](../tools/matrix).

| Client | Join | Chunks | Chat | Inventory | Combat | Switch ×2 | Notes |
|---|---|---|---|---|---|---|---|
| 1.7.10 | **Fail** | | | | | | Vine turns on compression, which 1.7 doesn't have, and the client can't read anything after login (`CompressionFailed`) |
| 1.8.9 | Pass | Pass (638) | No echo | Pass | not tested | **Fail** | Disconnected after joining: a play packet the client can't parse ("array size is abnormally large"). Vine's `/server` not intercepted |
| 1.12.2 | **Fail** | | | | | | Vine reads a chunk packet from the backend as a disconnect and drops the player: its packet ids for this version are wrong |
| 1.16.5 | **Fail** | | | | | | Same as 1.12.2 |
| 1.20.1 | Pass | Pass (638) | No echo | Pass | not tested | **Fail** | `/server` reaches Pumpkin ("Unknown or incomplete command"): Vine looks for the wrong packet id on this version |
| 1.20.4 | **Fail** | | | | | | Same as 1.12.2 |
| 1.21.11 | Pass | Pass (638) | Pass | Pass | not tested | **Fail** | Same `/server` problem as 1.20.1 |
| 26.1 | Pass | Pass | Pass | Pass | not tested | **Fail** | Tab completion shows Vine's `/server`; the switch starts but the client never finishes reconfiguring. Not yet known whether the bot or the translation is at fault |
| 26.3 | Pass | Pass | Pass | Pass | not tested | Pass | `netcheck`, also in CI on every PR. A real 26.3 client joined earlier |
| Bedrock, route A (native) | not used | | | | | | Not used: see [Decisions](DECISIONS.md) 4 |
| Bedrock, route B (Geyser) | Pass | Pass (3797) | Pass | not checked | not tested | **Pass** | 1.26.60 bot over RakNet → Geyser → Vine → Pumpkin. Needed a Vine fix: Geyser sends every command as `chat_command_signed`, which Vine didn't intercept, so `/server` did nothing (fixed in our fork). A real NetherNet client is still to try |

**Run 2026-11-15** with Vine `hallow` `e289b06`, which looks packet ids up per client version (#66 stage A) and adds the small fixes. Same bots and backends as above, with the multiversion plugin loaded.

| Client | Join | Chunks | Switch ×2 | Notes |
|---|---|---|---|---|
| 1.7.10 | Pass | Pass (660) | not tried | Compression is off below 1.8. A minecraft-protocol probe got Vine's `/server` reply |
| 1.8.9 | Pass | Pass (638) | **Fail** | Kicked when the bot tab-completes: the multiversion plugin can't read the pre-1.13 tab-complete packet, which Vine passes through. `/server` works for a client that doesn't tab-complete |
| 1.12.2 | Pass | Pass (638) | **Fail** | Same as 1.8.9 |
| 1.16.5 | Pass | Pass (1506) | Pass | Tab completion shows `/server`. Below 1.20.2 the switch is a second join without the respawn sequence; a proper sequence is #66 stage B |
| 1.20.1 | Pass | Pass (1565) | Pass | Same note as 1.16.5 |
| 1.21.11 | Pass | Pass (266) | Pass | |
| 26.1 | Pass | Pass (281) | Pass | |
| 26.3 | Pass | Pass | Pass | `netcheck` |

Old clients matter most because the multiversion plugin rewrites packet bodies only for entity spawns. Chunks, chat, join and inventory packets are where it is expected to break first.

**What the run showed:** every old-version failure except 1.8.9's parse error is in Vine, not the plugin. Vine uses one packet id table for every client, enables compression for 1.7, and only re-enters configuration on 1.20.2+. This is the work in #66 (translate old clients in Vine, [Decisions](DECISIONS.md) 29); launch stays 26.3 and Bedrock.

## Platform

| Test | Pass when | Result |
|---|---|---|
| Vine messages on old clients | `/server`, chat and tab completion render on 1.8.9 | **Fail, scoped to #66:** on 1.8.9 to 1.21.11 Vine never sees `/server` or tab completion (wrong packet ids), so its own messages can't show. On 26.1 tab completion lists Vine's `/server` |
| Wings create from template | A game instance is up and joinable in under 10 s | **Container floor: 0.35 s** from `docker run` to accepting players, 0.19 s to delete (Hallow server image, M1 Pro, 2026-11-14). Wings' own overhead is measured with the panel in Phase 1 |
| Wings churn | 200 create and delete cycles on one node with no leaks | **Pass at the container level:** 200 cycles in 99 s, 0 failures, no leftover containers, volumes or disk. Repeat through Wings in Phase 1 |
| Cold start | Time from the first queue entry after idle to the player in a game, on the floor node | **0.33 to 0.37 s** (3 runs, M1 Pro, 2026-11-14) from starting a stopped game server to a player joined through Vine and switched onto it. The matchmaker's own time is added in Phase 1 |
| Cloud node ready time | A Hetzner Cloud machine goes from API call to taking games, with Docker and Wings installed by cloud-init; record the time | **Estimated from published measurements, not timed by us** (2026-11-14): about **125 s** typical (80–300) from a stock image with cloud-init installing Docker, about **45 s** (30–200) from a snapshot with Docker, Wings and images baked in. See [Cloud node timing](#cloud-node-timing). Time our own node when the Hetzner provider lands (#42) |
| Provider billing | Billing step and minimum term for Hetzner Cloud, Hetzner Robot and OVHcloud, from their current docs | **Hetzner Cloud:** hourly, each started hour billed, capped at the monthly price; stopped servers and Primary IPv4 still billed. **OVHcloud Public Cloud:** per second after a 1-minute minimum, from `ACTIVE` (older guides say per started hour; confirm on an invoice). **Hetzner Robot:** monthly; auction and in-stock machines usually delivered within minutes to a workday, others in days to weeks. Sources in [Cloud node timing](#cloud-node-timing) |
| Pumpkin plugin API | A test plugin creates a world, loads a map from disk, teleports players, shows a scoreboard and fills an inventory | **Pass, 10/10** (2026-11-13, Pumpkin `058f7ec6f`, `crates/apiprobe`). Three things to design around: see [Plugin API findings](#plugin-api-findings) |
| Bedrock path | How a Bedrock client reaches Pumpkin now that it uses NetherNet, and whether Geyser still fits | **Geyser in front of Vine works** (2026-11-14): clients use NetherNet since 26.60, Geyser 2.12.1 accepts NetherNet and RakNet, and a Bedrock bot joined, chatted and switched servers twice (see the client table). Route chosen in Decisions 4. A real NetherNet client and the Floodgate handoff (#72) are Phase 1 |
| Calagopus extensions | An extension adds a table, an API route and a page | **Yes, from the source:** extensions ship their own `migrations/`, routes (`initialize_router`), background tasks, permissions and settings; the frontend adds admin, account and server pages, navigation, forms and overrides. Building the first one is Phase 1 (#23) |
| Pumpkin capacity | Bot players per backend before tick time passes 50 ms | **200 to 300** crowded, moving players per server on an M1 Pro (2026-11-14); see [Load test](#load-test). With the multiversion plugin loaded it **deadlocks at about 50** |
| Vine capacity | Bot players per proxy before latency rises | 400 held, but Vine used about 8 cores from 200 crowded players on; see [Load test](#load-test) |

The 10 s and 200-cycle targets are starting points; adjust them once there are real numbers.

## Plugin API findings

`crates/apiprobe` is a Pumpkin plugin that runs the calls every Hallow game needs when the first player joins, logging `ok` or `FAIL` for each. Run it with a Pumpkin server that has it in `plugins/`, and `netcheck --route "" --stay 9` as the player.

| Check | Result |
|---|---|
| Create a world for a match | ok |
| Teleport the player into it | ok |
| Build a 5x5 platform | ok, once the player is there |
| Sidebar scoreboard with a score | ok |
| Item in the player's inventory | ok |
| Unload the world (saved to disk) | ok |
| Load it again from disk and enter it | ok |
| The build survived the reload | ok |

What to design around:

1. **The Rust plugin crate targets API v0.1.** `pumpkin-plugin-api` generates its bindings from the v0.1 WIT, so Rust plugins don't get the newer v0.2 calls yet (for example scoreboard `get-score`). v0.1 covers everything above.
2. **Blocks only change in loaded chunks.** Pumpkin's `World::set_block_state` writes into loaded chunks only; a write anywhere else is silently dropped, where vanilla would load the chunk. Games build after a player has loaded the area, or keep their maps prebuilt on disk (which is the plan anyway: maps come from the server repo).
3. **Don't teleport a player during the join event.** Pumpkin kicks a client that confirms a teleport other than the latest one ("Wrong teleport id"), and a teleport during join gives the client two in a row. Vanilla ignores stale confirmations. Pumpkin-MC/Pumpkin#4540 fixes this; until it lands, the SDK delays the first teleport until the client has confirmed its spawn.

Minor: an `ItemStack` made with `minecraft:diamond_sword` reads its key back as `diamond_sword`, so compare keys without the namespace.

## Load test

`netcheck --bots N --ramp 20 --stay 40` against Vine and one Pumpkin server, with `crates/tickwatch` logging tick time once a second. Every bot stands near spawn and walks a small circle, sending a position four times a second, so each one sees every other: a crowded lobby, close to the worst case. Bots, Vine and Pumpkin all ran on one Apple M1 Pro (10 cores, 32 GB), Pumpkin `058f7ec6f`, Vine `408125e`, without the multiversion plugin. Relative numbers, not production capacity.

| Bots | Stayed connected | Join p50 / p95 | Tick median (budget 50 ms) | Peak CPU Pumpkin / Vine | Peak memory Pumpkin / Vine |
|---|---|---|---|---|---|
| 50 | 50/50 | 96 / 220 ms | 5.1 ms | 164 % / 174 % | 307 / 57 MB |
| 100 | 100/100 | 77 / 112 ms | 7.8 ms | 99 % / 355 % | 519 / 99 MB |
| 200 | 200/200 | 86 / 197 ms | 25.4 ms | 155 % / 784 % | 730 / 237 MB |
| 400 | 400/400 | 190 / 461 ms | 88.2 ms, over budget | 292 % / 770 % | 1193 / 451 MB |

- **A server holds 200 to 300 crowded players** before its tick passes 50 ms. Games with 2 to 16 players per instance are far below that; lobbies should cap well under it.
- **Vine costs more CPU than Pumpkin** in a crowded lobby: it relays and compresses every movement update to every player. Plan on more proxies than the player count alone suggests, and profile Vine before launch.

### The multiversion plugin deadlocks the server

With `pumpkin-java-multiversion` loaded, 50 moving players froze the server in 3 of 3 runs (0 of 3 without it): all joined, none was ever seen leaving, CPU near zero.

When any plugin handles `PacketSentEvent`, Pumpkin's `JavaClient::translate_outgoing` fires it for every outgoing play packet, for 26.3 clients too (as an observation), through `fire_blocking`, which `block_on`s the async runtime. It does so from the tick's worker threads, inside `World::broadcast_java_grouped`, while holding locks the async runtime's threads need (thread samples show them blocked in `EntityTracker::remove_entity` and `broadcast_client_move`). The plugin call can't run and the tick can't finish. It's also N squared plugin calls per tick in a crowded area, which is why the server falls 55 ticks behind first.

Until Pumpkin fixes it, the server base image ships the plugin off (`HALLOW_MULTIVERSION=1` loads it), so the launch version range is 26.3 unless the fix lands; see [Decisions](DECISIONS.md) 28.

### Fixing it

Two separate problems, measured with the plugin loaded, through Vine, same machine:

| Pumpkin build | 50 bots | 100 bots, join p95 | 200 bots, join p95 |
|---|---|---|---|
| master `058f7ec6f` | **froze 3/3 runs** | n/a | n/a |
| Writer fix (below) | no freeze 3/3, tick ~9 ms | 69 s | 166 s |
| Writer fix, and no observation for 26.3 clients | no freeze | **224 ms** | **226 ms** |
| No plugin loaded, for reference | | 112 ms | 197 ms |

1. **The freeze.** Our fix moves `PacketSentEvent` from enqueue to the connection's outgoing writer task, which awaits it before framing, so the tick never blocks on a plugin. Branch: [xena-studios/Pumpkin `fix/defer-packet-sent-event`](https://github.com/xena-studios/Pumpkin/tree/fix/defer-packet-sent-event), to be offered upstream after the issue.
2. **The slowness.** With the freeze gone, every packet to every 26.3 player still goes through the plugin as an observation, one call at a time. Skipping that observation for 26.3 clients brings joins back to normal. That changes what Pumpkin promises packet plugins, so it's proposed upstream as an option a plugin opts into (for example "only clients below the current version"), not done in the fix.

Plan: launch on 26.3. Old clients come back once both land upstream, or, if they don't, by translating in our Vine fork instead of in a server plugin: proxies already read every packet, scale out, and would only pay for old clients. See [Decisions](DECISIONS.md) 29.

## Cloud node timing

Hallow has no cloud account yet, so these are published measurements (Hetzner Cloud, mostly ARM in fsn1, one to two runs each), checked 2026-11-14. Hetzner's own `docker-ce` app image was slower to create (40–60 s) than stock Ubuntu.

| Phase | Low (s) | Typical (s) | High (s) | Source |
|---|---|---|---|---|
| Create call to `running`, stock image | 10 | 15 | 60 | [banjo/devbox#31](https://github.com/banjo/devbox/issues/31) (Nov 2026), [VPSBenchmarks](https://vpsbenchmarks.com/labs/provisioning_times) |
| Create call to `running`, snapshot | 12 | 15 | 130 | banjo/devbox#31; [terraform-provider-hcloud#966](https://github.com/hetznercloud/terraform-provider-hcloud/issues/966) (2024, before Hetzner's faster snapshot format) |
| `running` to booted and reachable | 16 | 20 | 30 | banjo/devbox#31 |
| cloud-init installing Docker with apt | 45 | 70 | 120 | banjo/devbox#31 |
| cloud-init on a prepared snapshot | 1 | 5 | 10 | banjo/devbox#31 |
| Pull a 150–300 MB image from GHCR | 5 | 15 | 60+ | estimate from [measured bandwidth](https://www.vpsbenchmarks.com/trials/hetzner_performance_trial_28Aug2026/plan_grades/explanation/category/net_perf) (~870 Mbps) |
| WireGuard join and Wings registering | 2 | 5 | 30 | no published data; estimate |
| **Stock image, Docker by cloud-init, image pulled** | **80** | **125** | **300** | |
| **Snapshot with Docker, Wings and images baked in** | **30** | **45** | **200** | |

What this means for [Fleet](FLEET.md):

- **Bake a snapshot.** Building the node image (Docker, Wings, WireGuard, current server images) in CI cuts about 80 s per node. Pre-pull only the images for the current `live` versions; anything newer pulls on demand.
- **Headroom covers about a minute.** Scaling up has to start before the free game slots run out, because a new burst node takes roughly a minute to take games, and up to a few minutes on a bad day. The floor node and warm pools carry players meanwhile.
- **Capacity is the bigger risk than speed.** Hetzner returns `resource_unavailable` for some server types and locations, and has limited new cloud servers for some accounts since June 2026. The provider needs fallback server types and locations, then OVHcloud.
- **API limits.** 3600 requests per hour per project, refilled at one per second; poll server status every 5 s or so, not tighter.
- **Short nodes cost a full hour on Hetzner.** A node that lives 5 minutes is billed for an hour, so scale-down waits until near the end of the paid hour unless the budget cap is hit.
- **OVHcloud is unmeasured.** Published numbers for its VPS range from 35 s to tens of minutes. Time Public Cloud ourselves before relying on it for fast bursts (#44).
- **GHCR over IPv6** has been reported to slow to minutes. Give nodes IPv4, or mirror images to a registry close to the nodes.

