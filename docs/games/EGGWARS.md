# Egg Wars

Four teams of two start on their own sky islands. Each team guards an egg. Generators make iron, gold and diamonds; the shop keeper on every island sells blocks, weapons, armour and tools for them. While your egg stands you respawn after a death. Once it's broken, your next death is final. The last team with a player left wins.

It's the classic egg-defence island game, with our own maps, names, prices and text. Server repo: [`servers/eggwars`](../../servers/eggwars), kind `eggwars`, lobby `eggwars-lobby`.

## A match

| When | What happens |
|---|---|
| Waiting | Players wait in a glass box above the middle, in adventure mode. Teams fill evenly, so two players land on two teams |
| 2 players on 2 teams | A 20 s countdown. It drops to 6 s once all 8 places are taken, and stops if players leave |
| Start | Everyone spawns on their island in survival with the kit. Teams nobody joined have no egg. The server turns `in_game`, so the matchmaker sends nobody else |
| 20:00 | Sudden death: every egg cracks |
| 30:00 | A draw |
| One team left | It wins. Titles and a top-three kills list, then 8 s later the server turns `draining` and everyone goes back to `eggwars-lobby` |

A player who leaves mid-match is out. A player who joins after the start spectates.

## Rules

- **Eggs.** Punch an enemy egg once to break it. Your own egg can't be broken. A broken egg means no more respawns for that team; players waiting to respawn when it breaks are out.
- **Deaths.** Dying or falling below the map's `void_y` puts you in spectator mode at the middle. With an egg you're back on your island after 5 s. A death within 10 s of a hit counts as a kill for the hitter, so knocking someone off counts.
- **Kit.** Wooden sword, leather cap and boots. Your armour tier and pickaxe stay through respawns; everything else is lost.
- **Building.** Place blocks anywhere inside the border, up to 24 blocks above the map's highest point. Only blocks players placed can be broken. Nothing can be placed on a generator.
- **Teams.** No friendly fire.
- **Combat.** Pumpkin's current combat. The seam for 1.8-style combat (#28) is `hallow_islands::combat`.

## Generators

Stand on a generator's block to collect what it made. Right-click the block to upgrade it. Anyone can upgrade any generator.

| Tier | Level 1 | Level 2 | Level 3 | Holds (by level) | Upgrade costs |
|---|---|---|---|---|---|
| Iron | 1 per 1.8 s | 1 per 1.1 s | 1 per 0.6 s | 48 / 64 / 64 | 24 iron, then 10 gold |
| Gold | 1 per 6 s | 1 per 3.5 s | 1 per 2 s | 16 / 24 / 32 | 16 iron (from off), 12 gold, then 4 diamonds |
| Diamond | 1 per 25 s | 1 per 15 s | 1 per 9 s | 4 / 6 / 8 | 16 gold (from off), 8 diamonds, then 16 diamonds |

Each team island has iron and gold. The middle has diamonds, some maps put gold between teams, and some generators start off until someone pays to turn them on.

## Shop

Right-click your island's shop keeper. Prices are in iron (I), gold (G) and diamonds (D).

| Category | Item | Price |
|---|---|---|
| Blocks | Team wool ×16 | 4 I |
| | Terracotta ×12 | 10 I |
| | Oak planks ×12 | 3 G |
| | End stone ×8 | 20 I |
| | Glass ×6 | 8 I |
| | Ladder ×8 | 4 I |
| | Obsidian ×2 | 3 D |
| Weapons | Stone sword | 10 I |
| | Iron sword | 6 G |
| | Keen iron sword (Sharpness I) | 3 D |
| | Diamond sword | 5 D |
| | Shove stick (Knockback I) | 5 G |
| Armour (kept) | Chainmail set | 28 I |
| | Iron set | 12 G |
| | Diamond set | 6 D |
| Tools | Wooden / stone / iron / diamond pickaxe (kept) | 8 I / 16 I / 4 G / 3 D |
| | Shears | 16 I |
| | Stone axe | 12 I |
| Ranged | Bow | 10 G |
| | Strong bow (Power I) | 4 D |
| | Arrows ×8 | 2 G |
| | Snowballs ×8 | 12 I |
| Supplies | Baked potatoes ×6 | 4 I |
| | Steak ×4 | 2 G |
| | Golden apple | 3 G |
| | Water bucket | 3 G |
| | Ender pearl | 4 D |
| | Guard helmet (Protection I) | 2 D |

Armour sets replace the chestplate and leggings. Tiers only go up: the shop refuses a set or pickaxe you already have or beat.

## Maps

Maps are TOML files in [`servers/eggwars/maps`](../../servers/eggwars/maps), drawn for one team and turned four times ([map format](../../servers/islands/src/map.rs)). The plugin picks one at random when the instance starts (`HALLOW_GAME_MAP` picks one by name) and builds it as the world generates, so every match starts on a fresh map.

| Map | Theme | Layout |
|---|---|---|
| Emberfall | Basalt and blackstone, magma ore | Close: short bridges to a raised middle with two diamond generators; gold outcrops between teams |
| Tidewater | Sand and prismarine | Wide: atolls far apart, piers that stop short of the middle, reef islands with gold between teams; a second diamond generator starts off |
| Mossgrove | Moss, oak and stone bricks | Teams set off to one side, a diamond generator on every island that starts off, iron stepping stones, a ruin in the middle with diamonds and gold |

`cargo test -p hallow-eggwars` builds every map and checks that spawns, eggs, shops and generators stand on solid ground. `cargo run -p hallow-islands --example preview -- <map.toml>` prints one from above.

## Stats

Sent to the control plane as stats for the `eggwars` kind:

| Stat | Counted |
|---|---|
| `games_played` | Every player on a team when the match ends |
| `wins`, `losses` | The winning team's players; everyone else |
| `kills` | Kills, including knock-offs within 10 s of a hit |
| `final_kills` | Kills of players without an egg |
| `deaths`, `final_deaths` | Deaths; deaths that put the player out |
| `eggs_broken` | Enemy eggs broken |
| `items_bought`, `generators_upgraded` | Shop purchases and upgrades |

## Testing

With `HALLOW_GAME_TEST_HOOKS=1` (never on a real network) a player's chat can end a match: `!test breakegg` breaks the next enemy egg, `!test die` dies as if knocked into the void, `!test start` cuts the countdown to 3 s. The dev network's bots use them; see [Dev network](../../deploy/compose/README.md#launch-games).
