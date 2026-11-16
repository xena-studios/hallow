//! Node bootstrap: one-time join tokens, and the join a new machine makes
//! with one (docs/FLEET.md, "Setting up a new node").
//!
//! The fleet manager (or a staff member) asks for a token with
//! [`IssueJoinToken`] and puts it in the machine's cloud-init or install
//! command. On first boot `deploy/node/install.sh` sends a [`JoinRequest`]
//! with the token as its bearer token. The control plane uses the token up,
//! creates the node and its game-port allocations in the panel, and answers
//! with a [`JoinReply`] holding the panel's one-time Wings enrollment code,
//! which Wings redeems with the panel for its own credentials.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Base nodes are monthly dedicated machines; burst nodes are hourly cloud
/// machines (docs/FLEET.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeRole {
    Base,
    Burst,
}

impl NodeRole {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Burst => "burst",
        }
    }
}

/// `POST /v1/nodes/join-tokens`, with the control plane's bearer token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueJoinToken {
    pub role: NodeRole,
    /// Region, such as `fsn1`; picks the panel location.
    pub region: String,
    /// The node's name in the panel. Without one, the machine's hostname.
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// How long the token stays usable; the control plane's default (24 h)
    /// without one, at most 7 days.
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}

/// The issued token. The token itself is only ever shown here; the control
/// plane keeps a hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinToken {
    pub id: Uuid,
    /// `hjt_` and 64 hex digits.
    pub token: String,
    pub role: NodeRole,
    pub region: String,
    pub name: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub expires_at_ms: u64,
}

/// `POST /v1/nodes/join`, with `Authorization: Bearer <join token>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequest {
    pub hostname: String,
    /// The address the panel and load balancer reach the node on: its
    /// WireGuard (or provider private network) address. Game-port
    /// allocations are made on it.
    pub private_ip: String,
    #[serde(default)]
    pub public_ip: Option<String>,
    /// So the private network's hub can add the node as a peer.
    #[serde(default)]
    pub wireguard_public_key: Option<String>,
    /// `x86_64` or `aarch64`.
    pub arch: String,
    pub memory_mb: u64,
    pub disk_mb: u64,
}

/// The answer to a join: what `wings configure --enroll` needs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinReply {
    /// The node's UUID in the panel.
    pub node: Uuid,
    pub name: String,
    pub role: NodeRole,
    pub region: String,
    /// The panel URL Wings talks to.
    pub panel_url: String,
    /// The panel's one-time Wings enrollment code (valid 30 minutes).
    pub enroll_code: String,
    pub enroll_expires_at_ms: u64,
}

/// One joined node in `GET /v1/nodes`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeInfo {
    pub name: String,
    pub panel_uuid: Uuid,
    pub role: NodeRole,
    pub region: String,
    pub labels: BTreeMap<String, String>,
    pub private_ip: String,
    pub public_ip: Option<String>,
    pub wireguard_public_key: Option<String>,
    pub joined_at_ms: u64,
}
