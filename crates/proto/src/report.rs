//! Player reports: `/report <player> <reason>` in game, worked through by
//! staff in the panel. Kept in PostgreSQL, so these endpoints need a control
//! plane with a database.
//!
//! A report is `open` until a staff member claims it (`claimed`), and ends
//! `resolved`, optionally naming the punishment it led to, or `dismissed`.
//! A closed report can't change again.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::punishment::{IssuerKind, StaffMember};

/// Where a report is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    /// Waiting for staff.
    Open,
    /// A staff member is looking at it.
    Claimed,
    Resolved,
    Dismissed,
}

impl ReportStatus {
    /// Resolved or dismissed: it can't change again.
    #[must_use]
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Resolved | Self::Dismissed)
    }
}

/// `GET /v1/reports?status=`: one status, or `active` (open and claimed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatusFilter {
    Open,
    Claimed,
    Resolved,
    Dismissed,
    /// Open or claimed.
    Active,
}

/// `POST /v1/reports`, sent by a proxy for `/report`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateReport {
    /// The reporting player. Must have joined.
    pub reporter: Uuid,
    /// The reported player's username, online or not; matched without case,
    /// the most recently seen account first.
    pub target: String,
    /// 1 to 256 characters.
    pub reason: String,
}

/// The answer to [`CreateReport`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportCreated {
    pub id: i64,
    /// For the reporter, with legacy `§` colour codes.
    pub message: String,
}

/// A player in a report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportPlayer {
    pub uuid: Uuid,
    /// Their current username.
    pub name: String,
}

/// Who claimed or closed a report: the same actors as a punishment's issuer
/// ([`PunishmentEntry::issuer`](crate::punishment::PunishmentEntry) and
/// `issuer_kind`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportActor {
    /// The player's username, the staff member's panel name, or `console`,
    /// `system` or `anticheat`.
    pub name: String,
    pub kind: IssuerKind,
    /// The player's UUID, for [`IssuerKind::Player`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uuid: Option<Uuid>,
    /// The panel user's id, for [`IssuerKind::Staff`] when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel_user: Option<String>,
}

/// One report, with its whole story.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub id: i64,
    pub reporter: ReportPlayer,
    pub target: ReportPlayer,
    pub reason: String,
    /// The server the reporter was on.
    pub server: Option<String>,
    pub status: ReportStatus,
    pub created_ms: u64,
    /// Who claimed it last; kept once it's closed.
    pub claimed_by: Option<ReportActor>,
    pub claimed_ms: Option<u64>,
    /// Who resolved or dismissed it.
    pub closed_by: Option<ReportActor>,
    pub closed_ms: Option<u64>,
    /// The resolution or dismissal note.
    pub note: Option<String>,
    /// The punishment a resolved report led to.
    pub punishment_id: Option<i64>,
    /// How many reports the target has had in all, this one included.
    pub target_reports: u64,
}

/// `GET /v1/reports`: every filter is optional.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportSearch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ReportStatusFilter>,
    /// The reported player's username, matched like a report target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Page size, 1 to [`REPORTS_MAX`](crate::routes::REPORTS_MAX);
    /// [`REPORTS_DEFAULT`](crate::routes::REPORTS_DEFAULT) if left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

/// A page of reports, newest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportPage {
    pub reports: Vec<Report>,
    /// How many match, over every page.
    pub total: u64,
}

/// `POST /v1/reports/{id}/claim`. The actor is given like a punishment's
/// issuer: `staff` for panel staff, else `issuer` for an in-game staff
/// player, else the console.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staff: Option<StaffMember>,
}

/// `POST /v1/reports/{id}/resolve` and `POST /v1/reports/{id}/dismiss`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseReport {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staff: Option<StaffMember>,
    /// Why, up to 1024 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Resolve only: the punishment it led to, which must be of the
    /// report's target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub punishment_id: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reports_serialize_with_nulls() {
        let report = Report {
            id: 3,
            reporter: ReportPlayer {
                uuid: Uuid::nil(),
                name: "alice".into(),
            },
            target: ReportPlayer {
                uuid: Uuid::max(),
                name: "bob".into(),
            },
            reason: "flying".into(),
            server: Some("skywars-1".into()),
            status: ReportStatus::Claimed,
            created_ms: 10,
            claimed_by: Some(ReportActor {
                name: "Mod".into(),
                kind: IssuerKind::Staff,
                uuid: None,
                panel_user: Some("7".into()),
            }),
            claimed_ms: Some(11),
            closed_by: None,
            closed_ms: None,
            note: None,
            punishment_id: None,
            target_reports: 2,
        };
        let value = serde_json::to_value(&report).unwrap();
        assert_eq!(value["status"], json!("claimed"));
        assert_eq!(
            value["claimed_by"],
            json!({"name": "Mod", "kind": "staff", "panel_user": "7"})
        );
        assert_eq!(value["closed_by"], json!(null));
        assert_eq!(value["punishment_id"], json!(null));
        assert_eq!(serde_json::from_value::<Report>(value).unwrap(), report);
        let search: ReportSearch = serde_json::from_value(json!({"status": "active"})).unwrap();
        assert_eq!(search.status, Some(ReportStatusFilter::Active));
        let claim: ClaimReport =
            serde_json::from_value(json!({"staff": {"name": "Mod", "panel_user": "7"}})).unwrap();
        assert_eq!(claim.staff.unwrap().name, "Mod");
    }
}
