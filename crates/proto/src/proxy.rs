//! Proxies (Vine) register when they start, heartbeat while they run, and
//! long-poll for commands the control plane wants them to carry out.

use serde::{Deserialize, Serialize};

/// `POST /v1/proxies`, sent by the Hallow Vine plugin on load.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterProxy {
    /// Unique per running proxy, such as its Wings server id.
    pub id: String,
    /// Where the load balancer reaches it, `host:port`.
    pub address: String,
    /// Vine version, for the panel.
    pub version: String,
}

/// The answer to [`RegisterProxy`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyRegistered {
    /// The current server list revision; poll [`crate::routes::SERVERS`] from here.
    pub servers_revision: u64,
    /// The current command revision; poll [`crate::routes::proxy_commands`] from here.
    pub commands_revision: u64,
}

/// `POST /v1/proxies/{id}/heartbeat`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyHeartbeat {
    pub players: u32,
    /// The proxy refuses new logins and is waiting for players to leave.
    pub draining: bool,
}

/// A proxy as the panel and other services see it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyInfo {
    pub id: String,
    pub address: String,
    pub version: String,
    pub players: u32,
    pub draining: bool,
    pub last_heartbeat_ms: u64,
}
