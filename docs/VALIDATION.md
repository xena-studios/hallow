# Validation

The Phase 0 test matrix. Each row gets a result and a note before the Phase 0 gate.

## Client versions

Each client joins through Vine, then plays 10 minutes: loads chunks, moves, breaks and places blocks, chats, opens an inventory, fights a mob and a player, and switches servers twice.

| Client | Join | Chunks | Chat | Inventory | Combat | Switch ×2 | Notes |
|---|---|---|---|---|---|---|---|
| 1.7.10 | | | | | | | |
| 1.8.9 | | | | | | | |
| 1.12.2 | | | | | | | |
| 1.16.5 | | | | | | | |
| 1.20.1 | | | | | | | |
| 1.20.4 | | | | | | | |
| 1.21.11 | | | | | | | |
| 26.3 | | | | | | | |
| Bedrock, route A (native) | | | | | | | |
| Bedrock, route B (Geyser) | | | | | | | |

Old clients matter most because the multiversion plugin rewrites packet bodies only for entity spawns. Chunks, chat, join and inventory packets are where it is expected to break first.

## Platform

| Test | Pass when | Result |
|---|---|---|
| Vine messages on old clients | `/server`, chat and tab completion render on 1.8.9 | |
| Wings create from template | A game instance is up and joinable in under 10 s | |
| Wings churn | 200 create and delete cycles on one node with no leaks | |
| Cold start | Time from the first queue entry after idle to the player in a game, on the floor node | |
| Cloud node ready time | A Hetzner Cloud machine goes from API call to taking games, with Docker and Wings installed by cloud-init; record the time | |
| Provider billing | Billing step and minimum term for Hetzner Cloud, Hetzner Robot and OVHcloud, from their current docs | |
| Pumpkin plugin API | A test plugin creates a world, loads a map from disk, teleports players, shows a scoreboard and fills an inventory | **Pass, 10/10** (2026-11-13, Pumpkin `058f7ec6f`, `crates/apiprobe`). Three things to design around: see [Plugin API findings](#plugin-api-findings) |
| Bedrock path | How a Bedrock client reaches Pumpkin now that it uses NetherNet, and whether Geyser still fits | |
| Calagopus extensions | An extension adds a table, an API route and a page | |
| Pumpkin capacity | Bot players per backend before tick time passes 50 ms | |
| Vine capacity | Bot players per proxy before latency rises | |

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
