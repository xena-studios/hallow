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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Actor {
    /// A staff member's account.
    Account(Uuid),
    /// The server console.
    Console,
    /// Automation, such as a scheduled job.
    System,
    /// The anticheat verdict service.
    Anticheat,
}

impl Actor {
    fn kind(self) -> &'static str {
        match self {
            Self::Account(_) => "account",
            Self::Console => "console",
            Self::System => "system",
            Self::Anticheat => "anticheat",
        }
    }

    fn uuid(self) -> Option<Uuid> {
        match self {
            Self::Account(uuid) => Some(uuid),
            _ => None,
        }
    }

    fn parse(kind: &str, uuid: Option<Uuid>) -> Result<Self, sqlx::Error> {
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
    reason: String,
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    revoked_by_kind: Option<String>,
    revoked_by_uuid: Option<Uuid>,
    revoked_reason: Option<String>,
}

impl TryFrom<PunishmentRow> for Punishment {
    type Error = sqlx::Error;

    fn try_from(row: PunishmentRow) -> Result<Self, Self::Error> {
        let revoked = match (row.revoked_at, row.revoked_by_kind) {
            (Some(at), Some(kind)) => Some(Revocation {
                at,
                by: Actor::parse(&kind, row.revoked_by_uuid)?,
                reason: row.revoked_reason,
            }),
            _ => None,
        };
        Ok(Self {
            id: row.id,
            kind: PunishmentKind::parse(&row.kind)?,
            target: row.target_uuid,
            issuer: Actor::parse(&row.issuer_kind, row.issuer_uuid)?,
            reason: row.reason,
            created_at: row.created_at,
            expires_at: row.expires_at,
            revoked,
        })
    }
}

macro_rules! columns {
    () => {
        "id, kind, target_uuid, issuer_kind, issuer_uuid, reason, created_at, expires_at, \
                       revoked_at, revoked_by_kind, revoked_by_uuid, revoked_reason"
    };
}

/// Records a punishment. Enforcing it (disconnecting a banned player,
/// blocking a muted player's chat) is the caller's job.
pub async fn add_punishment(
    pool: &PgPool,
    new: &NewPunishment<'_>,
) -> Result<Punishment, sqlx::Error> {
    let row: PunishmentRow = sqlx::query_as(concat!(
        "INSERT INTO hallow.punishments (kind, target_uuid, issuer_kind, issuer_uuid, reason, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING ", columns!(), ""
    ))
    .bind(new.kind.as_str())
    .bind(new.target)
    .bind(new.issuer.kind())
    .bind(new.issuer.uuid())
    .bind(new.reason)
    .bind(new.expires_at)
    .fetch_one(pool)
    .await?;
    row.try_into()
}

/// Lifts a punishment. Returns the updated punishment, or `None` if there is
/// no punishment with that id or it was already revoked.
pub async fn revoke_punishment(
    pool: &PgPool,
    id: i64,
    by: Actor,
    reason: Option<&str>,
) -> Result<Option<Punishment>, sqlx::Error> {
    sqlx::query_as::<_, PunishmentRow>(concat!(
        "UPDATE hallow.punishments
         SET revoked_at = greatest(now(), created_at), revoked_by_kind = $2, revoked_by_uuid = $3, revoked_reason = $4
         WHERE id = $1 AND revoked_at IS NULL
         RETURNING ", columns!(), ""
    ))
    .bind(id)
    .bind(by.kind())
    .bind(by.uuid())
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
