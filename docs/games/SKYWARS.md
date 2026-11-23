# Sky Wars

Every player (solo) or pair (teams) starts in a glass cage on their own sky island. When the cages open, loot the island's chests, bridge to the middle for better gear, and fight. One life each: the last player or team standing wins. Chests refill twice, and late in the match the world border closes in.

It's the classic island-looting game, with our own maps, names, loot tables and text. Server repo: [`servers/skywars`](../../servers/skywars), two kinds from one folder, and lobby `skywars-lobby`:

| Kind | Manifest | Players | Maps |
|---|---|---|---|
| `skywars` | `hallow.toml` | 8, one per island | `modes = ["solo"]` |
| `skywars-teams` | `hallow-teams.toml` | 8, two per island | `modes = ["teams"]` |

Both images run the same plugin with the same maps; `SKYWARS_MODE` (`solo` or `teams`) picks which maps.

## A match

| When | What happens |
|---|---|
| Waiting | Each player waits in their island's cage, in adventure mode. Teams fill evenly |
| 2 players on 2 islands | A 20 s countdown; 6 s once all 8 places are taken; it stops if players leave |
| Start | The cages vanish, everyone drops onto their island in survival, the chests are filled. The server turns `in_game` |
| 4:00 and 8:00 | Every chest refills with new loot |
| 10:00 | The world border shrinks to 24 blocks across over 2 minutes |
| 14:00 | A draw |
| One player or team left | It wins. Titles and a top-three kills list, then 8 s later the server turns `draining` and everyone goes back to `skywars-lobby` |

## Rules

- **One life.** Dying or falling below `void_y` makes you a spectator at the middle. A death within 10 s of a hit counts as a kill for the hitter.
- **Building.** Anything goes once the cages open; nothing before.
- **Teams.** No friendly fire.
- **Kit.** None: everything comes from the chests.
- **Combat.** Pumpkin's current combat. The seam for 1.8-style combat (#28) is `hallow_islands::combat`.

## Loot

Island chests roll 4 to 6 items, middle chests 5 to 7, from weighted tables into random slots ([`loot.rs`](../../servers/skywars/plugin/src/loot.rs)). A team's island chests between them always hold blocks, a weapon and a piece of armour.

| | Island chests | Middle chests |
|---|---|---|
| Blocks | Oak planks, stone, andesite (10 to 24) | Oak planks (16 to 32) |
| Weapons | Stone sword, stone axe, sometimes an iron sword or a bow | Iron or diamond sword, Sharpness I iron sword, Power I bow, Knockback I stick |
| Armour | Leather cap, chainmail chest and legs, iron boots and helmet | Iron chest and legs, diamond helmet or chestplate, Protection boots |
| Other | Arrows, food, a golden apple, snowballs, eggs, a stone pickaxe, water or lava, rarely a pearl | Arrows, golden apples, pearls, an Efficiency II iron pickaxe, lava, food |

## Maps

Maps are TOML files in [`servers/skywars/maps`](../../servers/skywars/maps), drawn for one island ([map format](../../servers/islands/src/map.rs)). Solo maps turn the island four times and mirror it, for eight; teams maps turn it four times with two cages per island. A map is picked at random when the instance starts (`HALLOW_GAME_MAP` picks one by name) and built as the world generates.

| Map | Mode | Theme | Layout |
|---|---|---|---|
| Shardspire | Solo | Calcite and andesite | Eight shards round a calcite spire; a ring of stepping stones halfway in; six middle chests |
| Lanternreach | Solo | Dark oak and blackstone | Rafts with lantern posts further out; a sunken shrine in the middle with chests under its roof |
| Greatbough | Solo | Moss and jungle wood | A tree on every island; a great tree in the middle with two chests up on its branches |
| Saltmarsh | Teams | Sand and sandstone | Wide islands for pairs; causeways that stop short of the middle |
| Ironbloom | Teams | Granite, copper and cherry | Islands set off the axis, so each pair is nearer one neighbour; a copper middle |

`cargo test -p hallow-skywars` builds every map and checks that cages, chests and the middle are where they can be used. `cargo run -p hallow-islands --example preview -- <map.toml>` prints one from above.

## Stats

Sent to the control plane as stats for the `skywars` or `skywars-teams` kind:

| Stat | Counted |
|---|---|
| `games_played` | Every player in the match when it ends |
| `wins`, `losses` | The winner (or winning pair); everyone else |
| `kills` | Kills, including knock-offs within 10 s of a hit |
| `deaths` | Deaths |

## Testing

With `HALLOW_GAME_TEST_HOOKS=1` (never on a real network) `!test die` in chat dies as if knocked into the void, and `!test start` cuts the countdown to 3 s. The dev network's bots use them; see [Dev network](../../deploy/compose/README.md#launch-games).
