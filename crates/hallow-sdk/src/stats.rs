//! Player stats: named counters per player for this server kind.
//!
//! A game counts what happens in a match; Hallow's server plugin sends the
//! increases to the control plane every 30 s, and the control plane adds
//! them to the player's totals for the kind (`GET /v1/stats/{player}`).
//! Play time and joins are counted by the server plugin itself.
//!
//! [`Match`](crate::Match) counts [`GAMES_PLAYED`] for everyone who starts
//! a match and [`WINS`] for the winners. Games add their own with
//! [`Match::add_stat`](crate::Match::add_stat) or [`add`]:
//!
//! ```rust,ignore
//! game.add_stat(killer, hallow_sdk::stats::KILLS, 1);
//! ```
//!
//! Reading totals back in game (for a stats menu or a leaderboard) comes
//! with stats, leaderboards and ranks (#30), as another `hallow_server` IPC
//! call wrapped here.

use uuid::Uuid;

/// Matches started. Counted by the SDK.
pub const GAMES_PLAYED: &str = "games_played";
/// Matches won. Counted by the SDK.
pub const WINS: &str = "wins";
/// Players killed. Counted by games.
pub const KILLS: &str = "kills";
/// Deaths. Counted by games.
pub const DEATHS: &str = "deaths";

/// Adds `amount` to `player`'s `name` stat. Names are 1 to 64 of
/// `a-z 0-9 _ .`. Returns whether Hallow's server plugin took it.
pub fn add(player: Uuid, name: &str, amount: u32) -> bool {
    crate::network::add_stat(player, name, amount)
}
