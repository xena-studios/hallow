//! Paths, methods and bodies of every endpoint. `{name}`-style segments are
//! filled in by the caller; the helpers build them.
//!
//! | Method | Path | Body | Response |
//! |---|---|---|---|
//! | `POST` | [`PROXIES`] | [`RegisterProxy`](crate::proxy::RegisterProxy) | [`ProxyRegistered`](crate::proxy::ProxyRegistered) |
//! | `POST` | [`proxy_heartbeat`] | [`ProxyHeartbeat`](crate::proxy::ProxyHeartbeat) | `204` |
//! | `DELETE` | [`proxy`] | | `204` |
//! | `GET` | [`proxy_commands`]`?since=` | | [`Poll`](crate::Poll)`<Vec<`[`ProxyCommand`](crate::command::ProxyCommand)`>>` |
//! | `POST` | [`SERVERS`] | [`RegisterServer`](crate::server::RegisterServer) | `204` |
//! | `POST` | [`server_heartbeat`] | [`ServerHeartbeat`](crate::server::ServerHeartbeat) | `204` |
//! | `DELETE` | [`server`] | | `204` |
//! | `GET` | [`SERVERS`]`?since=` | | [`Poll`](crate::Poll)`<Vec<`[`ServerInfo`](crate::server::ServerInfo)`>>` |
//! | `POST` | [`PRESENCE_LOGIN`] | [`LoginRequest`](crate::presence::LoginRequest) | [`LoginDecision`](crate::presence::LoginDecision) |
//! | `POST` | [`PRESENCE_SWITCH`] | [`PlayerSwitched`](crate::presence::PlayerSwitched) | `204` |
//! | `POST` | [`PRESENCE_LEAVE`] | [`PlayerLeft`](crate::presence::PlayerLeft) | `204` |
//! | `GET` | [`PRESENCE_COUNT`] | | [`NetworkCount`](crate::presence::NetworkCount) |
//! | `GET` | [`player`] | | [`PlayerLocation`](crate::presence::PlayerLocation) or `404` |
//! | `POST` | [`QUEUES`] | [`JoinQueue`](crate::queue::JoinQueue) | [`QueueStatus`](crate::queue::QueueStatus) |
//! | `DELETE` | [`queue_entry`] | | `204` |
//! | `POST` | [`COMMANDS`] | [`NetworkCommand`](crate::command::NetworkCommand) | `202` |
//!
//! Long polls are held for up to [`LONG_POLL_SECONDS`].

/// How long the control plane holds a long poll open before answering
/// [`Poll::Unchanged`](crate::Poll::Unchanged).
pub const LONG_POLL_SECONDS: u64 = 25;

/// How often proxies and servers should heartbeat. Missing three in a row
/// unregisters them.
pub const HEARTBEAT_SECONDS: u64 = 5;

pub const PROXIES: &str = "/v1/proxies";
pub const SERVERS: &str = "/v1/servers";
pub const PRESENCE_LOGIN: &str = "/v1/presence/login";
pub const PRESENCE_SWITCH: &str = "/v1/presence/switch";
pub const PRESENCE_LEAVE: &str = "/v1/presence/leave";
pub const PRESENCE_COUNT: &str = "/v1/presence/count";
pub const QUEUES: &str = "/v1/queues";
pub const COMMANDS: &str = "/v1/commands";

#[must_use]
pub fn proxy(id: &str) -> String {
    format!("{PROXIES}/{id}")
}

#[must_use]
pub fn proxy_heartbeat(id: &str) -> String {
    format!("{PROXIES}/{id}/heartbeat")
}

#[must_use]
pub fn proxy_commands(id: &str) -> String {
    format!("{PROXIES}/{id}/commands")
}

#[must_use]
pub fn server(name: &str) -> String {
    format!("{SERVERS}/{name}")
}

#[must_use]
pub fn server_heartbeat(name: &str) -> String {
    format!("{SERVERS}/{name}/heartbeat")
}

#[must_use]
pub fn player(uuid: &uuid::Uuid) -> String {
    format!("/v1/players/{uuid}")
}

#[must_use]
pub fn queue_entry(uuid: &uuid::Uuid) -> String {
    format!("{QUEUES}/{uuid}")
}

/// Appends `?since=<revision>` for a long poll.
#[must_use]
pub fn since(path: &str, revision: u64) -> String {
    format!("{path}?since={revision}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(proxy_heartbeat("proxy-1"), "/v1/proxies/proxy-1/heartbeat");
        assert_eq!(since(SERVERS, 12), "/v1/servers?since=12");
        assert_eq!(
            player(&uuid::Uuid::nil()),
            "/v1/players/00000000-0000-0000-0000-000000000000"
        );
    }
}
