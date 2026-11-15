//! Anticheat signals: proxies and servers send per-player samples, kept in
//! PostgreSQL for `signal_retention_days` and read back for review.

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, TimeDelta, Utc};
use hallow_db::{NewSignal, PgPool};
use hallow_proto::signal::{SignalBatch, SignalRecord};

use crate::{
    AppState,
    accounts::{DbResult, resolve},
    store::Store,
};

/// The most samples one batch may carry.
pub const MAX_BATCH: usize = 5000;
/// How many samples a lookup returns.
const HISTORY: i64 = 200;

fn time(ms: u64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(i64::try_from(ms).unwrap_or(0)).unwrap_or_else(Utc::now)
}

fn millis(at: DateTime<Utc>) -> u64 {
    u64::try_from(at.timestamp_millis()).unwrap_or(0)
}

/// Stores a batch; returns how many samples were kept.
pub async fn store(pool: &PgPool, batch: &SignalBatch) -> DbResult<u64> {
    let metrics: Vec<serde_json::Value> = batch
        .samples
        .iter()
        .map(|s| serde_json::to_value(&s.metrics).unwrap_or_default())
        .collect();
    let rows: Vec<NewSignal<'_>> = batch
        .samples
        .iter()
        .zip(&metrics)
        .map(|(s, metrics)| NewSignal {
            account: s.player,
            source: &batch.source,
            at: time(s.at_ms),
            window_ms: i32::try_from(s.window_ms.max(1)).unwrap_or(i32::MAX),
            protocol: s.protocol,
            metrics,
        })
        .collect();
    hallow_db::add_signals(pool, &rows).await
}

/// A player's recent samples by username, newest first; `None` if nobody
/// by that name has joined.
pub async fn history(
    pool: &PgPool,
    store: &Store,
    name: &str,
) -> DbResult<Option<Vec<SignalRecord>>> {
    let Some((account, _)) = resolve(pool, store, name).await? else {
        return Ok(None);
    };
    let records = hallow_db::recent_signals(pool, account, HISTORY)
        .await?
        .into_iter()
        .map(|s| SignalRecord {
            source: s.source,
            at_ms: millis(s.at),
            window_ms: u32::try_from(s.window_ms).unwrap_or(0),
            protocol: s.protocol,
            metrics: serde_json::from_value(s.metrics.0).unwrap_or_default(),
        })
        .collect();
    Ok(Some(records))
}

/// Deletes samples past the retention period, hourly. Every instance runs
/// it; deleting twice is harmless.
pub async fn retention(state: Arc<AppState>) {
    let Some(pool) = state.db.clone() else { return };
    let days = i64::from(state.config.signal_retention_days);
    let mut tick = tokio::time::interval(Duration::from_secs(3600));
    loop {
        tick.tick().await;
        match hallow_db::delete_signals_before(&pool, Utc::now() - TimeDelta::days(days)).await {
            Ok(0) => {}
            Ok(n) => tracing::info!(deleted = n, "old anticheat signals deleted"),
            Err(err) => tracing::warn!(%err, "signal retention failed"),
        }
    }
}
