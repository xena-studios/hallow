//! Anticheat signal samples.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, types::Json};
use uuid::Uuid;

/// One sample to store.
#[derive(Clone, Debug)]
pub struct NewSignal<'a> {
    pub account: Uuid,
    pub source: &'a str,
    pub at: DateTime<Utc>,
    pub window_ms: i32,
    pub protocol: Option<i32>,
    pub metrics: &'a serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, FromRow)]
pub struct Signal {
    pub source: String,
    pub at: DateTime<Utc>,
    pub window_ms: i32,
    pub protocol: Option<i32>,
    pub metrics: Json<serde_json::Value>,
}

/// Stores samples in one statement. Samples for accounts that never joined
/// are skipped. Returns how many were stored.
pub async fn add_signals(pool: &PgPool, samples: &[NewSignal<'_>]) -> Result<u64, sqlx::Error> {
    if samples.is_empty() {
        return Ok(0);
    }
    let accounts: Vec<Uuid> = samples.iter().map(|s| s.account).collect();
    let sources: Vec<&str> = samples.iter().map(|s| s.source).collect();
    let ats: Vec<DateTime<Utc>> = samples.iter().map(|s| s.at).collect();
    let windows: Vec<i32> = samples.iter().map(|s| s.window_ms).collect();
    let protocols: Vec<Option<i32>> = samples.iter().map(|s| s.protocol).collect();
    let metrics: Vec<Json<&serde_json::Value>> = samples.iter().map(|s| Json(s.metrics)).collect();
    let result = sqlx::query(
        "INSERT INTO hallow.signal_samples (account_uuid, source, at, window_ms, protocol, metrics)
         SELECT s.account, s.source, s.at, s.window_ms, s.protocol, s.metrics
         FROM unnest($1::uuid[], $2::text[], $3::timestamptz[], $4::int[], $5::int[], $6::jsonb[])
              AS s(account, source, at, window_ms, protocol, metrics)
         WHERE EXISTS (SELECT 1 FROM hallow.accounts a WHERE a.uuid = s.account)",
    )
    .bind(&accounts)
    .bind(&sources)
    .bind(&ats)
    .bind(&windows)
    .bind(&protocols)
    .bind(&metrics)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// An account's most recent samples, newest first.
pub async fn recent_signals(
    pool: &PgPool,
    account: Uuid,
    limit: i64,
) -> Result<Vec<Signal>, sqlx::Error> {
    sqlx::query_as(
        "SELECT source, at, window_ms, protocol, metrics
         FROM hallow.signal_samples WHERE account_uuid = $1
         ORDER BY at DESC LIMIT $2",
    )
    .bind(account)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Deletes samples older than `before`. Returns how many went.
pub async fn delete_signals_before(
    pool: &PgPool,
    before: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM hallow.signal_samples WHERE at < $1")
            .bind(before)
            .execute(pool)
            .await?
            .rows_affected(),
    )
}
