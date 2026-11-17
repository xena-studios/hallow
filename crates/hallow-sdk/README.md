# hallow-sdk

Hallow's minigame library, for a game's Pumpkin plugin in its server repo. One match runs per server instance. The SDK runs it from start to end and keeps the control plane told, so a game only writes its rules. It builds for `wasm32-wasip2` and targets Pumpkin's plugin API v0.1.

| Part | Module | What the SDK does |
|---|---|---|
| Arenas | `arena` | Picks a map from the repo's `maps/`, copies it and loads the copy as a world; spawn points per team; a spectator point; out below `void_y` |
| Match lifecycle | `lifecycle` | Waiting, countdown, in game, ending, then `draining`, so the server plugin sends everyone to the lobby |
| Teams | `teams` | Balanced assignment with preferences, no friendly fire, team chat, coloured name tags |
| Kits | `kit` | Gives each player the game's kit when the match starts |
| Scoreboards | `sidebar` | A sidebar for every player: state, time, teams, plus the game's lines |
| Spectating | `Match::eliminate` | Players who die, fall out or are put out spectate at the map's spectator point; late joiners watch |
| Stats | `stats` | Counts `games_played` and `wins`; games add their own |
| Control plane | `network` | Server state, stats and moving players, through Hallow's server plugin |

[`servers/test-game`](../../servers/test-game) uses every part.

## A game

```rust
use hallow_sdk::{Cause, Kit, Match, Minigame, Settings, stats, teams::{NamedColor, Team}};

struct Duels;

impl Minigame for Duels {
    fn kit(&self, _game: &Match, _player: uuid::Uuid) -> Option<Kit> {
        Some(Kit::new().item(0, "minecraft:iron_sword", 1).chestplate("minecraft:iron_chestplate"))
    }

    fn eliminated(&mut self, game: &mut Match, _player: uuid::Uuid, cause: &Cause) {
        if let Cause::Died { killer: Some(killer) } = cause {
            game.add_stat(*killer, stats::KILLS, 1);
        }
    }
}

// In Plugin::on_load:
let mut settings = Settings::new("§c§lDUELS");
settings.length_secs = Some(300);
settings.teams = vec![
    Team::new("red", "Red", NamedColor::Red),
    Team::new("blue", "Blue", NamedColor::Blue),
];
hallow_sdk::start(&context, "duels", settings, Duels)?;
```

The plugin's `PluginMetadata::permissions` include `hallow_sdk::permissions()`, and `hallow.toml` lists the same under `[[plugins]] permissions` so the base image pre-approves them.

Every `Minigame` method has a default:

| Method | Called |
|---|---|
| `started` | Players are on their spawns with their kits |
| `tick` | Once a second in game |
| `eliminated` | A player is out, with the `Cause` (died, fell, left, put out by the game) |
| `time_up` | The time limit passed; by default the last side standing wins, or it's a draw |
| `ended` | The SDK announced the result and counted wins |
| `kit` | For each player at the start |
| `sidebar` | Extra lines for a player, under the SDK's own |

Inside them, `Match` gives the server, the phase, the arena, players and teams, the last attacker (kill credit), `eliminate`, `end`, `add_stat`, `broadcast` and `title`. Commands and anything else outside the callbacks use `hallow_sdk::later(|game| ...)`, which runs on the next task run.

## Settings

`Settings::new(title)` reads what `hallow.toml` sets through the base image; the rest are code.

| Field | Default |
|---|---|
| `min_players`, `max_players` | `players.min` and `players.max` (`HALLOW_MIN_PLAYERS`, `HALLOW_MAX_PLAYERS`); 2 and 16 without them |
| `countdown_secs`, `full_countdown_secs` | 10 s, and 5 s once `max_players` are on |
| `length_secs` | No time limit |
| `ending_secs` | 5 s of result before `draining` |
| `teams`, `team_size` | Free for all; with teams, the even share |
| `friendly_fire` | Off |
| `team_chat` | On: chat in game goes to the team, `!message` to everyone, out players to each other |
| `last_standing_wins` | On |
| `gamemode` | Survival |
| `map_pick` | `maps.pick` (`HALLOW_MAPS_PICK`); `vote` picks at random until lobbies have a vote |
| `generator` | Empty chunks around the map |

## The lifecycle

| Phase | Server state | Moves on when |
|---|---|---|
| Waiting | `ready` | `min_players` are on |
| Countdown | `ready` | It reaches 0. It goes back to waiting below `min_players`, and shortens once full |
| In game | `in_game` | One side is left, the time limit passes, or the game calls `end` |
| Ending | `in_game` | `ending_secs` have passed |
| Over | `draining` | Never: Hallow's server plugin sends everyone to `hallow.toml`'s `lobby`, and the control plane ends the instance once it's empty |

While waiting, players stay in the server's own world in adventure mode, can't be hurt and don't get hungry. When the match starts, teams are balanced, everyone goes to their team's spawns with their kit, and `games_played` is counted. Anyone who joins later spectates.

## Maps

A map is a folder in the repo's `maps/`: a Pumpkin world plus a `map.toml`.

```toml
name = "Sky Temple"
authors = ["Xena Studios"]
void_y = 40.0                                   # below this a player is out (default -64)
spectators = { x = 0.5, y = 120.0, z = 0.5 }    # default: 10 above the first spawn

[[spawns]]
team = "red"                                    # leave it out for any team
x = 10.5
y = 100.0
z = 0.5
yaw = 90.0
```

`hallow.toml` names the folder (`[maps] dir = "maps"`), and the Dockerfile copies it to `/server/maps`. On every start, `hallow-manifest apply` copies it into the first plugin's data folder (`plugins/data/<plugin>/maps`), the only place a plugin can read files. The SDK picks a map, copies it to `arenas/<map>` and loads the copy, so a match always starts from the map as shipped. `world` in `hallow.toml` must be one folder deep (the default), because the copy is loaded relative to it.

## How it runs

`start` subscribes to Pumpkin's join, leave, damage, hit, death, move, chat and hunger events and runs a task every quarter second. Event handlers only note what happened; the task does the work and calls the game. Pumpkin can call back into a plugin from a call the plugin makes, so the SDK never holds a lock across a call into Pumpkin: while the task runs, the state is out of its lock, and events fired back meanwhile pass through untouched. It never sets health or food, which fires events back into the plugin from inside its own task and can hang the server (found on the hub).

Players are moved a second after they join or respawn, never during the join, because Pumpkin kicks a client that confirms a stale teleport until Pumpkin-MC/Pumpkin#4540 lands.

## Control plane

Games don't talk to the control plane themselves. Hallow's server plugin, in every server image, holds its address and the instance's token, and `network` talks to it over IPC:

| Call | Message | Effect |
|---|---|---|
| `set_state` | `ready`, `in_game`, `draining` | Reported in the next heartbeat |
| `add_stat` | `stat <uuid> <name> <amount>` | Sent with the next stats batch, within 30 s |
| `send_to_lobby` | `send <uuid> [mode]` | The control plane has the player's proxy move them |

Reading stats back in game comes with stats, leaderboards and ranks (#30), as one more call here.

## Tests

```sh
cargo test -p hallow-sdk
cargo clippy --target wasm32-wasip2 -p hallow-sdk -- -D warnings
```

They cover the lifecycle, team balancing, sidebar diffs, `map.toml` and map copying, and the IPC messages. The whole match runs on the dev network with a bot (see [Dev network](../../deploy/compose/README.md#dynamic-games)).
