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
| Calagopus extensions | An extension adds a table, an API route and a page | |
| Pumpkin capacity | Bot players per backend before tick time passes 50 ms | |
| Vine capacity | Bot players per proxy before latency rises | |

The 10 s and 200-cycle targets are starting points; adjust them once there are real numbers.
