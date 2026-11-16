//! Player stats: servers send increases in batches, added to running totals
//! in PostgreSQL per account, server kind and stat.

use std::collections::BTreeMap;

use hallow_db::{PgPool, StatIncrease};
use hallow_proto::stats::{
    MAX_BATCH_PLAYERS, MAX_STATS_PER_PLAYER, PlayerStats, StatsBatch, valid_stat_name,
};
use uuid::Uuid;

use crate::{
    accounts::{DbResult, resolve},
    store::Store,
};

/// Why a batch was refused.
pub fn check(batch: &StatsBatch) -> Result<(), String> {
    if batch.players.len() > MAX_BATCH_PLAYERS {
        return Err(format!("at most {MAX_BATCH_PLAYERS} players per batch"));
    }
    for player in &batch.players {
        if player.stats.len() > MAX_STATS_PER_PLAYER {
            return Err(format!(
                "at most {MAX_STATS_PER_PLAYER} stats per player per batch"
            ));
        }
        for (name, by) in &player.stats {
            if !valid_stat_name(name) {
                return Err(format!(
                    "stat {name:?}: names are 1 to 64 of a-z, 0-9, '_' and '.'"
                ));
            }
            if *by < 0 {
                return Err(format!("stat {name:?}: increases can't be negative"));
            }
        }
    }
    Ok(())
}

/// Adds a checked batch to the totals; returns how many totals changed.
pub async fn store(pool: &PgPool, batch: &StatsBatch) -> DbResult<u64> {
    let increases: Vec<StatIncrease<'_>> = batch
        .players
        .iter()
        .flat_map(|player| {
            player
                .stats
                .iter()
                .filter(|(_, by)| **by > 0)
                .map(|(stat, by)| StatIncrease {
                    account: player.player,
                    kind: &batch.kind,
                    stat,
                    by: *by,
                })
        })
        .collect();
    hallow_db::add_stats(pool, &increases).await
}

/// A player's totals by UUID or username; `None` if they never joined.
pub async fn of(pool: &PgPool, store: &Store, player: &str) -> DbResult<Option<PlayerStats>> {
    let found = match Uuid::parse_str(player) {
        Ok(uuid) => hallow_db::get_account(pool, uuid)
            .await?
            .map(|a| (a.uuid, a.username)),
        Err(_) => resolve(pool, store, player).await?,
    };
    let Some((uuid, username)) = found else {
        return Ok(None);
    };
    let mut kinds: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    for total in hallow_db::stats_of(pool, uuid).await? {
        kinds
            .entry(total.kind)
            .or_default()
            .insert(total.stat, total.value);
    }
    Ok(Some(PlayerStats {
        uuid,
        username,
        kinds,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hallow_proto::stats::PlayerStatsDelta;

    fn batch(stats: &[(&str, i64)]) -> StatsBatch {
        StatsBatch {
            source: "game-1".into(),
            kind: "skywars".into(),
            players: vec![PlayerStatsDelta {
                player: Uuid::nil(),
                stats: stats.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect(),
            }],
        }
    }

    #[test]
    fn checks_names_and_signs() {
        assert!(check(&batch(&[("kills", 2), ("play_time_s", 0)])).is_ok());
        assert!(check(&batch(&[("Kills", 2)])).is_err());
        assert!(check(&batch(&[("kills", -1)])).is_err());
    }
}
