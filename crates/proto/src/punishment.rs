//! Bans, mutes, kicks and warnings issued in game (or by the panel and the
//! anticheat later). They're kept in PostgreSQL, so these endpoints need a
//! control plane with a database.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PunishmentKind {
    Ban,
    Mute,
    Kick,
    Warn,
}

/// `POST /v1/punishments`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuePunishment {
    pub kind: PunishmentKind,
    /// The target's username, online or not; matched without case, the most
    /// recently seen account first.
    pub target: String,
    /// The staff member issuing it; `None` for the console.
    pub issuer: Option<Uuid>,
    pub reason: String,
    /// How long it lasts; `None` is permanent. Ignored for kicks and warnings.
    pub duration_seconds: Option<u64>,
}

/// `POST /v1/punishments/revoke`: lift the target's active ban or mute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokePunishment {
    /// [`PunishmentKind::Ban`] or [`PunishmentKind::Mute`].
    pub kind: PunishmentKind,
    pub target: String,
    pub issuer: Option<Uuid>,
    pub reason: Option<String>,
}

/// The answer to an issue or revoke, shown to whoever acted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PunishmentReply {
    /// False if refused; `message` says why.
    pub ok: bool,
    pub message: String,
}

/// An active mute, as a proxy needs it to block chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuteInfo {
    pub reason: String,
    /// Unix milliseconds; `None` is permanent.
    pub until_ms: Option<u64>,
}

/// One line of `GET /v1/punishments?name=`, newest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PunishmentRecord {
    pub id: i64,
    pub kind: PunishmentKind,
    pub reason: String,
    /// The issuer's username, or `console`, `system` or `anticheat`.
    pub issuer: String,
    pub created_ms: u64,
    pub expires_ms: Option<u64>,
    /// Revoked, or expired.
    pub active: bool,
}
