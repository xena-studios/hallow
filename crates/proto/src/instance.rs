//! Server kinds and the instances the control plane runs of them
//! (docs/INSTANCES.md).
//!
//! A **kind** is a server repo the control plane may start: its live image
//! and the parts of its `hallow.toml` that decide how it scales. Staff (or
//! the panel, or a deploy) register one with `PUT /v1/kinds/{name}`; the
//! control plane can also load kinds from a file at start.
//!
//! An **instance** is one running server of a kind, created by the control
//! plane through its orchestrator (Wings on real nodes, Docker on the dev
//! network). The server inside registers itself like any other (see
//! [`crate::server`]) under the instance's name; [`InstanceInfo`] is the
//! control plane's view of its life around that.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What a kind is for, as in `hallow.toml`'s `role`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KindRole {
    Hub,
    Lobby,
    Game,
}

/// `hallow.toml`'s `[players]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindPlayers {
    /// Players needed to start a match. Default 1.
    #[serde(default = "one")]
    pub min: u32,
    /// Players per instance.
    pub max: u32,
}

/// `hallow.toml`'s `[scaling]`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindScaling {
    /// Unused instances kept ready while players are online.
    #[serde(default)]
    pub warm: u32,
    /// The most instances of the kind at once; none means no limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_instances: Option<u32>,
}

fn one() -> u32 {
    1
}

/// `PUT /v1/kinds/{name}`: registers a kind, or replaces it. Running
/// instances keep the image they started with; new ones use this.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindSpec {
    /// The image new instances run, such as `ghcr.io/xena-studios/skywars:1.4.0`.
    pub image: String,
    pub role: KindRole,
    /// The kind players return to when a game ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lobby: Option<String>,
    pub players: KindPlayers,
    #[serde(default)]
    pub scaling: KindScaling,
    /// Extra environment for every instance. `HALLOW_*` names are the
    /// control plane's and refused.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Memory limit per instance in MiB, for orchestrators that set one.
    /// Default 1024.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_mb: Option<u32>,
    /// Who registered it, for the audit trail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_by: Option<String>,
}

/// One entry in `GET /v1/kinds`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindInfo {
    pub name: String,
    #[serde(flatten)]
    pub spec: KindSpec,
    /// Unix ms.
    pub updated_ms: u64,
}

/// Where an instance is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceState {
    /// Decided on; the orchestrator is creating it.
    Creating,
    /// Created and started; waiting for its server to register.
    Starting,
    /// Its server registered and takes players.
    Ready,
    /// Done (its game ended, it emptied, or it isn't needed): being stopped.
    /// The matchmaker sends nobody to it.
    Ending,
    /// Stopped; the orchestrator is deleting it.
    Deleting,
    /// An orchestrator call failed; `error` says why. Retried.
    Failed,
}

impl InstanceState {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Ending => "ending",
            Self::Deleting => "deleting",
            Self::Failed => "failed",
        }
    }

    /// The instance may still take players, now or once it's up.
    #[must_use]
    pub fn is_live(self) -> bool {
        matches!(self, Self::Creating | Self::Starting | Self::Ready)
    }
}

/// One entry in `GET /v1/instances`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceInfo {
    /// `<kind>-<6 hex>`; also its server's name.
    pub name: String,
    pub kind: String,
    /// Wings' server UUID; Docker's containers are named by `name`.
    pub uuid: Uuid,
    pub image: String,
    /// The node it runs on.
    pub node: String,
    pub state: InstanceState,
    /// Unix ms when the state last changed.
    pub state_since_ms: u64,
    pub created_ms: u64,
    /// Where proxies reach it, once known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// The game port on the node, for orchestrators that bind one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The orchestrator's id for it (a container id, a Wings UUID).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_id: Option<String>,
    /// Players on its server, from the server list.
    #[serde(default)]
    pub players: u32,
    /// It has had players: once it empties again its game is over.
    #[serde(default)]
    pub had_players: bool,
    /// Unix ms since when it has been empty after having players.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub empty_since_ms: Option<u64>,
    /// Why it is ending, or the last orchestrator error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kind_spec_defaults_like_hallow_toml() {
        let spec: KindSpec = serde_json::from_value(json!({
            "image": "hallow-test-game:dev",
            "role": "game",
            "players": {"max": 16},
        }))
        .unwrap();
        assert_eq!(spec.players.min, 1);
        assert_eq!(spec.scaling, KindScaling::default());
        let info = KindInfo {
            name: "test-game".into(),
            spec,
            updated_ms: 5,
        };
        let value = serde_json::to_value(&info).unwrap();
        assert_eq!(value["name"], json!("test-game"));
        assert_eq!(value["role"], json!("game"));
        assert_eq!(serde_json::from_value::<KindInfo>(value).unwrap(), info);
    }

    #[test]
    fn instance_states_are_snake_case() {
        assert_eq!(
            serde_json::to_value(InstanceState::Deleting).unwrap(),
            json!("deleting")
        );
        assert!(InstanceState::Starting.is_live());
        assert!(!InstanceState::Ending.is_live());
    }
}
