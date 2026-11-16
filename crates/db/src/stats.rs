//! Player stats: running totals per account, server kind and stat.

use sqlx::{FromRow, PgPool};
use uuid::Uuid;

/// One increase to add to a total.
#[derive(Clone, Debug)]
pub struct StatIncrease<'a> {
    pub account: Uuid,
    pub kind: &'a str,
    pub stat: &'a str,
    pub by: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct StatTotal {
    pub kind: String,
    pub stat: String,
    pub value: i64,
}

/// Adds the increases to their totals in one statement, creating totals
/// that don't exist yet. Several increases to the same total in one call
/// are summed first. Increases for accounts that never joined are skipped.
/// Returns how many totals changed.
pub async fn add_stats(pool: &PgPool, increases: &[StatIncrease<'_>]) -> Result<u64, sqlx::Error> {
    if increases.is_empty() {
        return Ok(0);
    }
    let accounts: Vec<Uuid> = increases.iter().map(|i| i.account).collect();
    let kinds: Vec<&str> = increases.iter().map(|i| i.kind).collect();
    let stats: Vec<&str> = increases.iter().map(|i| i.stat).collect();
    let bys: Vec<i64> = increases.iter().map(|i| i.by).collect();
    let result = sqlx::query(
        "INSERT INTO hallow.player_stats AS t (account_uuid, kind, stat, value)
         SELECT s.account, s.kind, s.stat, sum(s.by)::bigint
         FROM unnest($1::uuid[], $2::text[], $3::text[], $4::bigint[]) AS s(account, kind, stat, by)
         WHERE EXISTS (SELECT 1 FROM hallow.accounts a WHERE a.uuid = s.account)
         GROUP BY s.account, s.kind, s.stat
         ON CONFLICT (account_uuid, kind, stat)
         DO UPDATE SET value = t.value + EXCLUDED.value, updated_at = now()",
    )
    .bind(&accounts)
    .bind(&kinds)
    .bind(&stats)
    .bind(&bys)
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

/// Every total of an account, by kind then stat.
pub async fn stats_of(pool: &PgPool, account: Uuid) -> Result<Vec<StatTotal>, sqlx::Error> {
    sqlx::query_as(
        "SELECT kind, stat, value FROM hallow.player_stats
         WHERE account_uuid = $1 ORDER BY kind, stat",
    )
    .bind(account)
    .fetch_all(pool)
    .await
}
