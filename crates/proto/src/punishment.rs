//! Bans, mutes, kicks and warnings, issued in game by a player with a staff
//! rank, by panel staff, or by the console (the anticheat later). They're
//! kept in PostgreSQL, so these endpoints need a control plane with a
//! database.

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
    /// Set when a panel staff member issues it; then `issuer` is `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staff: Option<StaffMember>,
}

/// A staff member acting through the panel rather than in game.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaffMember {
    /// The panel username, shown as the issuer: 1 to 64 characters.
    pub name: String,
    /// The panel user's id, kept for the audit trail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel_user: Option<String>,
}

/// `POST /v1/punishments/revoke`: lift the target's active ban or mute.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokePunishment {
    /// [`PunishmentKind::Ban`] or [`PunishmentKind::Mute`].
    pub kind: PunishmentKind,
    pub target: String,
    pub issuer: Option<Uuid>,
    pub reason: Option<String>,
    /// Set when a panel staff member lifts it; then `issuer` is `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staff: Option<StaffMember>,
}

/// The answer to an issue or revoke, shown to whoever acted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PunishmentReply {
    /// False if refused; `message` says why.
    pub ok: bool,
    /// With legacy `§` colour codes, for chat.
    pub message: String,
    /// The punishment issued or lifted, when `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
}

/// An active mute, as a proxy needs it to block chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MuteInfo {
    pub reason: String,
    /// Unix milliseconds; `None` is permanent.
    pub until_ms: Option<u64>,
}

/// A banned player's ban, as the banned lobby shows it and the proxy needs
/// it to keep them there: `GET /v1/bans/{uuid}`, and carried by a banned
/// login and [`crate::command::ProxyCommand::Restrict`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BanInfo {
    /// The punishment id, for appeals.
    pub id: i64,
    pub reason: String,
    /// Who issued it, as the panel shows it: a player's username, a panel
    /// staff member's name, or `console`, `system` or `anticheat`.
    pub issuer: String,
    pub issuer_kind: IssuerKind,
    pub created_ms: u64,
    /// Unix milliseconds; `None` is permanent.
    pub expires_ms: Option<u64>,
    /// How to appeal (`HALLOW_APPEAL_TEXT`), legacy `§` text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appeal: Option<String>,
    /// Where to appeal (`HALLOW_APPEAL_URL`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appeal_url: Option<String>,
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

/// Who issued or lifted a punishment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssuerKind {
    /// A player with a staff rank, in game.
    Player,
    /// A panel staff member.
    Staff,
    Console,
    System,
    Anticheat,
}

/// One line of [`PunishmentPage`]: a punishment with its target and whole
/// story, for the panel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PunishmentEntry {
    pub id: i64,
    pub kind: PunishmentKind,
    pub target: Uuid,
    /// The target's current username.
    pub target_name: String,
    pub reason: String,
    /// The player's username, the staff member's panel name, or `console`,
    /// `system` or `anticheat`.
    pub issuer: String,
    pub issuer_kind: IssuerKind,
    pub created_ms: u64,
    pub expires_ms: Option<u64>,
    /// In force now. Kicks and warnings never are.
    pub active: bool,
    pub revoked: Option<RevocationInfo>,
}

/// When, by whom and why a punishment was lifted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationInfo {
    pub at_ms: u64,
    pub by: String,
    pub by_kind: IssuerKind,
    pub reason: Option<String>,
}

/// `GET /v1/punishments/search`: every filter is optional.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PunishmentSearch {
    /// The target's username, matched like a punishment target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<PunishmentKind>,
    /// Only those in force (`true`) or not (`false`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    /// Page size, 1 to [`SEARCH_MAX`](crate::routes::PUNISHMENT_SEARCH_MAX);
    /// [`SEARCH_DEFAULT`](crate::routes::PUNISHMENT_SEARCH_DEFAULT) if left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

/// A page of punishments, newest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PunishmentPage {
    /// How many match, over every page.
    pub total: u64,
    pub entries: Vec<PunishmentEntry>,
}

/// A reason staff pick instead of typing one, such as "Cheating" for a
/// 30-day ban.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PunishmentTemplate {
    pub id: i64,
    pub name: String,
    pub kind: PunishmentKind,
    /// `None` is permanent; always `None` for kicks and warnings.
    pub duration_seconds: Option<u64>,
    pub reason: String,
    pub updated_ms: u64,
}

/// `POST /v1/punishments/templates` and `PUT /v1/punishments/templates/{id}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveTemplate {
    /// 1 to 64 characters, unique without case.
    pub name: String,
    pub kind: PunishmentKind,
    pub duration_seconds: Option<u64>,
    /// 1 to 256 characters.
    pub reason: String,
}
