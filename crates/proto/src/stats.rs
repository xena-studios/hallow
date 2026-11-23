//! Player stats: named counters per player and server kind, such as
//! `play_time_s`, `joins`, `kills`, `deaths` or `wins` on `skywars`. Servers
//! count what happens on them and send the increases in batches; the control
//! plane adds them to the totals in PostgreSQL.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The most players one batch may carry.
pub const MAX_BATCH_PLAYERS: usize = 1000;
/// The most stats one player may carry in one batch.
pub const MAX_STATS_PER_PLAYER: usize = 64;

/// Whether `name` can be a stat name: 1 to 64 of `a-z 0-9 _ .`.
#[must_use]
pub fn valid_stat_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.'))
}

/// `POST /v1/stats`: increases since the last batch, from one server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatsBatch {
    /// The server that counted them, for logs.
    pub source: String,
    /// The server kind they count towards, such as `skywars`.
    pub kind: String,
    pub players: Vec<PlayerStatsDelta>,
}

/// One player's increases. Each value is added to the stored total; values
/// are never negative.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerStatsDelta {
    pub player: Uuid,
    pub stats: BTreeMap<String, i64>,
}

/// `GET /v1/stats/{player}`, by UUID or username.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerStats {
    pub uuid: Uuid,
    pub username: String,
    /// Server kind → stat name → total. Empty for a player with no stats.
    pub kinds: BTreeMap<String, BTreeMap<String, i64>>,
}

/// Stats the control plane counts from game results ([`GameResult`]):
/// one game for everyone in it, one win for each winner.
pub const GAMES: &str = "games";
pub const WINS: &str = "wins";
/// Counted by every server's plugin: seconds online and joins.
pub const PLAY_TIME: &str = "play_time_s";
pub const JOINS: &str = "joins";

/// The most players one game result may carry.
pub const MAX_GAME_PLAYERS: usize = 200;

/// What a game reports when it ends, over IPC to `hallow_server`
/// (`game_result <json>`). The server plugin adds its own name and kind, a
/// fresh id, and when the game started and ended, and sends it on as a
/// [`GameResult`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameReport {
    /// The map played, if the game has several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map: Option<String>,
    /// When the game started, if the game knows better than the server
    /// plugin (which uses the moment the server turned `in_game`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_ms: Option<u64>,
    pub players: Vec<GamePlayer>,
}

/// `POST /v1/games`: one finished game, from the server it ran on.
///
/// Each player's `stats` are added to their totals for `kind`, together
/// with [`GAMES`] `+1` and, for a winner, [`WINS`] `+1`. So a game reports a
/// counter either live (the `stat` IPC message) or in its result, not both.
/// The game is kept for players' recent games. A result whose `id` was
/// already stored is ignored, so a retried report counts once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameResult {
    pub id: Uuid,
    /// The server it ran on.
    pub source: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map: Option<String>,
    pub started_ms: u64,
    pub ended_ms: u64,
    pub players: Vec<GamePlayer>,
}

/// One player's part in a game.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GamePlayer {
    pub player: Uuid,
    #[serde(default)]
    pub won: bool,
    /// 1 for first place, if the game ranks players.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// What they did this game, such as `kills` or `deaths`. Same rules as
    /// batch stats: names from [`valid_stat_name`], no negative values.
    #[serde(default)]
    pub stats: BTreeMap<String, i64>,
}

/// A leaderboard's time window. Weeks start on Monday and months on the
/// 1st, both at 00:00 UTC.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Window {
    #[default]
    All,
    Week,
    Month,
}

impl Window {
    pub const ALL: [Self; 3] = [Self::All, Self::Week, Self::Month];

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Week => "week",
            Self::Month => "month",
        }
    }

    /// `all`, `week` or `month`, also `alltime`, `weekly` and `monthly`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "all" | "alltime" | "all-time" | "all_time" => Some(Self::All),
            "week" | "weekly" => Some(Self::Week),
            "month" | "monthly" => Some(Self::Month),
            _ => None,
        }
    }
}

/// How many entries a leaderboard returns when `limit` is left out, and the
/// most it returns.
pub const LEADERBOARD_DEFAULT: u32 = 10;
pub const LEADERBOARD_MAX: u32 = 100;

/// `GET /v1/leaderboards/{kind}/{stat}?window=&limit=` (and the public
/// copy): the top totals of one stat in one kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Leaderboard {
    pub kind: String,
    pub stat: String,
    pub window: Window,
    /// When the window began; `None` for all time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
    pub entries: Vec<LeaderboardEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaderboardEntry {
    /// 1 for the top. Ties share nothing: the earlier name alphabetically
    /// goes first.
    pub position: u32,
    pub uuid: Uuid,
    pub username: String,
    pub value: i64,
    /// The player's primary rank, if it isn't the default rank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<String>,
    /// That rank's prefix with legacy `&` codes; empty for none.
    #[serde(default)]
    pub prefix: String,
}

/// `POST /v1/display`: how to show these players (their primary rank).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayRequest {
    pub players: Vec<Uuid>,
}

/// The most players one [`DisplayRequest`] may name.
pub const MAX_DISPLAY_PLAYERS: usize = 1000;

/// One line of the `POST /v1/display` answer. Players who never joined are
/// left out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerDisplay {
    pub uuid: Uuid,
    pub username: String,
    /// The primary rank, if it isn't the default rank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<String>,
    /// With legacy `&` codes, such as `&c[ADMIN] `; empty for none.
    #[serde(default)]
    pub prefix: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows() {
        assert_eq!(Window::parse("weekly"), Some(Window::Week));
        assert_eq!(Window::parse("MONTH"), Some(Window::Month));
        assert_eq!(Window::parse("all"), Some(Window::All));
        assert_eq!(Window::parse("year"), None);
        for window in Window::ALL {
            assert_eq!(Window::parse(window.as_str()), Some(window));
        }
    }

    #[test]
    fn game_reports_need_only_players() {
        let report: GameReport = serde_json::from_str(
            r#"{"players":[{"player":"00000000-0000-0000-0000-000000000001","won":true}]}"#,
        )
        .unwrap();
        assert!(report.players[0].won);
        assert!(report.players[0].stats.is_empty());
        assert_eq!(report.map, None);
    }

    #[test]
    fn stat_names() {
        assert!(valid_stat_name("play_time_s"));
        assert!(valid_stat_name("skywars.kills"));
        assert!(!valid_stat_name(""));
        assert!(!valid_stat_name("Kills"));
        assert!(!valid_stat_name("kills deaths"));
        assert!(!valid_stat_name(&"a".repeat(65)));
    }
}
