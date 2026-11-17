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

/// The spec with `enabled` from its column, which is the truth.
macro_rules! spec {
    () => {
        "spec || jsonb_build_object('enabled', enabled) AS spec"
    };
}

/// Registers a kind or replaces it. `updated_by` and `enabled` come from
/// the spec.
pub async fn put_kind(
    pool: &PgPool,
    name: &str,
    spec: &KindSpec,
) -> Result<ServerKind, sqlx::Error> {
    sqlx::query_as(concat!(
        "INSERT INTO hallow.server_kinds (name, spec, updated_by, enabled) VALUES ($1, $2, $3, $4)
         ON CONFLICT (name) DO UPDATE
           SET spec = EXCLUDED.spec, updated_by = EXCLUDED.updated_by,
               enabled = EXCLUDED.enabled, updated_at = now()
         RETURNING name, ",
        spec!(),
        ", updated_at, updated_by"
    ))
    .bind(name)
    .bind(Json(spec))
    .bind(spec.updated_by.as_deref())
    .bind(spec.enabled)
    .fetch_one(pool)
    .await
}

/// Every kind, by name.
pub async fn list_kinds(pool: &PgPool) -> Result<Vec<ServerKind>, sqlx::Error> {
    sqlx::query_as(concat!(
        "SELECT name, ",
        spec!(),
        ", updated_at, updated_by FROM hallow.server_kinds ORDER BY name"
    ))
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
