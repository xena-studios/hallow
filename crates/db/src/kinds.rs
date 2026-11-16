//! Server kinds the control plane starts instances of (docs/INSTANCES.md).

use chrono::{DateTime, Utc};
use hallow_proto::instance::KindSpec;
use sqlx::{PgPool, types::Json};

/// A registered kind.
#[derive(Clone, Debug, PartialEq, sqlx::FromRow)]
pub struct ServerKind {
    pub name: String,
    pub spec: Json<KindSpec>,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Option<String>,
}

/// Registers a kind or replaces it. `updated_by` comes from the spec.
pub async fn put_kind(
    pool: &PgPool,
    name: &str,
    spec: &KindSpec,
) -> Result<ServerKind, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO hallow.server_kinds (name, spec, updated_by) VALUES ($1, $2, $3)
         ON CONFLICT (name) DO UPDATE
           SET spec = EXCLUDED.spec, updated_by = EXCLUDED.updated_by, updated_at = now()
         RETURNING name, spec, updated_at, updated_by",
    )
    .bind(name)
    .bind(Json(spec))
    .bind(spec.updated_by.as_deref())
    .fetch_one(pool)
    .await
}

/// Every kind, by name.
pub async fn list_kinds(pool: &PgPool) -> Result<Vec<ServerKind>, sqlx::Error> {
    sqlx::query_as(
        "SELECT name, spec, updated_at, updated_by FROM hallow.server_kinds ORDER BY name",
    )
    .fetch_all(pool)
    .await
}

/// Returns false if there was no such kind.
pub async fn delete_kind(pool: &PgPool, name: &str) -> Result<bool, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM hallow.server_kinds WHERE name = $1")
            .bind(name)
            .execute(pool)
            .await?
            .rows_affected()
            == 1,
    )
}
