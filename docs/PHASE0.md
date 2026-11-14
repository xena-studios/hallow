# Phase 0 report

Phase 0 asked whether Pumpkin and Vine can carry a minigame network before any game is written. **Mostly yes, with three gaps that become Hallow's contributions.** Status on 2026-11-14.

## What was proven

| Question | Result | Where |
|---|---|---|
| Do Pumpkin, Vine and the multiversion plugin build together? | Yes, pinned in `upstream/`, now on current upstream | #2, #64 |
| Can a player join through Vine and move between servers? | Yes, after our fix to Vine's server switching; tested on every change in CI | #3, #54, Pumpkin-MC/Vine#10 |
| Can games be built on Pumpkin's plugin API? | Yes, 10/10: worlds, teleport, building, scoreboard, inventory, reload from disk | #55 |
| How many players per server and proxy? | 200 to 300 crowded, moving players per server on an M1 Pro; Vine uses more CPU than Pumpkin | #10 |
| How fast does a server start? | 0.35 s from `docker run` to accepting players; 200 create/delete cycles with no leaks | #8 |
| Can the panel be built from Calagopus extensions? | Yes: tables, routes, background tasks, permissions, pages | #9 |
| How do Bedrock players get in? | Through Geyser in front of Vine (NetherNet since 26.60); native Bedrock in Vine later | #7, Decisions 4 |

## The three gaps

1. **Old Java clients.** Pumpkin's multiversion plugin deadlocks the server at about 50 moving players (Pumpkin-MC/Pumpkin#4583), and even with a fix it calls the plugin for every packet. Hallow will translate old clients in Vine instead (Decisions 29, #66). The plugin ships off; launch is 26.3 and Bedrock.
2. **Multiple proxies.** Vine has no shared state; the control plane and Vine plugins provide it (Phase 1).
3. **Proxy CPU.** Vine used about 8 cores at 200 crowded players; profile and fix (Phase 1).

## Upstream so far

Vine: build fix and server-switch fix sent (#9, #10). Pumpkin: deadlock reported and a first fix sent (#4583, #4584; review found it changes cancellation results, rework pending the maintainers' preferred direction); a teleport kick already fixed upstream (#4540). Multiversion: licence question open (#10); chunk translation fixed by its rework (#11). Details in [Upstream](UPSTREAM.md#contributions).

## The gate

The gate is "a 26.3 and a Bedrock client each join, play and switch servers twice, and the version matrix is filled in".

| Part | Status |
|---|---|
| 26.3 join and switch | Bots: passing in CI. A real 26.3 client joined the dev network; switching with a real client still to confirm |
| Bedrock join and switch | Needs Geyser in the dev network (#73) and a Bedrock client |
| Version matrix | Old versions wait for translation in Vine (#66); 26.3 is the launch version |

## Recommendation

Start the Phase 1 work that doesn't depend on the open gate items now (Vine's runtime server registry, the control-plane protocol and registry, server repos and the base image), and close the gate with real-client tests as Geyser lands. Nothing found in Phase 0 blocks the architecture.
