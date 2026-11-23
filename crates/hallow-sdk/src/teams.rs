//! Teams: who plays with whom.
//!
//! A game lists its teams in [`Settings::teams`](crate::Settings::teams).
//! With none, every player plays alone (free for all). When the match
//! starts, [`balance`] puts each player on a team:
//!
//! - a player who picked a team (with [`Match::prefer_team`](crate::Match::prefer_team))
//!   gets it while it has room;
//! - everyone else goes to the smallest team, so sizes never differ by more
//!   than one;
//! - teams hold at most `ceil(players / teams)`, or
//!   [`Settings::team_size`](crate::Settings::team_size) when set.
//!
//! Friendly fire is off unless [`Settings::friendly_fire`](crate::Settings::friendly_fire)
//! is set: the SDK cancels damage between teammates. Chat in game goes to
//! the player's team; a message starting with `!` goes to everyone
//! (see [`Settings::team_chat`](crate::Settings::team_chat)). Team colours
//! show on name tags and in the tab list.

use std::collections::BTreeMap;

pub use pumpkin_plugin_api::common::NamedColor;
use uuid::Uuid;

/// One team a game plays with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Team {
    /// Short and stable, such as `red`. Map spawns name teams by it.
    pub id: String,
    /// What players see, such as `Red`.
    pub name: String,
    pub color: NamedColor,
}

impl Team {
    #[must_use]
    pub fn new(id: &str, name: &str, color: NamedColor) -> Self {
        Self {
            id: id.to_owned(),
            name: name.to_owned(),
            color,
        }
    }

    /// The legacy formatting code for the team's colour, such as `§c`.
    #[must_use]
    pub fn code(&self) -> &'static str {
        color_code(self.color)
    }

    /// The team's name in its colour, for chat and the sidebar.
    #[must_use]
    pub fn colored_name(&self) -> String {
        format!("{}{}", self.code(), self.name)
    }
}

/// The legacy formatting code (`§` and a digit or letter) for a colour.
#[must_use]
pub fn color_code(color: NamedColor) -> &'static str {
    match color {
        NamedColor::Black => "§0",
        NamedColor::DarkBlue => "§1",
        NamedColor::DarkGreen => "§2",
        NamedColor::DarkAqua => "§3",
        NamedColor::DarkRed => "§4",
        NamedColor::DarkPurple => "§5",
        NamedColor::Gold => "§6",
        NamedColor::Gray => "§7",
        NamedColor::DarkGray => "§8",
        NamedColor::Blue => "§9",
        NamedColor::Green => "§a",
        NamedColor::Aqua => "§b",
        NamedColor::Red => "§c",
        NamedColor::LightPurple => "§d",
        NamedColor::Yellow => "§e",
        NamedColor::White => "§f",
    }
}

/// Puts every player in `players` on one of `teams`; see the module docs.
/// `size` caps a team (at least the even share). The result is the same for
/// the same input, so tests and replays agree.
#[must_use]
pub fn balance(
    players: &[Uuid],
    teams: &[Team],
    preferences: &BTreeMap<Uuid, String>,
    size: Option<usize>,
) -> BTreeMap<Uuid, String> {
    let mut assigned = BTreeMap::new();
    if teams.is_empty() {
        return assigned;
    }
    let even = players.len().div_ceil(teams.len()).max(1);
    let cap = size.map_or(even, |size| size.max(even));
    let mut counts: Vec<usize> = vec![0; teams.len()];

    // Preferences first, in player order, while the team has room.
    for player in players {
        let Some(wanted) = preferences.get(player) else {
            continue;
        };
        if let Some(index) = teams.iter().position(|team| &team.id == wanted)
            && counts[index] < cap.min(even)
        {
            counts[index] += 1;
            assigned.insert(*player, teams[index].id.clone());
        }
    }
    // Everyone else to the smallest team; ties to the first listed.
    for player in players {
        if assigned.contains_key(player) {
            continue;
        }
        let index = (0..teams.len())
            .min_by_key(|index| counts[*index])
            .expect("teams isn't empty");
        counts[index] += 1;
        assigned.insert(*player, teams[index].id.clone());
    }
    assigned
}

#[cfg(test)]
mod tests {
    use super::*;

    fn teams(n: usize) -> Vec<Team> {
        ["red", "blue", "green", "yellow"][..n]
            .iter()
            .map(|id| Team::new(id, id, NamedColor::White))
            .collect()
    }

    fn players(n: u64) -> Vec<Uuid> {
        (1..=n).map(|i| Uuid::from_u64_pair(0, i)).collect()
    }

    fn sizes(assigned: &BTreeMap<Uuid, String>, teams: &[Team]) -> Vec<usize> {
        teams
            .iter()
            .map(|team| assigned.values().filter(|id| **id == team.id).count())
            .collect()
    }

    #[test]
    fn spreads_players_evenly() {
        let teams = teams(2);
        let assigned = balance(&players(5), &teams, &BTreeMap::new(), None);
        assert_eq!(assigned.len(), 5);
        assert_eq!(sizes(&assigned, &teams), [3, 2]);
        let teams4 = super::tests::teams(4);
        let assigned = balance(&players(2), &teams4, &BTreeMap::new(), None);
        assert_eq!(sizes(&assigned, &teams4), [1, 1, 0, 0]);
    }

    #[test]
    fn keeps_preferences_while_there_is_room() {
        let teams = teams(2);
        let players = players(4);
        // Three want red; only two fit if teams stay even.
        let preferences: BTreeMap<Uuid, String> = players[..3]
            .iter()
            .map(|p| (*p, "red".to_owned()))
            .collect();
        let assigned = balance(&players, &teams, &preferences, None);
        assert_eq!(assigned[&players[0]], "red");
        assert_eq!(assigned[&players[1]], "red");
        assert_eq!(assigned[&players[2]], "blue");
        assert_eq!(sizes(&assigned, &teams), [2, 2]);
    }

    #[test]
    fn ignores_unknown_teams_and_solo_games() {
        let teams = teams(2);
        let players = players(2);
        let preferences = BTreeMap::from([(players[0], "purple".to_owned())]);
        assert_eq!(
            sizes(&balance(&players, &teams, &preferences, None), &teams),
            [1, 1]
        );
        assert!(balance(&players, &[], &preferences, None).is_empty());
    }

    #[test]
    fn colors() {
        let red = Team::new("red", "Red", NamedColor::Red);
        assert_eq!(red.colored_name(), "§cRed");
    }
}
