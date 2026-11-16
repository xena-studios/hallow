//! Player stats, counted on this server and sent as increases in a
//! [`StatsBatch`](hallow_proto::stats::StatsBatch) every [`FLUSH`]; the
//! control plane adds them to the totals for this server's kind.
//!
//! Pure: [`crate::players`] feeds it from Pumpkin's join and leave events and
//! from IPC, with a monotonic clock in milliseconds.
//!
//! | Stat | Counted |
//! |---|---|
//! | `play_time_s` | Seconds online, sent in whole seconds as they add up |
//! | `joins` | Joins to this server |
//! | anything else | What a game bumps over IPC, such as `kills`, `deaths` or `wins` |
//!
//! A game bumps a counter by sending `hallow_server` the IPC message
//! `stat <uuid> <name> [amount]` (amount defaults to 1, and can't be
//! negative). Names are 1 to 64 of `a-z 0-9 _ .`.

use std::collections::BTreeMap;
use std::time::Duration;

use hallow_proto::stats::{MAX_BATCH_PLAYERS, PlayerStatsDelta, valid_stat_name};
use uuid::Uuid;

/// How often increases are sent.
pub const FLUSH: Duration = Duration::from_secs(30);
/// How many flushes in a row may fail before the unsent increases are
/// dropped, so a long outage can't grow them without bound.
pub const MAX_RETRIES: u32 = 10;

pub const PLAY_TIME: &str = "play_time_s";
pub const JOINS: &str = "joins";

#[derive(Debug, Default)]
struct Entry {
    /// When the current stay began (or was last counted), while online.
    online_since: Option<u64>,
    /// Play time not yet turned into whole seconds.
    play_ms: u64,
    counters: BTreeMap<String, i64>,
}

impl Entry {
    fn add(&mut self, stat: &str, by: i64) {
        let value = self.counters.entry(stat.to_owned()).or_default();
        *value = value.saturating_add(by);
    }

    /// Moves play time up to `now` into whole seconds.
    fn count_play(&mut self, now: u64) {
        if let Some(since) = self.online_since {
            self.play_ms += now.saturating_sub(since);
            self.online_since = Some(now);
        }
        let seconds = self.play_ms / 1000;
        self.play_ms %= 1000;
        if seconds > 0 {
            self.add(PLAY_TIME, i64::try_from(seconds).unwrap_or(i64::MAX));
        }
    }
}

#[derive(Debug, Default)]
pub struct Tally {
    players: BTreeMap<Uuid, Entry>,
}

impl Tally {
    /// Counts a join, unless the player is already counted as online (Pumpkin
    /// fires the event again when a player changes worlds).
    pub fn joined(&mut self, player: Uuid, now: u64) {
        let entry = self.players.entry(player).or_default();
        if entry.online_since.is_none() {
            entry.online_since = Some(now);
            entry.add(JOINS, 1);
        }
    }

    pub fn left(&mut self, player: Uuid, now: u64) {
        if let Some(entry) = self.players.get_mut(&player)
            && let Some(since) = entry.online_since.take()
        {
            entry.play_ms += now.saturating_sub(since);
        }
    }

    pub fn bump(&mut self, player: Uuid, stat: &str, by: i64) -> Result<(), String> {
        if !valid_stat_name(stat) {
            return Err(format!(
                "stat {stat:?}: names are 1 to 64 of a-z, 0-9, '_' and '.'"
            ));
        }
        if by < 0 {
            return Err(format!("stat {stat:?}: amounts can't be negative"));
        }
        if by > 0 {
            self.players.entry(player).or_default().add(stat, by);
        }
        Ok(())
    }

    /// The increases since the last take, for at most [`MAX_BATCH_PLAYERS`]
    /// players; the rest wait for the next. Play time is counted up to `now`.
    /// Players who left and have nothing to send are forgotten, along with
    /// any play time under a second.
    pub fn take(&mut self, now: u64) -> Vec<PlayerStatsDelta> {
        let mut deltas = Vec::new();
        self.players.retain(|player, entry| {
            entry.count_play(now);
            if !entry.counters.is_empty() && deltas.len() < MAX_BATCH_PLAYERS {
                deltas.push(PlayerStatsDelta {
                    player: *player,
                    stats: std::mem::take(&mut entry.counters),
                });
            }
            entry.online_since.is_some() || !entry.counters.is_empty()
        });
        deltas
    }

    /// Puts back increases whose batch failed, to go with the next one.
    pub fn restore(&mut self, deltas: Vec<PlayerStatsDelta>) {
        for delta in deltas {
            let entry = self.players.entry(delta.player).or_default();
            for (stat, by) in delta.stats {
                entry.add(&stat, by);
            }
        }
    }
}

/// Parses `stat <uuid> <name> [amount]`; `None` if the message isn't a stat
/// message at all.
pub fn parse_ipc(text: &str) -> Option<Result<(Uuid, String, i64), String>> {
    let rest = text.strip_prefix("stat ")?;
    let mut words = rest.split_whitespace();
    let parsed = (|| {
        let usage = || "send: stat <uuid> <name> [amount]".to_owned();
        let player = words.next().ok_or_else(usage)?;
        let player = Uuid::parse_str(player).map_err(|_| format!("{player:?} isn't a UUID"))?;
        let name = words.next().ok_or_else(usage)?.to_owned();
        let by = match words.next() {
            Some(by) => by
                .parse::<i64>()
                .map_err(|_| format!("{by:?} isn't a whole number"))?,
            None => 1,
        };
        if words.next().is_some() {
            return Err(usage());
        }
        Ok((player, name, by))
    })();
    Some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(deltas: &[PlayerStatsDelta], player: Uuid) -> BTreeMap<String, i64> {
        deltas
            .iter()
            .find(|d| d.player == player)
            .map(|d| d.stats.clone())
            .unwrap_or_default()
    }

    #[test]
    fn counts_play_time_joins_and_counters() {
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut tally = Tally::default();
        tally.joined(a, 0);
        tally.joined(a, 500); // a world change fires join again
        tally.joined(b, 1_000);
        tally.bump(a, "kills", 2).unwrap();
        tally.bump(a, "kills", 1).unwrap();

        let first = tally.take(30_400);
        assert_eq!(
            stats(&first, a),
            BTreeMap::from([
                ("joins".to_owned(), 1),
                ("kills".to_owned(), 3),
                ("play_time_s".to_owned(), 30),
            ])
        );
        assert_eq!(stats(&first, b)["play_time_s"], 29);

        // The 400 ms left over carry into the next window.
        tally.left(a, 31_000);
        let second = tally.take(60_000);
        assert_eq!(
            stats(&second, a),
            BTreeMap::from([("play_time_s".to_owned(), 1)])
        );
        assert_eq!(stats(&second, b)["play_time_s"], 30);
        assert!(!second.iter().any(|d| d.stats.contains_key("joins")));

        // a left: nothing more to send, and they're forgotten.
        let third = tally.take(90_000);
        assert!(stats(&third, a).is_empty());
        assert_eq!(tally.players.len(), 1);
    }

    #[test]
    fn failed_batches_go_with_the_next() {
        let a = Uuid::from_u128(1);
        let mut tally = Tally::default();
        tally.bump(a, "wins", 1).unwrap();
        let failed = tally.take(0);
        assert!(tally.take(0).is_empty());
        tally.bump(a, "wins", 1).unwrap();
        tally.restore(failed);
        assert_eq!(stats(&tally.take(0), a)["wins"], 2);
    }

    #[test]
    fn batches_are_capped() {
        let mut tally = Tally::default();
        let total = MAX_BATCH_PLAYERS + 5;
        for n in 0..total {
            tally.bump(Uuid::from_u128(n as u128), "kills", 1).unwrap();
        }
        assert_eq!(tally.take(0).len(), MAX_BATCH_PLAYERS);
        assert_eq!(tally.take(0).len(), 5);
        assert!(tally.take(0).is_empty());
    }

    #[test]
    fn bumps_are_checked() {
        let a = Uuid::from_u128(1);
        let mut tally = Tally::default();
        assert!(tally.bump(a, "Kills", 1).is_err());
        assert!(tally.bump(a, "kills", -1).is_err());
        tally.bump(a, "kills", 0).unwrap();
        assert!(tally.take(0).is_empty());
    }

    #[test]
    fn ipc_messages() {
        let a = Uuid::from_u128(1);
        let text = format!("stat {a} kills");
        assert_eq!(parse_ipc(&text), Some(Ok((a, "kills".to_owned(), 1))));
        let text = format!("stat {a} wins 3");
        assert_eq!(parse_ipc(&text), Some(Ok((a, "wins".to_owned(), 3))));
        assert_eq!(parse_ipc("in_game"), None);
        assert!(matches!(parse_ipc("stat nope kills"), Some(Err(_))));
        assert!(matches!(parse_ipc(&format!("stat {a}")), Some(Err(_))));
        assert!(matches!(
            parse_ipc(&format!("stat {a} kills x")),
            Some(Err(_))
        ));
        assert!(matches!(
            parse_ipc(&format!("stat {a} kills 1 2")),
            Some(Err(_))
        ));
    }
}
