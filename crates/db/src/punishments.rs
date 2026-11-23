//! Bans, mutes, kicks and warnings.

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::decode_error;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PunishmentKind {
    Ban,
    Mute,
    /// Takes effect once; never has an expiry.
    Kick,
    Warn,
}

impl PunishmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ban => "ban",
            Self::Mute => "mute",
            Self::Kick => "kick",
            Self::Warn => "warn",
        }
    }

    fn parse(value: &str) -> Result<Self, sqlx::Error> {
        match value {
            "ban" => Ok(Self::Ban),
            "mute" => Ok(Self::Mute),
            "kick" => Ok(Self::Kick),
            "warn" => Ok(Self::Warn),
            other => Err(decode_error("punishment kind", other)),
        }
    }
}

/// Who issued or revoked a punishment.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Actor {
    /// A staff member's account, in game.
    Account(Uuid),
    /// A staff member in the panel: their panel username, and the panel
    /// user's id when known.
    Staff {
        name: String,
        panel_user: Option<String>,
    },
    /// The server console.
    Console,
    /// Automation, such as a scheduled job.
    System,
    /// The anticheat verdict service.
    Anticheat,
}

impl Actor {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Account(_) => "account",
            Self::Staff { .. } => "staff",
            Self::Console => "console",
            Self::System => "system",
            Self::Anticheat => "anticheat",
        }
    }

    pub(crate) fn uuid(&self) -> Option<Uuid> {
        match self {
            Self::Account(uuid) => Some(*uuid),
            _ => None,
        }
    }

    pub(crate) fn name(&self) -> Option<&str> {
        match self {
            Self::Staff { name, .. } => Some(name),
            _ => None,
        }
    }

    pub(crate) fn panel_user(&self) -> Option<&str> {
        match self {
            Self::Staff { panel_user, .. } => panel_user.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn parse(
        kind: &str,
        uuid: Option<Uuid>,
        name: Option<String>,
        panel_user: Option<String>,
    ) -> Result<Self, sqlx::Error> {
        if kind == "staff" {
            return match name {
                Some(name) => Ok(Self::Staff { name, panel_user }),
                None => Err(decode_error("actor", "staff without a name")),
            };
        }
        match (kind, uuid) {
            ("account", Some(uuid)) => Ok(Self::Account(uuid)),
            ("console", None) => Ok(Self::Console),
            ("system", None) => Ok(Self::System),
            ("anticheat", None) => Ok(Self::Anticheat),
            (other, _) => Err(decode_error("actor", other)),
        }
    }
}

/// A punishment to record.
#[derive(Clone, Debug)]
pub struct NewPunishment<'a> {
    pub kind: PunishmentKind,
    /// Must be an account that has joined.
    pub target: Uuid,
    pub issuer: Actor,
    pub reason: &'a str,
    /// `None` is permanent. Must be in the future, and `None` for a kick.
    pub expires_at: Option<DateTime<Utc>>,
}

/// When, by whom and why a punishment was lifted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revocation {
    pub at: DateTime<Utc>,
    pub by: Actor,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Punishment {
    pub id: i64,
    pub kind: PunishmentKind,
    pub target: Uuid,
    pub issuer: Actor,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    /// `None` is permanent.
    pub expires_at: Option<DateTime<Utc>>,
    pub revoked: Option<Revocation>,
}

impl Punishment {
    /// Whether it is in force at `now`: not revoked and not expired.
    pub fn is_active_at(&self, now: DateTime<Utc>) -> bool {
        self.revoked.is_none() && self.expires_at.is_none_or(|expires| expires > now)
    }
}

#[derive(FromRow)]
struct PunishmentRow {
    id: i64,
    kind: String,
    target_uuid: Uuid,
    issuer_kind: String,
    issuer_uuid: Option<Uuid>,
    issuer_name: Option<String>,
    issuer_panel_user: Option<String>,
    reason: String,
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    revoked_by_kind: Option<String>,
    revoked_by_uuid: Option<Uuid>,
    revoked_by_name: Option<String>,
    revoked_by_panel_user: Option<String>,
    revoked_reason: Option<String>,
}

impl TryFrom<PunishmentRow> for Punishment {
    type Error = sqlx::Error;

    fn try_from(row: PunishmentRow) -> Result<Self, Self::Error> {
        let revoked = match (row.revoked_at, row.revoked_by_kind) {
            (Some(at), Some(kind)) => Some(Revocation {
                at,
                by: Actor::parse(
                    &kind,
                    row.revoked_by_uuid,
                    row.revoked_by_name,
                    row.revoked_by_panel_user,
                )?,
                reason: row.revoked_reason,
            }),
            _ => None,
        };
        Ok(Self {
            id: row.id,
            kind: PunishmentKind::parse(&row.kind)?,
            target: row.target_uuid,
            issuer: Actor::parse(
                &row.issuer_kind,
                row.issuer_uuid,
                row.issuer_name,
                row.issuer_panel_user,
            )?,
            reason: row.reason,
            created_at: row.created_at,
            expires_at: row.expires_at,
            revoked,
        })
    }
}

macro_rules! columns {
    () => {
        "id, kind, target_uuid, issuer_kind, issuer_uuid, issuer_name, issuer_panel_user, reason, \
         created_at, expires_at, revoked_at, revoked_by_kind, revoked_by_uuid, revoked_by_name, \
         revoked_by_panel_user, revoked_reason"
    };
}

/// Records a punishment. Enforcing it (disconnecting a banned player,
/// blocking a muted player's chat) is the caller's job.
pub async fn add_punishment(
    pool: &PgPool,
    new: &NewPunishment<'_>,
) -> Result<Punishment, sqlx::Error> {
    let row: PunishmentRow = sqlx::query_as(concat!(
        "INSERT INTO hallow.punishments
            (kind, target_uuid, issuer_kind, issuer_uuid, issuer_name, issuer_panel_user, reason, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
         RETURNING ", columns!(), ""
    ))
    .bind(new.kind.as_str())
    .bind(new.target)
    .bind(new.issuer.kind())
    .bind(new.issuer.uuid())
    .bind(new.issuer.name())
    .bind(new.issuer.panel_user())
    .bind(new.reason)
    .bind(new.expires_at)
    .fetch_one(pool)
    .await?;
    row.try_into()
}

/// One punishment by id.
pub async fn get_punishment(pool: &PgPool, id: i64) -> Result<Option<Punishment>, sqlx::Error> {
    sqlx::query_as::<_, PunishmentRow>(concat!(
        "SELECT ",
        columns!(),
        " FROM hallow.punishments WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?
    .map(Punishment::try_from)
    .transpose()
}

/// Lifts a punishment. Returns the updated punishment, or `None` if there is
/// no punishment with that id or it was already revoked.
pub async fn revoke_punishment(
    pool: &PgPool,
    id: i64,
    by: &Actor,
    reason: Option<&str>,
) -> Result<Option<Punishment>, sqlx::Error> {
    sqlx::query_as::<_, PunishmentRow>(concat!(
        "UPDATE hallow.punishments
         SET revoked_at = greatest(now(), created_at), revoked_by_kind = $2, revoked_by_uuid = $3,
             revoked_by_name = $4, revoked_by_panel_user = $5, revoked_reason = $6
         WHERE id = $1 AND revoked_at IS NULL
         RETURNING ",
        columns!(),
        ""
    ))
    .bind(id)
    .bind(by.kind())
    .bind(by.uuid())
    .bind(by.name())
    .bind(by.panel_user())
    .bind(reason)
    .fetch_optional(pool)
    .await?
    .map(Punishment::try_from)
    .transpose()
}

async fn active(
    pool: &PgPool,
    target: Uuid,
    kind: PunishmentKind,
) -> Result<Option<Punishment>, sqlx::Error> {
    // Served by punishments_active_idx. A permanent one wins over any
    // temporary one, then the one that lasts longest.
    sqlx::query_as::<_, PunishmentRow>(concat!(
        "SELECT ",
        columns!(),
        " FROM hallow.punishments
         WHERE target_uuid = $1 AND kind = $2 AND revoked_at IS NULL
           AND (expires_at IS NULL OR expires_at > now())
         ORDER BY expires_at DESC NULLS FIRST, id DESC
         LIMIT 1"
    ))
    .bind(target)
    .bind(kind.as_str())
    .fetch_optional(pool)
    .await?
    .map(Punishment::try_from)
    .transpose()
}

/// The ban in force on the account, if any (the longest, if several).
pub async fn active_ban(pool: &PgPool, target: Uuid) -> Result<Option<Punishment>, sqlx::Error> {
    active(pool, target, PunishmentKind::Ban).await
}

/// The mute in force on the account, if any (the longest, if several).
pub async fn active_mute(pool: &PgPool, target: Uuid) -> Result<Option<Punishment>, sqlx::Error> {
    active(pool, target, PunishmentKind::Mute).await
}

/// The account's punishments of every kind, revoked and expired included,
/// newest first, at most `limit`.
pub async fn punishment_history(
    pool: &PgPool,
    target: Uuid,
    limit: i64,
) -> Result<Vec<Punishment>, sqlx::Error> {
    sqlx::query_as::<_, PunishmentRow>(concat!(
        "SELECT ",
        columns!(),
        " FROM hallow.punishments
         WHERE target_uuid = $1
         ORDER BY created_at DESC, id DESC
         LIMIT $2"
    ))
    .bind(target)
    .bind(limit)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(Punishment::try_from)
    .collect()
}

/// Filters for [`search_punishments`]; `None` matches everything.
#[derive(Clone, Debug, Default)]
pub struct PunishmentFilter {
    /// Only this account's.
    pub target: Option<Uuid>,
    pub kind: Option<PunishmentKind>,
    /// In force now (an unrevoked, unexpired ban or mute), or not.
    pub active: Option<bool>,
}

/// A punishment with its target's current username.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PunishmentWithTarget {
    pub punishment: Punishment,
    pub target_name: String,
}

#[derive(FromRow)]
struct SearchRow {
    #[sqlx(flatten)]
    punishment: PunishmentRow,
    target_name: String,
    total: i64,
}

/// Punishments matching `filter`, newest first, `limit` from `offset`, and
/// how many match in all.
pub async fn search_punishments(
    pool: &PgPool,
    filter: &PunishmentFilter,
    limit: i64,
    offset: i64,
) -> Result<(u64, Vec<PunishmentWithTarget>), sqlx::Error> {
    let rows: Vec<SearchRow> = sqlx::query_as(
        "SELECT p.id, p.kind, p.target_uuid, p.issuer_kind, p.issuer_uuid, p.issuer_name,
                p.issuer_panel_user, p.reason, p.created_at, p.expires_at, p.revoked_at,
                p.revoked_by_kind, p.revoked_by_uuid, p.revoked_by_name, p.revoked_by_panel_user,
                p.revoked_reason, a.username AS target_name, count(*) OVER () AS total
         FROM hallow.punishments p
         JOIN hallow.accounts a ON a.uuid = p.target_uuid
         WHERE ($1::uuid IS NULL OR p.target_uuid = $1)
           AND ($2::text IS NULL OR p.kind = $2)
           AND ($3::boolean IS NULL OR $3 = (
                p.kind IN ('ban', 'mute') AND p.revoked_at IS NULL
                AND (p.expires_at IS NULL OR p.expires_at > now())))
         ORDER BY p.created_at DESC, p.id DESC
         LIMIT $4 OFFSET $5",
    )
    .bind(filter.target)
    .bind(filter.kind.map(PunishmentKind::as_str))
    .bind(filter.active)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;
    let total = match rows.first() {
        Some(row) => u64::try_from(row.total).unwrap_or(0),
        // Past the last page the window has no rows to count on.
        None if offset > 0 => {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM hallow.punishments p
                 WHERE ($1::uuid IS NULL OR p.target_uuid = $1)
                   AND ($2::text IS NULL OR p.kind = $2)
                   AND ($3::boolean IS NULL OR $3 = (
                        p.kind IN ('ban', 'mute') AND p.revoked_at IS NULL
                        AND (p.expires_at IS NULL OR p.expires_at > now())))",
            )
            .bind(filter.target)
            .bind(filter.kind.map(PunishmentKind::as_str))
            .bind(filter.active)
            .fetch_one(pool)
            .await?;
            u64::try_from(count).unwrap_or(0)
        }
        None => 0,
    };
    let entries = rows
        .into_iter()
        .map(|row| {
            Ok(PunishmentWithTarget {
                punishment: row.punishment.try_into()?,
                target_name: row.target_name,
            })
        })
        .collect::<Result<_, sqlx::Error>>()?;
    Ok((total, entries))
}

/// A reason staff pick from, with the kind and length it comes with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PunishmentTemplate {
    pub id: i64,
    pub name: String,
    pub kind: PunishmentKind,
    /// `None` is permanent.
    pub duration_seconds: Option<i64>,
    pub reason: String,
    pub updated_at: DateTime<Utc>,
}

/// A template to create or replace.
#[derive(Clone, Debug)]
pub struct NewTemplate<'a> {
    pub name: &'a str,
    pub kind: PunishmentKind,
    /// Positive; `None` for kicks and warnings.
    pub duration_seconds: Option<i64>,
    pub reason: &'a str,
}

/// Why a template wasn't saved.
#[derive(Debug, thiserror::Error)]
pub enum TemplateError {
    #[error("another template is called that")]
    NameTaken,
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

#[derive(FromRow)]
struct TemplateRow {
    id: i64,
    name: String,
    kind: String,
    duration_seconds: Option<i64>,
    reason: String,
    updated_at: DateTime<Utc>,
}

impl TryFrom<TemplateRow> for PunishmentTemplate {
    type Error = sqlx::Error;

    fn try_from(row: TemplateRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            name: row.name,
            kind: PunishmentKind::parse(&row.kind)?,
            duration_seconds: row.duration_seconds,
            reason: row.reason,
            updated_at: row.updated_at,
        })
    }
}

fn template_error(err: sqlx::Error) -> TemplateError {
    match &err {
        sqlx::Error::Database(db) if db.constraint() == Some("punishment_templates_name_idx") => {
            TemplateError::NameTaken
        }
        _ => TemplateError::Database(err),
    }
}

/// Every template, by name.
pub async fn list_templates(pool: &PgPool) -> Result<Vec<PunishmentTemplate>, sqlx::Error> {
    sqlx::query_as::<_, TemplateRow>(
        "SELECT id, name, kind, duration_seconds, reason, updated_at
         FROM hallow.punishment_templates ORDER BY lower(name)",
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(PunishmentTemplate::try_from)
    .collect()
}

pub async fn add_template(
    pool: &PgPool,
    new: &NewTemplate<'_>,
) -> Result<PunishmentTemplate, TemplateError> {
    let row: TemplateRow = sqlx::query_as(
        "INSERT INTO hallow.punishment_templates (name, kind, duration_seconds, reason)
         VALUES ($1, $2, $3, $4)
         RETURNING id, name, kind, duration_seconds, reason, updated_at",
    )
    .bind(new.name)
    .bind(new.kind.as_str())
    .bind(new.duration_seconds)
    .bind(new.reason)
    .fetch_one(pool)
    .await
    .map_err(template_error)?;
    Ok(row.try_into()?)
}

/// Replaces a template; `None` if there's none with that id.
pub async fn update_template(
    pool: &PgPool,
    id: i64,
    new: &NewTemplate<'_>,
) -> Result<Option<PunishmentTemplate>, TemplateError> {
    let row: Option<TemplateRow> = sqlx::query_as(
        "UPDATE hallow.punishment_templates
         SET name = $2, kind = $3, duration_seconds = $4, reason = $5, updated_at = now()
         WHERE id = $1
         RETURNING id, name, kind, duration_seconds, reason, updated_at",
    )
    .bind(id)
    .bind(new.name)
    .bind(new.kind.as_str())
    .bind(new.duration_seconds)
    .bind(new.reason)
    .fetch_optional(pool)
    .await
    .map_err(template_error)?;
    Ok(row.map(PunishmentTemplate::try_from).transpose()?)
}

/// Deletes a template; whether it existed.
pub async fn delete_template(pool: &PgPool, id: i64) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM hallow.punishment_templates WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
