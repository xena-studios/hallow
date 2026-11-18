//! The public API: read-only, no token, rate limited per client address.
//! It serves the player website and anyone else who wants a player's stats
//! or a leaderboard, and only shows what a player could see in game.
//!
//! | Method | Path | Response |
//! |---|---|---|
//! | `GET` | [`NETWORK`] | [`NetworkSummary`] |
//! | `GET` | [`SEARCH`]`?q=` | `Vec<`[`PlayerMatch`]`>`, at most [`SEARCH_MAX`], names starting with `q` |
//! | `GET` | [`profile`] | [`PlayerProfile`] or `404`; by UUID or username |
//! | `GET` | [`LEADERBOARDS`] | `Vec<`[`BoardKind`]`>`: every kind with stats, and its stats |
//! | `GET` | [`leaderboard`]`?window=&limit=` | [`Leaderboard`](crate::stats::Leaderboard) |
//!
//! Errors are [`ApiError`](crate::ApiError) bodies like the rest of the API.
//! A client over its rate gets `429` with `Retry-After`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::stats::{Window, valid_stat_name};

pub const PREFIX: &str = "/public/v1";
pub const NETWORK: &str = "/public/v1/network";
pub const SEARCH: &str = "/public/v1/search";
pub const PLAYERS: &str = "/public/v1/players";
pub const LEADERBOARDS: &str = "/public/v1/leaderboards";

/// The most names [`SEARCH`] returns.
pub const SEARCH_MAX: usize = 10;
/// The most recent games a profile shows.
pub const RECENT_GAMES: u32 = 10;

/// A player's profile, by UUID or username (percent-encoded).
#[must_use]
pub fn profile(player: &str) -> String {
    let mut path = format!("{PLAYERS}/");
    crate::routes::encode_into(&mut path, player);
    path
}

/// A leaderboard, `?window=` and `?limit=` added by [`leaderboard_query`].
#[must_use]
pub fn leaderboard(kind: &str, stat: &str) -> String {
    format!("{LEADERBOARDS}/{kind}/{stat}")
}

/// [`leaderboard`] with its query.
#[must_use]
pub fn leaderboard_query(kind: &str, stat: &str, window: Window, limit: u32) -> String {
    format!(
        "{}?window={}&limit={limit}",
        leaderboard(kind, stat),
        window.as_str()
    )
}

/// Whether `kind` can name a server kind in a path: 1 to 64 of
/// `A-Z a-z 0-9 . _ -`, like every name in the API.
#[must_use]
pub fn valid_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 64
        && kind
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Whether `kind` and `stat` can name a leaderboard.
#[must_use]
pub fn valid_board(kind: &str, stat: &str) -> bool {
    valid_kind(kind) && valid_stat_name(stat)
}

/// `GET /public/v1/network`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkSummary {
    /// Players online now.
    pub online: u32,
    /// Accounts that ever joined.
    pub players: u64,
    /// Games finished and reported.
    pub games: u64,
}

/// One line of `GET /public/v1/search`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerMatch {
    pub uuid: Uuid,
    pub username: String,
}

/// `GET /public/v1/players/{player}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerProfile {
    pub uuid: Uuid,
    pub username: String,
    /// `java` or `bedrock`.
    pub edition: String,
    pub first_seen_ms: u64,
    pub last_seen_ms: u64,
    pub online: bool,
    /// The primary rank, if it isn't the default rank.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<String>,
    /// That rank's prefix with legacy `&` codes; empty for none.
    #[serde(default)]
    pub prefix: String,
    /// Server kind → stat → all-time total.
    pub kinds: BTreeMap<String, BTreeMap<String, i64>>,
    /// The last [`RECENT_GAMES`] games, newest first.
    pub recent_games: Vec<RecentGame>,
}

/// A game in a profile, from that player's side.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentGame {
    pub id: Uuid,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map: Option<String>,
    pub started_ms: u64,
    pub ended_ms: u64,
    /// How many played.
    pub players: u32,
    pub won: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    pub stats: BTreeMap<String, i64>,
}

/// One line of `GET /public/v1/leaderboards`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardKind {
    pub kind: String,
    /// Stats with at least one total, by name.
    pub stats: Vec<String>,
    /// Players with any stat in this kind.
    pub players: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(profile("Steve"), "/public/v1/players/Steve");
        assert_eq!(profile(".Bedrock Guy"), "/public/v1/players/.Bedrock%20Guy");
        assert_eq!(
            leaderboard_query("test-game", "wins", Window::Week, 10),
            "/public/v1/leaderboards/test-game/wins?window=week&limit=10"
        );
        assert!(valid_board("test-game", "wins"));
        assert!(!valid_board("test game", "wins"));
        assert!(!valid_board("test-game", "Wins"));
    }
}
