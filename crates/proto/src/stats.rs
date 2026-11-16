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

#[cfg(test)]
mod tests {
    use super::*;

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
