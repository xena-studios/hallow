//! The sidebar: the scoreboard on the right of the screen.
//!
//! The SDK draws one for every player, once a second: the game's title,
//! then what the match is doing (waiting, the countdown, time left, players
//! and teams left), then the game's own lines from
//! [`Minigame::sidebar`](crate::Minigame::sidebar). Only lines that changed
//! are sent again.
//!
//! Lines may use legacy `§` colour codes. The scores that order them are
//! hidden. Two equal lines are told apart with invisible codes, so a blank
//! spacer can appear more than once.
//!
//! Each player gets their own scoreboard (Pumpkin's per-player Java
//! scoreboard), which also carries the teams, so team colours show on name
//! tags and in the tab list.

use std::collections::{BTreeMap, BTreeSet};

use pumpkin_plugin_api::{
    CollisionRule, NametagVisibility, Player, TeamSettings,
    scoreboard::{DisplaySlot, NumberFormat, RenderType, Scoreboard},
    text::TextComponent,
};

use crate::teams::Team;

/// The objective the SDK draws into.
pub const OBJECTIVE: &str = "hallow_sidebar";
/// The most lines a sidebar shows.
pub const MAX_LINES: usize = 15;

/// What one player's sidebar shows now.
#[derive(Debug, Default)]
pub(crate) struct Shown {
    title: Option<String>,
    lines: Vec<String>,
    /// Team id → member names, as sent.
    teams: BTreeMap<String, BTreeSet<String>>,
}

/// Makes every line unique (appending `§r` per earlier copy) and keeps at
/// most [`MAX_LINES`].
#[must_use]
pub fn unique(lines: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    lines
        .iter()
        .take(MAX_LINES)
        .map(|line| {
            let mut line = line.clone();
            while !seen.insert(line.clone()) {
                line.push_str("§r");
            }
            line
        })
        .collect()
}

/// The score that puts line `index` of `count` in place: the first line has
/// the highest.
fn score(index: usize, count: usize) -> i32 {
    i32::try_from(count - index).unwrap_or(i32::MAX)
}

/// What to change to go from `old` to `new` (both from [`unique`]): the
/// entries to remove, and the entries to set with their scores.
#[must_use]
pub fn diff(old: &[String], new: &[String]) -> (Vec<String>, Vec<(String, i32)>) {
    let remove = old
        .iter()
        .filter(|line| !new.contains(line))
        .cloned()
        .collect();
    let set = new
        .iter()
        .enumerate()
        .filter(|(index, line)| {
            old.iter()
                .position(|old_line| old_line == *line)
                .map(|i| score(i, old.len()))
                != Some(score(*index, new.len()))
        })
        .map(|(index, line)| (line.clone(), score(index, new.len())))
        .collect();
    (remove, set)
}

fn text(legacy: &str) -> TextComponent {
    TextComponent::from_legacy_string(legacy)
}

fn team_settings(team: &Team, friendly_fire: bool) -> TeamSettings {
    TeamSettings {
        display_name: text(&team.colored_name()),
        friendly_fire,
        see_friendly_invisibles: true,
        nametag_visibility: NametagVisibility::Always,
        collision_rule: CollisionRule::Always,
        color: team.color,
        prefix: text(team.code()),
        suffix: text(""),
    }
}

/// Brings `player`'s sidebar and team colours up to date. `teams` holds
/// each team with its members' names.
pub(crate) fn draw(
    player: &Player,
    shown: &mut Shown,
    title: &str,
    lines: &[String],
    teams: &[(&Team, Vec<String>)],
    friendly_fire: bool,
) {
    let Some(java) = player.as_java() else {
        return;
    };
    let board = java.get_scoreboard();
    if shown.title.as_deref() != Some(title) {
        if shown.title.is_none() {
            board.add_objective(
                OBJECTIVE,
                text(title),
                RenderType::Integer,
                Some(NumberFormat::Blank),
            );
            board.set_display_slot(DisplaySlot::Sidebar, OBJECTIVE);
        } else {
            board.update_objective(
                OBJECTIVE,
                text(title),
                RenderType::Integer,
                Some(NumberFormat::Blank),
            );
        }
        shown.title = Some(title.to_owned());
    }
    let lines = unique(lines);
    let (remove, set) = diff(&shown.lines, &lines);
    for line in remove {
        board.remove_score(&line, OBJECTIVE);
    }
    for (line, value) in set {
        board.update_score(&line, OBJECTIVE, value, None);
    }
    shown.lines = lines;
    sync_teams(&board, shown, teams, friendly_fire);
}

fn sync_teams(
    board: &Scoreboard,
    shown: &mut Shown,
    teams: &[(&Team, Vec<String>)],
    friendly_fire: bool,
) {
    for (team, members) in teams {
        let sent = shown.teams.entry(team.id.clone()).or_insert_with(|| {
            board.create_team(&team.id, team_settings(team, friendly_fire));
            BTreeSet::new()
        });
        let members: BTreeSet<String> = members.iter().cloned().collect();
        for gone in sent.difference(&members) {
            board.remove_player_from_team(&team.id, gone);
        }
        for new in members.difference(sent) {
            board.add_player_to_team(&team.id, new);
        }
        *sent = members;
    }
}

/// Minutes and seconds, such as `2:05`.
#[must_use]
pub fn clock(seconds: u64) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn unique_lines() {
        assert_eq!(
            unique(&lines(&["a", "", "b", "", ""])),
            lines(&["a", "", "b", "§r", "§r§r"])
        );
        assert_eq!(unique(&vec!["x".to_owned(); 20]).len(), MAX_LINES);
    }

    #[test]
    fn diff_sends_only_changes() {
        let old = lines(&["Time 1:00", "", "Alive 2"]);
        let (remove, set) = diff(&old, &lines(&["Time 0:59", "", "Alive 2"]));
        assert_eq!(remove, ["Time 1:00"]);
        assert_eq!(set, [("Time 0:59".to_owned(), 3)]);

        let (remove, set) = diff(&old, &old);
        assert!(remove.is_empty() && set.is_empty());

        // A new line shifts the scores of the lines above it; the lines
        // below keep theirs, so they aren't sent again.
        let (remove, set) = diff(&old, &lines(&["Time 1:00", "Red", "", "Alive 2"]));
        assert!(remove.is_empty());
        assert_eq!(set, [("Time 1:00".to_owned(), 4), ("Red".to_owned(), 3)]);
    }

    #[test]
    fn clocks() {
        assert_eq!(clock(125), "2:05");
        assert_eq!(clock(9), "0:09");
    }
}
