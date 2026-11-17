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
    /// The player's address as the proxy sees it (the real one behind a load
    /// balancer with PROXY protocol). Kept for ban evasion checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
}

/// The answer to [`LoginRequest`]: where the player goes. The network never
/// turns a player away for something it can fix: a ban sends them to the
/// banned lobby, a full network or no free hub holds them in limbo until
/// there is room (docs/ARCHITECTURE.md, "A player's path").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum LoginDecision {
    /// Let the player in and send them to `server`. `permissions` are the
    /// player's nodes from their ranks (see [`crate::permission`]); empty
    /// when the network runs without a database.
    Allow {
        server: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        permissions: Vec<String>,
        /// The player's active mute, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mute: Option<crate::punishment::MuteInfo>,
        /// Set for a banned player: `server` is a banned lobby (or limbo
        /// while none is up) and the player can't leave it. The control
        /// plane lifts it when the ban ends.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ban: Option<crate::punishment::BanInfo>,
    },
    /// Let the player in but hold them in limbo ([`LIMBO`]) until there is
    /// room: the network is full or no hub is free. The control plane moves
    /// them on by itself, oldest first.
    Wait {
        /// Why they wait, shown to the player.
        reason: String,
        /// Their place among the players waiting, from 1.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        permissions: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mute: Option<crate::punishment::MuteInfo>,
    },
    /// Refuse the login with `reason`. Hallow only sends it when the
    /// network is set to keep an old session over a new one
    /// (`HALLOW_DUPLICATE_LOGIN=deny`).
    Deny { reason: String },
}

/// The server name of the proxy's built-in limbo, where players wait when
/// the network can't place them. Vine answers it itself, so it is there
/// whenever the proxy is ([Vine fork], `[limbo]` in `vine.toml`).
///
/// [Vine fork]: https://github.com/xena-studios/Vine
pub const LIMBO: &str = "limbo";

/// `POST /v1/presence/fallback`, sent by a proxy when a server kicked a
/// player, crashed, stopped, or couldn't be reached: where should they go?
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackRequest {
    pub uuid: Uuid,
    /// The server the player lost.
    pub from: String,
    /// The server's kick message, if it sent one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The answer to [`FallbackRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fallback {
    /// A game's lobby, a hub, a banned lobby for a banned player, or
    /// [`LIMBO`].
    pub server: String,
    /// What the player is told once they arrive.
    pub message: String,
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
                server: "hub-1".into(),
                permissions: vec![],
                mute: None,
                ban: None,
            }
        );
        let wait: LoginDecision = serde_json::from_value(
            json!({"decision": "wait", "reason": "The network is full.", "position": 3}),
        )
        .unwrap();
        assert_eq!(
            wait,
            LoginDecision::Wait {
                reason: "The network is full.".into(),
                position: Some(3),
                permissions: vec![],
                mute: None,
            }
        );
    }
}
