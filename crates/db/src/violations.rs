//! Anticheat violations, scores and verdicts.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, types::Json};
use uuid::Uuid;

/// One violation to store.
#[derive(Clone, Debug)]
pub struct NewViolation<'a> {
    pub account: Uuid,
    pub source: &'a str,
    pub check: &'a str,
    pub at: DateTime<Utc>,
    pub severity: f64,
    pub protocol: Option<i32>,
    pub summary: &'a str,
    pub evidence: &'a serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, FromRow)]
pub struct StoredViolation {
    pub source: String,
    pub check_name: String,
    pub at: DateTime<Utc>,
    pub severity: f64,
    pub protocol: Option<i32>,
    pub summary: String,
    pub evidence: Json<serde_json::Value>,
}

/// A player's score on one check, as of `updated_at`.
#[derive(Clone, Debug, PartialEq, FromRow)]
pub struct Score {
    pub check_name: String,
    pub level: f64,
    pub updated_at: DateTime<Utc>,
    /// The action last recorded for this score (`none`, `review`, ...).
    pub action: String,
}

#[derive(Clone, Debug, PartialEq, FromRow)]
pub struct Verdict {
    pub check_name: String,
    pub action: String,
    pub level: f64,
    pub at: DateTime<Utc>,
    pub enforced: bool,
    pub reason: String,
}

/// Stores a violation; `false` if the account never joined.
pub async fn add_violation(pool: &PgPool, v: &NewViolation<'_>) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "INSERT INTO hallow.anticheat_violations
             (account_uuid, source, check_name, at, severity, protocol, summary, evidence)
         SELECT $1, $2, $3, $4, $5, $6, $7, $8
         WHERE EXISTS (SELECT 1 FROM hallow.accounts a WHERE a.uuid = $1)",
    )
    .bind(v.account)
    .bind(v.source)
    .bind(v.check)
    .bind(v.at)
    .bind(v.severity)
    .bind(v.protocol)
    .bind(v.summary)
    .bind(Json(v.evidence))
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Adds `severity` to the player's score on `check`, after decaying the old
/// score from its last update to `at` with a half-life of
/// `half_life_secs`. One statement, so concurrent violations from several
/// proxies and servers all count. Returns the new level and the action last
/// recorded, or `None` if the account never joined.
pub async fn bump_score(
    pool: &PgPool,
    account: Uuid,
    check: &str,
    severity: f64,
    at: DateTime<Utc>,
    half_life_secs: f64,
) -> Result<Option<(f64, String)>, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO hallow.anticheat_scores AS s (account_uuid, check_name, level, updated_at)
         SELECT $1, $2, $3, $4
         WHERE EXISTS (SELECT 1 FROM hallow.accounts a WHERE a.uuid = $1)
         ON CONFLICT (account_uuid, check_name) DO UPDATE SET
             level = s.level * power(0.5::float8,
                         GREATEST(0, extract(epoch FROM excluded.updated_at - s.updated_at))::float8
                         / $5) + excluded.level,
             updated_at = GREATEST(s.updated_at, excluded.updated_at)
         RETURNING level, action",
    )
    .bind(account)
    .bind(check)
    .bind(severity)
    .bind(at)
    .bind(half_life_secs.max(1.0))
    .fetch_optional(pool)
    .await
}

/// Sets the action recorded for a score.
pub async fn set_score_action(
    pool: &PgPool,
    account: Uuid,
    check: &str,
    action: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE hallow.anticheat_scores SET action = $3
         WHERE account_uuid = $1 AND check_name = $2",
    )
    .bind(account)
    .bind(check)
    .bind(action)
    .execute(pool)
    .await?;
    Ok(())
}

/// Records a verdict.
pub async fn add_verdict(
    pool: &PgPool,
    account: Uuid,
    check: &str,
    action: &str,
    level: f64,
    enforced: bool,
    reason: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO hallow.anticheat_verdicts
             (account_uuid, check_name, action, level, enforced, reason)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(account)
    .bind(check)
    .bind(action)
    .bind(level)
    .bind(enforced)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

/// A player's scores, by check name.
pub async fn scores_of(pool: &PgPool, account: Uuid) -> Result<Vec<Score>, sqlx::Error> {
    sqlx::query_as(
        "SELECT check_name, level, updated_at, action FROM hallow.anticheat_scores
         WHERE account_uuid = $1 ORDER BY check_name",
    )
    .bind(account)
    .fetch_all(pool)
    .await
}

/// A player's most recent violations, newest first.
pub async fn recent_violations(
    pool: &PgPool,
    account: Uuid,
    limit: i64,
) -> Result<Vec<StoredViolation>, sqlx::Error> {
    sqlx::query_as(
        "SELECT source, check_name, at, severity, protocol, summary, evidence
         FROM hallow.anticheat_violations WHERE account_uuid = $1
         ORDER BY at DESC, id DESC LIMIT $2",
    )
    .bind(account)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// A player's most recent verdicts, newest first.
pub async fn recent_verdicts(
    pool: &PgPool,
    account: Uuid,
    limit: i64,
) -> Result<Vec<Verdict>, sqlx::Error> {
    sqlx::query_as(
        "SELECT check_name, action, level, at, enforced, reason
         FROM hallow.anticheat_verdicts WHERE account_uuid = $1
         ORDER BY at DESC, id DESC LIMIT $2",
    )
    .bind(account)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Deletes violations older than `before`. Returns how many went.
pub async fn delete_violations_before(
    pool: &PgPool,
    before: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM hallow.anticheat_violations WHERE at < $1")
            .bind(before)
            .execute(pool)
            .await?
            .rows_affected(),
    )
}
