//! Anticheat violations and verdicts (docs/ANTICHEAT.md, phase 2).
//!
//! Checks run where their data is: on the proxy for packet timing, on the
//! server for movement and combat (see `hallow-anticheat`). Each failed check
//! is a [`Violation`] with its evidence, sent to the control plane's verdict
//! service. The verdict service scores violations per player and check, with
//! decay, and records the action it would take. In observe-only mode it never
//! takes one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `POST /v1/violations`: one batch from a proxy or server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViolationBatch {
    /// The proxy id or server name that ran the checks.
    pub source: String,
    pub violations: Vec<Violation>,
}

/// One failed check for one player.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Violation {
    pub player: Uuid,
    /// When the check ran, Unix milliseconds.
    pub at_ms: u64,
    /// The check's name, such as `speed`, `fly`, `reach`, `click_rate` or
    /// `timer`.
    pub check: String,
    /// How bad it was: the measured value over its limit, so `1.0` is just
    /// past it and `2.0` is twice the limit. Scores add these up.
    pub severity: f64,
    /// The client's real protocol version, when the source knows it.
    pub protocol: Option<i32>,
    /// One line for staff, such as `horizontal speed 21.4 blocks/s, limit 10`.
    pub summary: String,
    /// The measurements and limits behind it.
    pub evidence: BTreeMap<String, f64>,
}

/// What the verdict service would do about a player's score on one check.
/// Ordered from mildest to harshest.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum VerdictAction {
    #[default]
    None,
    /// Flag the player for staff to look at.
    Review,
    /// Move the player back (movement checks).
    Setback,
    Kick,
    /// A temporary ban, after staff review.
    Ban,
}

impl VerdictAction {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Review => "review",
            Self::Setback => "setback",
            Self::Kick => "kick",
            Self::Ban => "ban",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "none" => Self::None,
            "review" => Self::Review,
            "setback" => Self::Setback,
            "kick" => Self::Kick,
            "ban" => Self::Ban,
            _ => return None,
        })
    }
}

/// Whether verdicts are carried out. Phase 2 only observes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerdictMode {
    /// Verdicts are recorded, never carried out.
    #[default]
    Observe,
    /// Verdicts are carried out (phase 3).
    Enforce,
}

/// `GET /v1/violations?name=`: a player's anticheat standing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerViolations {
    pub player: Uuid,
    pub mode: VerdictMode,
    /// The current score on each check the player has failed, decayed to now.
    pub scores: Vec<CheckScore>,
    /// Verdicts, newest first.
    pub verdicts: Vec<VerdictRecord>,
    /// Recent violations, newest first.
    pub violations: Vec<ViolationRecord>,
}

/// A player's score on one check.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CheckScore {
    pub check: String,
    /// The decayed sum of severities.
    pub level: f64,
    /// What that level calls for.
    pub action: VerdictAction,
    /// When the last violation came in, Unix milliseconds.
    pub updated_ms: u64,
}

/// A verdict: the score on a check reached a level that calls for an action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerdictRecord {
    pub check: String,
    pub action: VerdictAction,
    pub level: f64,
    pub at_ms: u64,
    /// False in observe-only mode: recorded, not carried out.
    pub enforced: bool,
    /// The violation that tipped it, for staff.
    pub reason: String,
}

/// One stored violation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViolationRecord {
    pub source: String,
    pub check: String,
    pub at_ms: u64,
    pub severity: f64,
    pub protocol: Option<i32>,
    pub summary: String,
    pub evidence: BTreeMap<String, f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_are_ordered_and_round_trip() {
        assert!(VerdictAction::None < VerdictAction::Review);
        assert!(VerdictAction::Kick < VerdictAction::Ban);
        for action in [
            VerdictAction::None,
            VerdictAction::Review,
            VerdictAction::Setback,
            VerdictAction::Kick,
            VerdictAction::Ban,
        ] {
            assert_eq!(VerdictAction::parse(action.as_str()), Some(action));
            assert_eq!(
                serde_json::to_string(&action).unwrap(),
                format!("\"{}\"", action.as_str())
            );
        }
        assert_eq!(VerdictAction::parse("smite"), None);
    }
}
