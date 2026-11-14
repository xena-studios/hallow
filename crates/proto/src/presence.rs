//! Who is online and where. Proxies report logins, switches and leaves; the
//! control plane keeps presence in Redis and answers counts and lookups.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// `POST /v1/presence/login`, sent by a proxy after authentication and before
/// the player reaches a server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginRequest {
    pub uuid: Uuid,
    pub username: String,
    pub proxy: String,
    /// Java protocol version, or `None` for a Bedrock player through Geyser.
    pub protocol: Option<i32>,
}

/// The answer to [`LoginRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum LoginDecision {
    /// Let the player in and send them to `server`.
    Allow { server: String },
    /// Refuse the login with `reason`, shown to the player. Used for a
    /// duplicate login when the network keeps the old session, bans, and a
    /// full network.
    Deny { reason: String },
}

/// `POST /v1/presence/switch`, sent when a player lands on another server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerSwitched {
    pub uuid: Uuid,
    pub server: String,
}

/// `POST /v1/presence/leave`, sent when a player disconnects from a proxy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerLeft {
    pub uuid: Uuid,
    pub proxy: String,
}

/// `GET /v1/presence/count`: what proxies show in the server list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkCount {
    pub online: u32,
    pub max: u32,
}

/// `GET /v1/players/{uuid}`, for `/find`, messages and party warps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerLocation {
    pub uuid: Uuid,
    pub username: String,
    pub proxy: String,
    /// `None` while the player is between servers.
    pub server: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn login_decision_is_tagged() {
        assert_eq!(
            serde_json::to_value(LoginDecision::Deny {
                reason: "You are already online.".into()
            })
            .unwrap(),
            json!({"decision": "deny", "reason": "You are already online."})
        );
        let allow: LoginDecision =
            serde_json::from_value(json!({"decision": "allow", "server": "hub-1"})).unwrap();
        assert_eq!(
            allow,
            LoginDecision::Allow {
                server: "hub-1".into()
            }
        );
    }
}
