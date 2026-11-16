//! Fleet manager settings: one row in force, and the history of changes.

use chrono::{DateTime, Utc};
use hallow_proto::fleet::FleetSettings;
use sqlx::{FromRow, PgPool, types::Json};

/// The settings in force.
#[derive(Clone, Debug, PartialEq, FromRow)]
pub struct StoredFleetSettings {
    pub settings: Json<FleetSettings>,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

/// One change, from `fleet_settings_history`.
#[derive(Clone, Debug, PartialEq, FromRow)]
pub struct FleetSettingsChange {
    pub id: i64,
    pub before: Option<Json<FleetSettings>>,
    pub after: Json<FleetSettings>,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

/// The saved settings, or `None` if they were never changed from the
/// defaults.
pub async fn fleet_settings(pool: &PgPool) -> Result<Option<StoredFleetSettings>, sqlx::Error> {
    sqlx::query_as(
        "SELECT settings, updated_at, updated_by FROM hallow.fleet_settings WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
}

/// Replaces the settings and records the change in the history, in one
/// transaction. Changes are serialized, so each history row's `before` is
/// the previous row's `after`. Validate the settings first.
pub async fn save_fleet_settings(
    pool: &PgPool,
    settings: &FleetSettings,
    updated_by: &str,
) -> Result<StoredFleetSettings, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // Also covers the first save, when there is no row to lock yet.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('hallow.fleet_settings'))")
        .execute(&mut *tx)
        .await?;
    let before: Option<Json<serde_json::Value>> =
        sqlx::query_scalar("SELECT settings FROM hallow.fleet_settings WHERE id = 1")
            .fetch_optional(&mut *tx)
            .await?;
    let saved: StoredFleetSettings = sqlx::query_as(
        "INSERT INTO hallow.fleet_settings (id, settings, updated_at, updated_by)
         VALUES (1, $1, now(), $2)
         ON CONFLICT (id) DO UPDATE
             SET settings = EXCLUDED.settings,
                 updated_at = EXCLUDED.updated_at,
                 updated_by = EXCLUDED.updated_by
         RETURNING settings, updated_at, updated_by",
    )
    .bind(Json(settings))
    .bind(updated_by)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO hallow.fleet_settings_history (before, after, updated_at, updated_by)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(before)
    .bind(Json(settings))
    .bind(saved.updated_at)
    .bind(updated_by)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(saved)
}

/// The latest changes, newest first.
pub async fn fleet_settings_history(
    pool: &PgPool,
    limit: i64,
) -> Result<Vec<FleetSettingsChange>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, before, after, updated_at, updated_by
         FROM hallow.fleet_settings_history
         ORDER BY updated_at DESC, id DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}
