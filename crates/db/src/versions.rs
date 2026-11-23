//! Server kind versions, their live and canary channels, and the audit log
//! of every version action (docs/SERVERS.md#versions).
//!
//! `hallow.server_kinds.spec.image` stays the live image; the channels say
//! which version that is. Channel changes lock the kind's row, so they are
//! serialized per kind, and each writes its audit entry in the same
//! transaction.

use chrono::{DateTime, Utc};
use hallow_proto::instance::KindSpec;
use sqlx::{FromRow, PgConnection, PgPool, types::Json};

use crate::ServerKind;

const VERSION_COLUMNS: &str =
    "kind, version, image, base, base_version, git_commit, added_at, added_by";

/// A recorded version of a kind.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct KindVersion {
    pub kind: String,
    pub version: String,
    pub image: String,
    pub base: String,
    pub base_version: Option<String>,
    pub git_commit: Option<String>,
    pub added_at: DateTime<Utc>,
    pub added_by: String,
}

/// A version to record.
#[derive(Clone, Copy, Debug)]
pub struct NewVersion<'a> {
    pub kind: &'a str,
    pub version: &'a str,
    pub image: &'a str,
    pub base: &'a str,
    pub base_version: Option<&'a str>,
    pub git_commit: Option<&'a str>,
    pub by: &'a str,
    pub panel_user: Option<&'a str>,
}

/// What [`add_version`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AddedVersion {
    Added(KindVersion),
    /// The version was already recorded; this is it (its image may differ).
    Exists(KindVersion),
    NoKind,
}

/// A kind's channels. All `None` before any was set.
#[derive(Clone, Debug, Default, PartialEq, Eq, FromRow)]
pub struct KindChannels {
    pub live: Option<String>,
    pub previous: Option<String>,
    pub canary: Option<String>,
    pub canary_percent: Option<i16>,
    pub canary_since: Option<DateTime<Utc>>,
}

/// What the instance planner needs per kind: the live version and the
/// canary's image and share.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct Rollout {
    pub kind: String,
    pub live: Option<String>,
    pub canary: Option<String>,
    pub canary_image: Option<String>,
    pub canary_percent: Option<i16>,
}

/// An audit entry to write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewVersionAudit {
    pub kind: String,
    /// `hallow_proto::version::VersionAction::as_str`.
    pub action: &'static str,
    pub version: Option<String>,
    pub from_version: Option<String>,
    pub image: Option<String>,
    pub percent: Option<i16>,
    pub by: String,
    pub panel_user: Option<String>,
}

/// An audit entry.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct VersionAudit {
    pub id: i64,
    pub kind: String,
    pub action: String,
    pub version: Option<String>,
    pub from_version: Option<String>,
    pub image: Option<String>,
    pub percent: Option<i16>,
    pub by_name: String,
    pub panel_user: Option<String>,
    pub at: DateTime<Utc>,
}

/// A kind as a channel change sees it, inside the change's transaction.
#[derive(Clone, Debug, PartialEq)]
pub struct KindState {
    pub kind: ServerKind,
    pub channels: KindChannels,
    /// Newest first.
    pub versions: Vec<KindVersion>,
}

impl KindState {
    #[must_use]
    pub fn version(&self, version: &str) -> Option<&KindVersion> {
        self.versions.iter().find(|v| v.version == version)
    }
}

/// What a channel change writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelChange {
    pub channels: KindChannels,
    /// The kind's new live image, if it changes.
    pub image: Option<String>,
    pub audit: NewVersionAudit,
}

async fn write_audit(conn: &mut PgConnection, audit: &NewVersionAudit) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO hallow.kind_version_audit
           (kind, action, version, from_version, image, percent, by_name, panel_user)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(&audit.kind)
    .bind(audit.action)
    .bind(&audit.version)
    .bind(&audit.from_version)
    .bind(&audit.image)
    .bind(audit.percent)
    .bind(&audit.by)
    .bind(&audit.panel_user)
    .execute(conn)
    .await?;
    Ok(())
}

async fn versions_in(conn: &mut PgConnection, kind: &str) -> Result<Vec<KindVersion>, sqlx::Error> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {VERSION_COLUMNS} FROM hallow.kind_versions WHERE kind = $1
         ORDER BY added_at DESC, version DESC"
    )))
    .bind(kind)
    .fetch_all(conn)
    .await
}

async fn channels_in(conn: &mut PgConnection, kind: &str) -> Result<KindChannels, sqlx::Error> {
    Ok(sqlx::query_as(
        "SELECT live, previous, canary, canary_percent, canary_since
         FROM hallow.kind_channels WHERE kind = $1",
    )
    .bind(kind)
    .fetch_optional(conn)
    .await?
    .unwrap_or_default())
}

async fn write_channels(
    conn: &mut PgConnection,
    kind: &str,
    channels: &KindChannels,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO hallow.kind_channels
           (kind, live, previous, canary, canary_percent, canary_since, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, now())
         ON CONFLICT (kind) DO UPDATE
           SET live = EXCLUDED.live, previous = EXCLUDED.previous, canary = EXCLUDED.canary,
               canary_percent = EXCLUDED.canary_percent, canary_since = EXCLUDED.canary_since,
               updated_at = now()",
    )
    .bind(kind)
    .bind(&channels.live)
    .bind(&channels.previous)
    .bind(&channels.canary)
    .bind(channels.canary_percent)
    .bind(channels.canary_since)
    .execute(conn)
    .await?;
    Ok(())
}

async fn lock_kind(conn: &mut PgConnection, kind: &str) -> Result<Option<ServerKind>, sqlx::Error> {
    sqlx::query_as(
        "SELECT name, spec, updated_at, updated_by FROM hallow.server_kinds
         WHERE name = $1 FOR UPDATE",
    )
    .bind(kind)
    .fetch_optional(conn)
    .await
}

/// Records a version and its `add` audit entry. A version already recorded
/// is left as it is.
pub async fn add_version(pool: &PgPool, new: &NewVersion<'_>) -> Result<AddedVersion, sqlx::Error> {
    let mut tx = pool.begin().await?;
    if lock_kind(&mut tx, new.kind).await?.is_none() {
        return Ok(AddedVersion::NoKind);
    }
    let added: Option<KindVersion> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "INSERT INTO hallow.kind_versions
           (kind, version, image, base, base_version, git_commit, added_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (kind, version) DO NOTHING
         RETURNING {VERSION_COLUMNS}"
    )))
    .bind(new.kind)
    .bind(new.version)
    .bind(new.image)
    .bind(new.base)
    .bind(new.base_version)
    .bind(new.git_commit)
    .bind(new.by)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(added) = added else {
        let existing: KindVersion = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT {VERSION_COLUMNS} FROM hallow.kind_versions WHERE kind = $1 AND version = $2"
        )))
        .bind(new.kind)
        .bind(new.version)
        .fetch_one(&mut *tx)
        .await?;
        return Ok(AddedVersion::Exists(existing));
    };
    write_audit(
        &mut tx,
        &NewVersionAudit {
            kind: new.kind.to_owned(),
            action: "add",
            version: Some(new.version.to_owned()),
            from_version: None,
            image: Some(new.image.to_owned()),
            percent: None,
            by: new.by.to_owned(),
            panel_user: new.panel_user.map(str::to_owned),
        },
    )
    .await?;
    tx.commit().await?;
    Ok(AddedVersion::Added(added))
}

/// A kind's versions (newest first) and channels, or `None` if there is no
/// such kind.
pub async fn kind_state(pool: &PgPool, kind: &str) -> Result<Option<KindState>, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let found: Option<ServerKind> = sqlx::query_as(
        "SELECT name, spec, updated_at, updated_by FROM hallow.server_kinds WHERE name = $1",
    )
    .bind(kind)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(found) = found else {
        return Ok(None);
    };
    Ok(Some(KindState {
        kind: found,
        channels: channels_in(&mut conn, kind).await?,
        versions: versions_in(&mut conn, kind).await?,
    }))
}

/// Every kind's live version and canary, for the instance planner.
pub async fn list_rollouts(pool: &PgPool) -> Result<Vec<Rollout>, sqlx::Error> {
    sqlx::query_as(
        "SELECT c.kind, c.live, c.canary, v.image AS canary_image, c.canary_percent
         FROM hallow.kind_channels c
         LEFT JOIN hallow.kind_versions v ON v.kind = c.kind AND v.version = c.canary
         ORDER BY c.kind",
    )
    .fetch_all(pool)
    .await
}

/// Changes a kind's channels: `decide` sees the kind, its channels and
/// versions with the kind's row locked, and says what to write (or why
/// not). Returns `Ok(None)` if there is no such kind.
pub async fn change_channels<E>(
    pool: &PgPool,
    kind: &str,
    decide: impl FnOnce(&KindState) -> Result<ChannelChange, E>,
) -> Result<Option<Result<KindState, E>>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let Some(found) = lock_kind(&mut tx, kind).await? else {
        return Ok(None);
    };
    let state = KindState {
        kind: found,
        channels: channels_in(&mut tx, kind).await?,
        versions: versions_in(&mut tx, kind).await?,
    };
    let change = match decide(&state) {
        Ok(change) => change,
        Err(err) => return Ok(Some(Err(err))),
    };
    let mut kind_row = state.kind.clone();
    if let Some(image) = &change.image {
        kind_row.spec.0.image.clone_from(image);
        kind_row.spec.0.updated_by = Some(change.audit.by.clone());
        kind_row = sqlx::query_as(
            "UPDATE hallow.server_kinds SET spec = $2, updated_by = $3, updated_at = now()
             WHERE name = $1
             RETURNING name, spec, updated_at, updated_by",
        )
        .bind(kind)
        .bind(&kind_row.spec)
        .bind(&change.audit.by)
        .fetch_one(&mut *tx)
        .await?;
    }
    write_channels(&mut tx, kind, &change.channels).await?;
    write_audit(&mut tx, &change.audit).await?;
    tx.commit().await?;
    Ok(Some(Ok(KindState {
        kind: kind_row,
        channels: change.channels,
        versions: state.versions,
    })))
}

/// Registers a kind or replaces it, like [`crate::put_kind`], and keeps its
/// channels in step: when the image changes, `live` becomes the recorded
/// version with that image (or `None`), the old live version becomes
/// `previous`, and a `register` audit entry says who did it.
pub async fn register_kind(
    pool: &PgPool,
    name: &str,
    spec: &KindSpec,
) -> Result<ServerKind, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let before = lock_kind(&mut tx, name).await?;
    let stored: ServerKind = sqlx::query_as(
        "INSERT INTO hallow.server_kinds (name, spec, updated_by) VALUES ($1, $2, $3)
         ON CONFLICT (name) DO UPDATE
           SET spec = EXCLUDED.spec, updated_by = EXCLUDED.updated_by, updated_at = now()
         RETURNING name, spec, updated_at, updated_by",
    )
    .bind(name)
    .bind(Json(spec))
    .bind(spec.updated_by.as_deref())
    .fetch_one(&mut *tx)
    .await?;
    if before
        .as_ref()
        .is_some_and(|b| b.spec.0.image == spec.image)
    {
        tx.commit().await?;
        return Ok(stored);
    }
    let old = channels_in(&mut tx, name).await?;
    let found: Option<String> = sqlx::query_scalar(
        "SELECT version FROM hallow.kind_versions WHERE kind = $1 AND image = $2
         ORDER BY added_at DESC LIMIT 1",
    )
    .bind(name)
    .bind(&spec.image)
    .fetch_optional(&mut *tx)
    .await?;
    let mut channels = old.clone();
    channels.live.clone_from(&found);
    if old.live.is_some() && old.live != found {
        channels.previous.clone_from(&old.live);
    }
    if found.is_some() && old.canary == found {
        channels.canary = None;
        channels.canary_percent = None;
        channels.canary_since = None;
    }
    if channels != old {
        write_channels(&mut tx, name, &channels).await?;
    }
    write_audit(
        &mut tx,
        &NewVersionAudit {
            kind: name.to_owned(),
            action: "register",
            version: found,
            from_version: old.live,
            image: Some(spec.image.clone()),
            percent: None,
            by: spec.updated_by.clone().unwrap_or_else(|| "api".into()),
            panel_user: None,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(stored)
}

/// Audit entries, newest first, for one kind or all.
pub async fn version_audit(
    pool: &PgPool,
    kind: Option<&str>,
    limit: i64,
) -> Result<Vec<VersionAudit>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, kind, action, version, from_version, image, percent, by_name, panel_user, at
         FROM hallow.kind_version_audit
         WHERE $1::text IS NULL OR kind = $1
         ORDER BY at DESC, id DESC
         LIMIT $2",
    )
    .bind(kind)
    .bind(limit)
    .fetch_all(pool)
    .await
}
