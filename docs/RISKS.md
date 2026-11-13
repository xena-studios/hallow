# Risks

The biggest risk is that old-version support turns into a protocol-translation project of its own.

| Risk | Impact | Mitigation |
|---|---|---|
| The multiversion plugin can't play 1.7 or 1.8 correctly | Loses the classic PvP audience | Test in Phase 0. Fallbacks: contribute payload translators upstream, put ViaProxy in front of Vine, or launch with 1.20.5+ only |
| Old-client server switching breaks (Vine resets the config phase only on 1.20.2+) | Old clients disconnect or glitch when moving | Test in Phase 0; add a respawn-based switch for old clients in the Vine fork |
| Vine's own packets skip backend translation | Broken proxy messages on old clients | Check in Phase 0; patch Vine if needed |
| Upstream churn: young repos linked by relative paths | Breakage on every Minecraft update | Pin submodules, update deliberately, run the version matrix in CI |
| Missing vanilla features in Pumpkin | Some mechanics unavailable | Design games around what exists; build mechanics in the minigame library; contribute upstream |
| Bedrock route A bypasses Vine | Two routing paths | Pick the route in Phase 0; the control plane treats both the same |
| Calagopus extension API too narrow | More core edits, harder upstream merges | Check in Phase 0; keep core edits to hooks and upstream them |
| Wings too slow for short-lived instances | Long queue times | Measure in Phase 0; pool warm instances; talk to Docker directly as a last resort |
| GPL-3.0 on Vine and Pumpkin | Forks must stay GPL when distributed | Hallow is AGPL-3.0; anything private lives outside this repo |
| Trademark and Minecraft EULA | Takedowns | Original names and art; follow Minecraft's commercial usage guidelines for any store |
