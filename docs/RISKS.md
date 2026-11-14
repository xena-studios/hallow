# Risks

The biggest risk is that old-version support turns into a protocol-translation project of its own.

| Risk | Impact | Mitigation |
|---|---|---|
| The multiversion plugin can't play 1.7 or 1.8 correctly | Loses the classic PvP audience | Test in Phase 0. Fallbacks: contribute payload translators upstream, put ViaProxy in front of Vine, or launch with 1.20.5+ only |
| **Confirmed:** with the multiversion plugin loaded, Pumpkin deadlocks at about 50 moving players (blocking `PacketSentEvent` fired from the tick) | No old clients until fixed; a server with it loaded can freeze | Plugin off by default in the base image; report upstream with a reproduction; ViaProxy in front of Vine is the fallback for old clients |
| Vine's CPU per player is high in crowded areas (about 8 cores at 200 crowded players) | More proxies, higher cost | Cap lobby density; profile Vine and send fixes upstream |
| Old-client server switching breaks (Vine resets the config phase only on 1.20.2+) | Old clients disconnect or glitch when moving | Test in Phase 0; add a respawn-based switch for old clients in the Vine fork |
| Vine's own packets skip backend translation | Broken proxy messages on old clients | Check in Phase 0; patch Vine if needed |
| Upstream churn: young repos linked by relative paths | Breakage on every Minecraft update | Pin submodules, update deliberately, run the version matrix in CI |
| Vine has more bugs like the two found on day one | Players can't move between servers, or worse, in production | `netcheck` in CI on every change; our fork fixes and upstreams them; servers stay proxy-agnostic so Velocity remains a fallback |
| Pumpkin's WASM plugin API can't do something a game needs | A game can't be built | v0.2 already covers worlds, teleport, blocks, scoreboards, inventories and GUIs; Phase 0 builds a test plugin against it; gaps go upstream |
| Cheating on a server whose movement is client-authoritative | Fly and speed hacks go unchecked until our checks exist | Anticheat designed now ([Anticheat](ANTICHEAT.md)); signals from Phase 1; offer server-side movement validation upstream |
| Anticheat false positives | Honest players kicked or banned | Checks report, the verdict service decides; observe-only through the beta; honest bots and beta recordings as a false-positive suite |
| Missing vanilla features in Pumpkin | Some mechanics unavailable | Design games around what exists; build mechanics in the minigame library; contribute upstream |
| Bedrock route A bypasses Vine | Two routing paths | Pick the route in Phase 0; the control plane treats both the same |
| Pumpkin moved Bedrock to NetherNet and removed RakNet (Pumpkin-MC/Pumpkin#4427) | Geyser, Vine's Bedrock ping and our assumptions about how Bedrock clients connect may not hold | Phase 0 finds out how a Bedrock client reaches Pumpkin today, before picking a route |
| Calagopus extension API too narrow | More core edits, harder upstream merges | Check in Phase 0; keep core edits to hooks and upstream them |
| Wings too slow for short-lived instances | Long queue times | Measure in Phase 0; pool warm instances; talk to Docker directly as a last resort |
| The floor node fails while it is the only machine | The whole network is down | Backups and a tested restore; raise the floor to two or more after launch |
| Cold start after a quiet spell is slow | The first players wait for a game | Measure in Phase 0; prebuilt images; a warm pool of one for the top game |
| A scaling bug buys machines in a loop | A large bill | Spend caps, node limits, cooldowns, dry run by default, alerts on every order |
| Provider API keys leak | Someone buys or wipes machines on our account | Keys in the secrets store, scoped to what the fleet manager needs, rotated; audit log of every call |
| New nodes take too long to be ready | Players wait during a spike | Measure in Phase 0; prebuilt images; scale up early with headroom |
| Provider stock or region outage | No burst capacity | More than one provider and region behind the same trait |
| GPL-3.0 on Vine and Pumpkin | Forks must stay GPL when distributed | Hallow is AGPL-3.0; anything private lives outside this repo |
| Trademark and Minecraft EULA | Takedowns | Original names and art; follow Minecraft's commercial usage guidelines for any store |
