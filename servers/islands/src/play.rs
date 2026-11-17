//! Match plumbing both games share, kept here until `hallow-sdk` (#27) has
//! it: talking to Hallow's server plugin (state and stats over IPC), loading
//! and picking maps, team colours and balancing, the sidebar, and the test
//! hooks the dev network's bots use.

use std::collections::BTreeMap;

use pumpkin_plugin_api::{
    Player,
    common::NamedColor,
    ipc::send_ipc_message,
    scoreboard::{DisplaySlot, NumberFormat, RenderType},
    text::TextComponent,
    wit::pumpkin::plugin::uuid::Uuid as WitUuid,
};
use uuid::Uuid;

use crate::map::Map;

const SERVER_PLUGIN: &str = "hallow_server";

/// Set when the dev network's bots should be able to end a match from chat
/// (`!test ...`); never on a real network.
pub const TEST_HOOKS_VAR: &str = "HALLOW_GAME_TEST_HOOKS";
/// Picks a map by file name (without `.toml`) instead of at random.
pub const MAP_VAR: &str = "HALLOW_GAME_MAP";

/// Tells Hallow's server plugin the server's state: `in_game` once a match
/// starts (the matchmaker sends nobody else), `draining` when it's over
/// (everyone goes back to the lobby and the instance ends).
pub fn set_state(state: &str) {
    match send_ipc_message(SERVER_PLUGIN, state.as_bytes()) {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => tracing::warn!("islands: hallow_server refused {state}: {err}"),
        Err(()) => tracing::warn!("islands: hallow_server isn't loaded; can't set {state}"),
    }
}

/// Adds `amount` to a player's stat for this server's kind.
pub fn stat(player: Uuid, name: &str, amount: u64) {
    if amount == 0 {
        return;
    }
    let message = format!("stat {player} {name} {amount}");
    if !matches!(send_ipc_message(SERVER_PLUGIN, message.as_bytes()), Ok(Ok(_))) {
        tracing::debug!("islands: couldn't count {name} for {player}");
    }
}

#[must_use]
pub fn to_uuid(id: &WitUuid) -> Uuid {
    Uuid::from_u64_pair(id.high, id.low)
}

#[must_use]
pub fn to_wit(id: Uuid) -> WitUuid {
    let (high, low) = id.as_u64_pair();
    WitUuid { high, low }
}

/// `§`-coloured text, not italic (item names and lore are by default).
#[must_use]
pub fn text(legacy: &str) -> TextComponent {
    TextComponent::from_legacy_string(legacy).italic(false)
}

/// A title and subtitle.
pub fn title(player: &Player, title: &str, subtitle: &str) {
    player.send_title_animation(5, 50, 15);
    player.show_subtitle(TextComponent::from_legacy_string(subtitle));
    player.show_title(TextComponent::from_legacy_string(title));
}

pub fn chat(player: &Player, line: &str) {
    player.send_system_message(TextComponent::from_legacy_string(line), false);
}

/// The maps in `dir` (`*.toml`) made for `mode` (or for any mode), sorted
/// by file name. Maps that don't parse are logged and skipped.
#[must_use]
pub fn load_maps(dir: &str, mode: &str) -> Vec<(String, Map)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        tracing::warn!("islands: no maps folder at {dir}");
        return Vec::new();
    };
    let mut maps: Vec<(String, Map)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".toml")?.to_owned();
            let text = std::fs::read_to_string(entry.path()).ok()?;
            match Map::parse(&text) {
                Ok(map) => Some((id, map)),
                Err(err) => {
                    tracing::warn!("islands: skipping map {name}: {err}");
                    None
                }
            }
        })
        .filter(|(_, map)| map.modes.is_empty() || map.modes.iter().any(|m| m == mode))
        .collect();
    maps.sort_by(|a, b| a.0.cmp(&b.0));
    maps
}

/// The map named `wanted` if there is one, else one picked by `roll`.
#[must_use]
pub fn pick_map(maps: &[(String, Map)], wanted: Option<&str>, roll: u64) -> Option<usize> {
    if maps.is_empty() {
        return None;
    }
    if let Some(wanted) = wanted
        && let Some(i) = maps.iter().position(|(id, _)| id == wanted)
    {
        return Some(i);
    }
    Some((roll % maps.len() as u64) as usize)
}

/// A number that differs between instances, for picking maps and loot.
#[must_use]
pub fn roll() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}

/// A team's colour: its name, chat colour, wool and scoreboard colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Colour {
    pub name: &'static str,
    pub code: &'static str,
    pub wool: &'static str,
    pub glass: &'static str,
    pub named: NamedColor,
}

/// Team colours in team order.
pub const COLOURS: [Colour; 8] = [
    Colour {
        name: "Red",
        code: "§c",
        wool: "minecraft:red_wool",
        glass: "minecraft:red_stained_glass",
        named: NamedColor::Red,
    },
    Colour {
        name: "Blue",
        code: "§9",
        wool: "minecraft:blue_wool",
        glass: "minecraft:blue_stained_glass",
        named: NamedColor::Blue,
    },
    Colour {
        name: "Green",
        code: "§a",
        wool: "minecraft:lime_wool",
        glass: "minecraft:lime_stained_glass",
        named: NamedColor::Green,
    },
    Colour {
        name: "Yellow",
        code: "§e",
        wool: "minecraft:yellow_wool",
        glass: "minecraft:yellow_stained_glass",
        named: NamedColor::Yellow,
    },
    Colour {
        name: "Aqua",
        code: "§b",
        wool: "minecraft:cyan_wool",
        glass: "minecraft:cyan_stained_glass",
        named: NamedColor::Aqua,
    },
    Colour {
        name: "Pink",
        code: "§d",
        wool: "minecraft:pink_wool",
        glass: "minecraft:pink_stained_glass",
        named: NamedColor::LightPurple,
    },
    Colour {
        name: "White",
        code: "§f",
        wool: "minecraft:white_wool",
        glass: "minecraft:white_stained_glass",
        named: NamedColor::White,
    },
    Colour {
        name: "Gray",
        code: "§7",
        wool: "minecraft:gray_wool",
        glass: "minecraft:gray_stained_glass",
        named: NamedColor::Gray,
    },
];

/// The team a new player joins: the one with the fewest players that still
/// has room, the first such team on a tie, so two players land on two
/// teams. `None` when every team is full.
#[must_use]
pub fn balance(sizes: &[usize], team_size: usize) -> Option<usize> {
    sizes
        .iter()
        .enumerate()
        .filter(|(_, size)| **size < team_size)
        .min_by_key(|(i, size)| (**size, *i))
        .map(|(i, _)| i)
}

/// `m:ss`.
#[must_use]
pub fn clock(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Each player's sidebar, redrawn from a list of lines. Pumpkin keeps a
/// scoreboard per player, so every player gets their own (their team, their
/// kills). Lines are made unique with trailing `§r` runs.
#[derive(Default)]
pub struct Sidebars {
    shown: BTreeMap<Uuid, Vec<String>>,
}

const OBJECTIVE: &str = "match";

impl Sidebars {
    /// Shows `lines` (top first) under `title` to `player`, changing only
    /// what changed.
    pub fn show(&mut self, player: &Player, title: &str, lines: &[String]) {
        let Some(java) = player.as_java() else {
            return;
        };
        let id = to_uuid(&player.get_id());
        let lines = unique(lines);
        let scoreboard = java.get_scoreboard();
        let shown = match self.shown.get(&id) {
            Some(shown) if *shown == lines => return,
            Some(shown) => shown.clone(),
            None => {
                scoreboard.add_objective(
                    OBJECTIVE,
                    TextComponent::from_legacy_string(title),
                    RenderType::Integer,
                    Some(NumberFormat::Blank),
                );
                scoreboard.set_display_slot(DisplaySlot::Sidebar, OBJECTIVE);
                Vec::new()
            }
        };
        for old in &shown {
            if !lines.contains(old) {
                scoreboard.remove_score(old, OBJECTIVE);
            }
        }
        let count = lines.len();
        for (i, line) in lines.iter().enumerate() {
            let old = shown.iter().position(|l| l == line);
            if old.map(|o| shown.len() - o) != Some(count - i) {
                let score = i32::try_from(count - i).unwrap_or(0);
                scoreboard.update_score(line, OBJECTIVE, score, Some(NumberFormat::Blank));
            }
        }
        self.shown.insert(id, lines);
    }

    /// Forgets a player who left.
    pub fn forget(&mut self, id: Uuid) {
        self.shown.remove(&id);
    }
}

/// Makes repeated lines (blank ones, mostly) distinct with `§r` runs.
fn unique(lines: &[String]) -> Vec<String> {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    lines
        .iter()
        .map(|line| {
            let n = seen.entry(line.as_str()).or_insert(0);
            let out = format!("{line}{}", "§r".repeat(*n));
            *n += 1;
            out
        })
        .collect()
}

/// A `!test` chat command from a bot, when [`TEST_HOOKS_VAR`] is set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TestHook {
    /// Start the countdown's last seconds now.
    Start,
    /// Die as if knocked into the void.
    Die,
    /// Break the egg of the next team that has one (EggWars).
    BreakEgg,
}

/// Parses `!test start|die|breakegg`.
#[must_use]
pub fn parse_test_hook(message: &str) -> Option<TestHook> {
    let rest = message.trim().strip_prefix("!test")?.trim();
    match rest {
        "start" => Some(TestHook::Start),
        "die" => Some(TestHook::Die),
        "breakegg" => Some(TestHook::BreakEgg),
        _ => None,
    }
}

/// Whether test hooks are on: [`TEST_HOOKS_VAR`] is `1` or `true`.
#[must_use]
pub fn test_hooks_on() -> bool {
    std::env::var(TEST_HOOKS_VAR).is_ok_and(|v| matches!(v.trim(), "1" | "true"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn players_spread_over_teams() {
        assert_eq!(balance(&[0, 0, 0, 0], 2), Some(0));
        assert_eq!(balance(&[1, 0, 0, 0], 2), Some(1));
        assert_eq!(balance(&[1, 1, 1, 1], 2), Some(0));
        assert_eq!(balance(&[2, 2, 1, 2], 2), Some(2));
        assert_eq!(balance(&[1, 1], 1), None);
    }

    #[test]
    fn maps_are_picked() {
        let map = |id: &str| (id.to_owned(), toml_map());
        let maps = vec![map("a"), map("b"), map("c")];
        assert_eq!(pick_map(&maps, Some("b"), 0), Some(1));
        assert_eq!(pick_map(&maps, Some("nope"), 5), Some(2));
        assert_eq!(pick_map(&[], None, 5), None);
    }

    fn toml_map() -> Map {
        Map::parse(
            r#"
            name = "M"
            void_y = 0
            border = 50
            copies = 2
            spectate = [0, 80, 0]
            [team]
            spawn = [0.5, 70, -20.5]
            "#,
        )
        .unwrap()
    }

    #[test]
    fn hooks_and_clock() {
        assert_eq!(parse_test_hook("!test die"), Some(TestHook::Die));
        assert_eq!(parse_test_hook(" !test  breakegg "), Some(TestHook::BreakEgg));
        assert_eq!(parse_test_hook("!test fly"), None);
        assert_eq!(parse_test_hook("hello"), None);
        assert_eq!(clock(125), "2:05");
    }

    #[test]
    fn blank_lines_stay_apart() {
        let lines = unique(&["".into(), "a".into(), "".into()]);
        assert_eq!(lines, ["", "a", "§r"]);
    }
}
