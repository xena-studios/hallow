# Alternatives

What else could run a multi-version, multi-proxy minigame network, checked on 2026-11-14 against what Phase 0 measured. **Hallow stays on Pumpkin and Vine** ([Decisions](DECISIONS.md) 30): showing that Pumpkin's own stack can run a network, and contributing what it's missing, is the point of the project. This page records what we'd give up and gain with each alternative, so the trade-off stays visible.

## Servers

| Option | Language | Versions | State | For per-match minigames |
|---|---|---|---|---|
| **Pumpkin** (ours) | Rust | 26.3 within days of release | Pre-1.0, very active | 200 to 300 crowded players per server in our load test; light, fast to start. Bugs found so far were in plugin and teleport handling |
| Minestom | Java | One version; latest release targets 26.2, 26.3 pending since September | Mature, widely used for minigames | Built for it: cheap instances, no vanilla overhead. Lags new Minecraft versions |
| Paper / Folia | Java | Full vanilla | Very mature | 1 to 2 GB per server; too heavy for one match per instance |
| FerrumC | Rust | | Earlier than Pumpkin | Not ready |
| Valence | Rust | | No pushes since June 2026 | Stalled |

## Proxies

| Option | Language | Old clients | Multi-proxy | Notes |
|---|---|---|---|---|
| **Vine** (ours) | Rust | None yet: Pumpkin's multiversion plugin deadlocked and, fixed, is slow under load | Ours to build | Young; two serious bugs fixed by us; high CPU in a crowded lobby |
| Velocity 4 | Java 25 | ViaVersion, ViaBackwards and ViaRewind on the proxy: the proven standard, 1.7 to latest | Mature Redis sync plugins | What most networks run |
| Gate | Go | Passes clients to matching servers; no translation, ViaVersion doesn't run on it | Partial | Very light, built-in Geyser |
| Infrarust | Rust | No | No | Routing only |
| ViaProxy | Java | Yes, standalone | | An extra hop in front of the proxy |

## Panels and orchestration

| Option | Notes |
|---|---|
| **Calagopus fork** (ours) | Rust and TypeScript, MIT; Wings has an HTTP API the control plane calls; the panel saves building a dashboard |
| Pelican / Pterodactyl | PHP; nothing over Calagopus for us |
| Shulker on Agones | Kubernetes operator for Minecraft networks (proxy and server fleets, autoscaling). Would replace much of the control plane, at the cost of running Kubernetes, heavy for a one-machine floor. Worth a second look if Hallow outgrows a handful of machines |

## What staying on Pumpkin and Vine means

The gaps are in the proxy, and they become Hallow's contributions to Vine:

1. **Old clients.** Translate on the proxy, the way ViaVersion runs on Velocity: Vine already parses every packet, proxies scale out, and only old clients pay. The multiversion plugin's translation can be linked into Vine as a library. Pumpkin servers then only ever see 26.3, and its plugin path (Pumpkin-MC/Pumpkin#4583) stops mattering for us.
2. **Multiple proxies.** Shared presence, global counts and cross-proxy commands, as planned in [Multi-proxy](MULTI-PROXY.md).
3. **Proxy CPU.** Profile Vine in a crowded lobby (about 8 cores at 200 players in our test) and fix what we find.

Each one is offered upstream. If one stalls badly, the fallback for that layer is in the tables above; servers stay proxy-agnostic ([Decisions](DECISIONS.md) 23) so a proxy swap would not touch the games.

Sources: project pages and GitHub activity for each option (stars, last push, latest release) on 2026-11-14; ViaVersion, Velocity, Gate, Minestom and Shulker documentation.
