//! Versions of a server kind and its `live` and `canary` channels
//! (docs/SERVERS.md#versions, docs/INSTANCES.md#versions).
//!
//! A version is an image built from a tag on the kind's repo. `live` is what
//! new instances start on; `canary` optionally takes a share of new
//! instances. Promoting, canarying and rolling back are API actions, each
//! recorded in the audit log ([`VersionAuditEntry`]) with who did it.
//! Running instances keep the version they started on.

use serde::{Deserialize, Serialize};

/// `POST /v1/kinds/{name}/versions`: records a version CI published. Adding
/// the same version with the same image again is a no-op, so CI can retry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddVersion {
    /// Its name, normally the image tag: `1.4.0`, or `sha-3f2a9c1` for a dev
    /// build. Up to 64 letters, digits, `.`, `_`, `+` and `-`.
    pub version: String,
    /// The image instances run, such as `ghcr.io/xena-studios/skywars:1.4.0`.
    pub image: String,
    /// `hallow.toml`'s `base`: the base image versions it works with, as a
    /// semver range (`^0.2`).
    pub base: String,
    /// The base image version it was built on (the image's
    /// `io.xena-studios.hallow.base-version` label). Must sit inside `base`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_version: Option<String>,
    /// The git commit it was built from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(flatten)]
    pub actor: Actor,
}

/// Who did it, for the audit log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    /// A staff member's name, or a service such as `ci`. 1 to 64 characters.
    pub by: String,
    /// The panel user's id, when it came from the panel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel_user: Option<String>,
}

impl Actor {
    #[must_use]
    pub fn new(by: impl Into<String>) -> Self {
        Self {
            by: by.into(),
            panel_user: None,
        }
    }
}

/// `POST /v1/kinds/{name}/canary`: starts `percent` of new instances on
/// `version`. Replaces any canary already running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetCanary {
    pub version: String,
    /// 1 to 100.
    pub percent: u8,
    #[serde(flatten)]
    pub actor: Actor,
}

/// `POST /v1/kinds/{name}/promote`: makes a version live.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Promote {
    /// Default: the canary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(flatten)]
    pub actor: Actor,
}

/// `POST /v1/kinds/{name}/rollback`: makes an older version live again and
/// stops any canary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rollback {
    /// Default: the version that was live before this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(flatten)]
    pub actor: Actor,
}

/// The canary channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Canary {
    pub version: String,
    /// The share of new instances it gets, 1 to 100.
    pub percent: u8,
    /// Unix ms.
    pub since_ms: u64,
}

/// One version in [`KindVersions`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindVersion {
    pub version: String,
    pub image: String,
    pub base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Unix ms.
    pub added_ms: u64,
    pub added_by: String,
    /// `base` takes the network's base image version, so it may go live.
    pub compatible: bool,
    /// A `main` build (`sha-…` or `dev`): never live or canary unless the
    /// network allows dev versions.
    pub dev: bool,
    /// Instances on it now (creating, starting or ready).
    pub instances: u32,
}

/// `GET /v1/kinds/{name}/versions`, and the answer to every channel action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindVersions {
    pub kind: String,
    /// What new instances start on. `None` when the kind's image was set
    /// with `PUT /v1/kinds/{name}` and isn't a recorded version.
    pub live: Option<String>,
    /// The kind's live image (its `KindSpec::image`).
    pub live_image: String,
    /// What a rollback with no version goes back to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canary: Option<Canary>,
    /// The network's base image version, which `base` ranges are checked
    /// against.
    pub base_version: String,
    /// Whether dev builds may go live or canary on this network.
    pub dev_versions: bool,
    /// Newest first.
    pub versions: Vec<KindVersion>,
}

/// What an audit entry records.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionAction {
    /// A version was recorded.
    Add,
    /// A canary started, or its version or share changed.
    Canary,
    /// The canary stopped.
    StopCanary,
    /// A version was made live.
    Promote,
    /// An older version was made live again.
    Rollback,
    /// `PUT /v1/kinds/{name}` (or the kinds file) set a new live image.
    Register,
}

impl VersionAction {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Canary => "canary",
            Self::StopCanary => "stop_canary",
            Self::Promote => "promote",
            Self::Rollback => "rollback",
            Self::Register => "register",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "add" => Self::Add,
            "canary" => Self::Canary,
            "stop_canary" => Self::StopCanary,
            "promote" => Self::Promote,
            "rollback" => Self::Rollback,
            "register" => Self::Register,
            _ => return None,
        })
    }
}

/// One entry in `GET /v1/version-audit`, newest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionAuditEntry {
    pub id: i64,
    pub kind: String,
    pub action: VersionAction,
    /// The version acted on: added, made canary or live, or the canary
    /// stopped. `None` for a register whose image isn't a recorded version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// What it replaced: the live version before a promote, rollback or
    /// register, the canary before a canary change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_version: Option<String>,
    /// The image, for a register.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// The canary's share, for a canary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub percent: Option<u8>,
    pub by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panel_user: Option<String>,
    /// Unix ms.
    pub at_ms: u64,
}

/// Whether a version name is a `main` build: `sha-<commit>` or `dev`.
#[must_use]
pub fn is_dev_version(version: &str) -> bool {
    version == "dev" || version.starts_with("sha-")
}

/// Up to 64 letters, digits, `.`, `_`, `+` and `-`, starting with a letter
/// or digit.
#[must_use]
pub fn is_version_name(version: &str) -> bool {
    let bytes = version.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn actions_take_a_flat_actor() {
        let canary: SetCanary = serde_json::from_value(json!({
            "version": "1.4.0", "percent": 10, "by": "alice", "panel_user": "7"
        }))
        .unwrap();
        assert_eq!(canary.actor.by, "alice");
        assert_eq!(canary.actor.panel_user.as_deref(), Some("7"));
        let promote: Promote = serde_json::from_value(json!({ "by": "alice" })).unwrap();
        assert_eq!(promote.version, None);
        assert_eq!(
            serde_json::to_value(&promote).unwrap(),
            json!({ "by": "alice" })
        );
    }

    #[test]
    fn version_names() {
        for good in ["1.4.0", "sha-3f2a9c1", "dev", "1.0.0+build.2", "v2"] {
            assert!(is_version_name(good), "{good}");
        }
        for bad in ["", "-1", ".x", "a b", "a:b", "a/b", &"1".repeat(65)] {
            assert!(!is_version_name(bad), "{bad}");
        }
        assert!(is_dev_version("sha-3f2a9c1"));
        assert!(is_dev_version("dev"));
        assert!(!is_dev_version("1.4.0"));
    }

    #[test]
    fn actions_round_trip() {
        for action in [
            VersionAction::Add,
            VersionAction::Canary,
            VersionAction::StopCanary,
            VersionAction::Promote,
            VersionAction::Rollback,
            VersionAction::Register,
        ] {
            assert_eq!(VersionAction::parse(action.as_str()), Some(action));
            assert_eq!(
                serde_json::to_value(action).unwrap(),
                json!(action.as_str())
            );
        }
    }
}
