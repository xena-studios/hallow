//! Servers (Pumpkin) register when they're ready for players and heartbeat
//! while they run. Proxies long-poll the list and mirror it into Vine's
//! runtime server registry.
//!
//! Each server also reports who is on it: a join or leave as it happens
//! ([`ServerPlayersChanged`]) and the whole list with every heartbeat
//! ([`ServerHeartbeat::roster`]), which corrects anything a lost report
//! left wrong. That view is the server's own, independent of the proxies'.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What a server is for, from its repo's `hallow.toml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerRole {
    Lobby,
    Game,
}

/// Where a server is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerState {
    /// Up and taking players: a lobby, or a game waiting for its match.
    Ready,
    /// A match is running; only spectators and reconnects join.
    InGame,
    /// Finishing up; no new players. Unregisters when empty.
    Draining,
}

/// `POST /v1/servers`, sent by the Hallow server plugin once it accepts players.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterServer {
    /// Unique per running instance, such as `skywars-7f3a`. Players and Vine use this.
    pub name: String,
    /// The server repo it runs, such as `skywars` or `skywars-lobby`.
    pub kind: String,
    /// The repo version (git tag) it runs, such as `v1.4.0`.
    pub version: String,
    pub role: ServerRole,
    /// Where proxies reach it, `host:port` on the private network.
    pub address: String,
    pub max_players: u32,
}

/// `POST /v1/servers/{name}/heartbeat`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerHeartbeat {
    pub players: u32,
    pub state: ServerState,
    /// Average milliseconds per tick lately (50 is the 20 TPS budget), if the
    /// server measures it. Shown on the monitoring dashboard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_ms: Option<f32>,
    /// Everyone on the server now. When present, the control plane replaces
    /// its list of the server's players with this one; when absent, it keeps
    /// the list as the join and leave reports left it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roster: Option<Vec<ServerPlayer>>,
}

/// A player on a server, as the server sees them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerPlayer {
    pub uuid: Uuid,
    pub username: String,
}

/// `POST /v1/servers/{name}/players`: players who joined or left the server
/// since the last report. A player who joins one server is taken off any
/// other server's list straight away.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerPlayersChanged {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joined: Vec<ServerPlayer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub left: Vec<Uuid>,
}

/// One entry in `GET /v1/servers`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServerInfo {
    pub name: String,
    pub kind: String,
    pub version: String,
    pub role: ServerRole,
    pub address: String,
    pub max_players: u32,
    pub players: u32,
    pub state: ServerState,
    /// From the last heartbeat that reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_ms: Option<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn server_info_shape() {
        let info = ServerInfo {
            name: "skywars-7f3a".into(),
            kind: "skywars".into(),
            version: "v1.4.0".into(),
            role: ServerRole::Game,
            address: "10.0.0.5:25565".into(),
            max_players: 12,
            players: 3,
            state: ServerState::InGame,
            tick_ms: Some(12.5),
        };
        let value = serde_json::to_value(&info).unwrap();
        assert_eq!(value["role"], json!("game"));
        assert_eq!(value["state"], json!("in_game"));
        assert_eq!(serde_json::from_value::<ServerInfo>(value).unwrap(), info);
    }

    #[test]
    fn heartbeat_roster_is_optional() {
        let old: ServerHeartbeat =
            serde_json::from_value(json!({"players": 2, "state": "ready"})).unwrap();
        assert_eq!(old.roster, None);
        let beat = ServerHeartbeat {
            players: 1,
            state: ServerState::Ready,
            tick_ms: None,
            roster: Some(vec![ServerPlayer {
                uuid: Uuid::nil(),
                username: "Steve".into(),
            }]),
        };
        let value = serde_json::to_value(&beat).unwrap();
        assert_eq!(value["roster"][0]["username"], json!("Steve"));
        assert_eq!(
            serde_json::to_value(ServerPlayersChanged::default()).unwrap(),
            json!({})
        );
    }
}
