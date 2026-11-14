# Phase 0 report

Phase 0 asked whether Pumpkin and Vine can carry a minigame network before any game is written. **Yes, with three gaps that become Hallow's contributions. Phase 0 is closed** (2026-11-14); Phase 1 starts.

## What was proven

| Question | Result | Where |
|---|---|---|
| Do Pumpkin, Vine and the multiversion plugin build together? | Yes, pinned in `upstream/`, now on current upstream | #2, #64 |
| Can a player join through Vine and move between servers? | Yes, after our fix to Vine's server switching; tested on every change in CI | #3, #54, Pumpkin-MC/Vine#10 |
| Can games be built on Pumpkin's plugin API? | Yes, 10/10: worlds, teleport, building, scoreboard, inventory, reload from disk | #55 |
| How many players per server and proxy? | 200 to 300 crowded, moving players per server on an M1 Pro; Vine uses more CPU than Pumpkin | #10 |
| How fast does a server start? | 0.35 s from `docker run` to accepting players; 200 create/delete cycles with no leaks | #8 |
| Can the panel be built from Calagopus extensions? | Yes: tables, routes, background tasks, permissions, pages | #9 |
| How do Bedrock players get in? | Through Geyser in front of Vine (NetherNet since 26.60). A Bedrock bot joined, chatted and switched servers twice, after a Vine fix for Geyser's signed commands | #7, Decisions 4 |
| How fast is a cold start? | 0.35 s from starting a stopped game server to a player on it through Vine | Validation |
| How fast does a cloud node join? | About 125 s from a stock image, about 45 s from a prepared snapshot (published measurements; timed ourselves in Phase 2) | #47, [Validation](VALIDATION.md#cloud-node-timing) |

## The three gaps

1. **Old Java clients.** Pumpkin's multiversion plugin deadlocks the server at about 50 moving players (Pumpkin-MC/Pumpkin#4583), and even with a fix it calls the plugin for every packet. The version matrix also showed that Vine itself breaks old clients: one packet id table for every version, compression on 1.7, configuration re-entry only on 1.20.2+. Only 26.3 and Bedrock pass end to end. Hallow will translate old clients in Vine (Decisions 29, #66). The plugin ships off; launch is 26.3 and Bedrock.
2. **Multiple proxies.** Vine has no shared state; the control plane and Vine plugins provide it (Phase 1).
3. **Proxy CPU.** Vine used about 8 cores at 200 crowded players; profile and fix (Phase 1).

## Upstream so far

Vine: build fix and server-switch fix sent (#9, #10). Pumpkin: deadlock reported and a first fix sent (#4583, #4584; review found it changes cancellation results, rework pending the maintainers' preferred direction); a teleport kick already fixed upstream (#4540). Multiversion: licence question open (#10); chunk translation fixed by its rework (#11). Details in [Upstream](UPSTREAM.md#contributions).

## The gate

The gate was "a 26.3 and a Bedrock client each join, play and switch servers twice, and the version matrix is filled in". **Passed with bots on 2026-11-14.**

| Part | Status |
|---|---|
| 26.3 join and switch | **Pass**: `netcheck` in CI on every PR, and with the multiversion plugin loaded. A real 26.3 client joined; switching with a real client is confirmed in Phase 1 |
| Bedrock join and switch | **Pass**: 1.26.60 bot through Geyser 2.12.1 and Vine joined, chatted and switched twice. A real NetherNet client comes with Geyser in the dev network (#73) |
| Version matrix | **Filled in** ([Validation](VALIDATION.md#client-versions)): 1.8.9, 1.20.1, 1.21.11 and 26.1 join but can't switch; 1.7.10, 1.12.2, 1.16.5 and 1.20.4 can't join. All but one failure are in Vine; fixing them is #66 |
| Cloud node timing | Estimated from published data; timed with a real account when the Hetzner provider lands (#42) |

## Recommendation

Start Phase 1. First: CI that builds and tests the Rust crates, then the control-plane API types and registry (#14, #15), the Vine plugins (#16, #17) and server repos (#20). Confirm with real 26.3 and Bedrock clients as Geyser joins the dev network (#73). Nothing found in Phase 0 blocks the architecture.
