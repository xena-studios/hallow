//! Paths, methods and bodies of every endpoint. `{name}`-style segments are
//! filled in by the caller; the helpers build them.
//!
//! | Method | Path | Body | Response |
//! |---|---|---|---|
//! | `POST` | [`PROXIES`] | [`RegisterProxy`](crate::proxy::RegisterProxy) | [`ProxyRegistered`](crate::proxy::ProxyRegistered) |
//! | `GET` | [`PROXIES`] | | `Vec<`[`ProxyInfo`](crate::proxy::ProxyInfo)`>` |
//! | `POST` | [`proxy_heartbeat`] | [`ProxyHeartbeat`](crate::proxy::ProxyHeartbeat) | `204` |
//! | `POST` | [`proxy_drain`] | [`SetDraining`](crate::proxy::SetDraining) | `204` |
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
//! | `GET` | [`player_by_name`] | | [`PlayerLocation`](crate::presence::PlayerLocation) or `404`; names match without case |
//! | `POST` | [`FRIENDS`] | [`FriendAction`](crate::friend::FriendAction) | [`FriendReply`](crate::friend::FriendReply) |
//! | `POST` | [`PARTIES`] | [`PartyAction`](crate::party::PartyAction) | [`PartyReply`](crate::party::PartyReply) |
//! | `GET` | [`party`] | | [`PartyInfo`](crate::party::PartyInfo) or `404` |
//! | `POST` | [`PUNISHMENTS`] | [`IssuePunishment`](crate::punishment::IssuePunishment) | [`PunishmentReply`](crate::punishment::PunishmentReply) |
//! | `POST` | [`PUNISHMENTS_REVOKE`] | [`RevokePunishment`](crate::punishment::RevokePunishment) | [`PunishmentReply`](crate::punishment::PunishmentReply) |
//! | `GET` | [`punishment_history`] | | `Vec<`[`PunishmentRecord`](crate::punishment::PunishmentRecord)`>` |
//! | `POST` | [`SIGNALS`] | [`SignalBatch`](crate::signal::SignalBatch) | `204` |
//! | `GET` | [`signals_of`] | | `Vec<`[`SignalRecord`](crate::signal::SignalRecord)`>` |
//! | `POST` | [`QUEUES`] | [`JoinQueue`](crate::queue::JoinQueue) | [`QueueStatus`](crate::queue::QueueStatus) |
//! | `DELETE` | [`queue_entry`] | | `204` |
//! | `POST` | [`COMMANDS`] | [`NetworkCommand`](crate::command::NetworkCommand) | `202` |
//! | `GET` | [`FLEET_SETTINGS`] | | [`FleetSettingsRecord`](crate::fleet::FleetSettingsRecord); the defaults if never changed |
//! | `PUT` | [`FLEET_SETTINGS`] | [`UpdateFleetSettings`](crate::fleet::UpdateFleetSettings) | [`FleetSettingsRecord`](crate::fleet::FleetSettingsRecord), or `400` with [`ApiError::fields`](crate::ApiError::fields) |
//! | `GET` | [`fleet_settings_history`] | | `Vec<`[`FleetSettingsChange`](crate::fleet::FleetSettingsChange)`>`, newest first |
//!
//! The control plane also answers HAProxy agent checks on a separate TCP
//! port (not HTTP); see the control plane's README.
//!
//! Long polls are held for up to [`LONG_POLL_SECONDS`], or for `?wait=`
//! seconds if that is shorter. `wait=0` answers at once, for callers that
//! can't hold a request open, such as Vine plugins, whose calls block the
//! plugin while they run.

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
pub const PLAYERS: &str = "/v1/players";
pub const PARTIES: &str = "/v1/parties";
pub const FRIENDS: &str = "/v1/friends";
pub const SIGNALS: &str = "/v1/signals";
pub const PUNISHMENTS: &str = "/v1/punishments";
pub const PUNISHMENTS_REVOKE: &str = "/v1/punishments/revoke";
pub const FLEET_SETTINGS: &str = "/v1/fleet/settings";
pub const FLEET_SETTINGS_HISTORY: &str = "/v1/fleet/settings/history";

/// How many changes [`fleet_settings_history`] returns when `limit` is
/// left out, and the most it returns.
pub const FLEET_SETTINGS_HISTORY_DEFAULT: u32 = 50;
pub const FLEET_SETTINGS_HISTORY_MAX: u32 = 500;

#[must_use]
pub fn proxy(id: &str) -> String {
    format!("{PROXIES}/{id}")
}

#[must_use]
pub fn proxy_heartbeat(id: &str) -> String {
    format!("{PROXIES}/{id}/heartbeat")
}

#[must_use]
pub fn proxy_drain(id: &str) -> String {
    format!("{PROXIES}/{id}/drain")
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
    format!("{PLAYERS}/{uuid}")
}

/// `/v1/players?name=<username>`, percent-encoding anything but letters,
/// digits, `_`, `-` and `.` (Bedrock names through Geyser can hold spaces).
#[must_use]
pub fn player_by_name(name: &str) -> String {
    let mut path = format!("{PLAYERS}?name=");
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.') {
            path.push(char::from(byte));
        } else {
            path.push_str(&format!("%{byte:02X}"));
        }
    }
    path
}

/// A player's punishments by username, newest first.
#[must_use]
pub fn punishment_history(name: &str) -> String {
    player_by_name(name).replacen(PLAYERS, PUNISHMENTS, 1)
}

/// A player's recent anticheat signal samples by username, newest first.
#[must_use]
pub fn signals_of(name: &str) -> String {
    player_by_name(name).replacen(PLAYERS, SIGNALS, 1)
}

/// The party `uuid` is in.
#[must_use]
pub fn party(uuid: &uuid::Uuid) -> String {
    format!("{PARTIES}/{uuid}")
}

#[must_use]
pub fn queue_entry(uuid: &uuid::Uuid) -> String {
    format!("{QUEUES}/{uuid}")
}

/// `/v1/fleet/settings/history?limit=<limit>`: the latest settings changes,
/// newest first.
#[must_use]
pub fn fleet_settings_history(limit: u32) -> String {
    format!("{FLEET_SETTINGS_HISTORY}?limit={limit}")
}

/// Appends `?since=<revision>` for a long poll.
#[must_use]
pub fn since(path: &str, revision: u64) -> String {
    format!("{path}?since={revision}")
}

/// Appends `?since=<revision>&wait=<seconds>`: a poll held for at most
/// `wait` seconds (`0` for none).
#[must_use]
pub fn since_wait(path: &str, revision: u64, wait: u64) -> String {
    format!("{path}?since={revision}&wait={wait}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(proxy_heartbeat("proxy-1"), "/v1/proxies/proxy-1/heartbeat");
        assert_eq!(since(SERVERS, 12), "/v1/servers?since=12");
        assert_eq!(
            fleet_settings_history(20),
            "/v1/fleet/settings/history?limit=20"
        );
        assert_eq!(since_wait(SERVERS, 12, 0), "/v1/servers?since=12&wait=0");
        assert_eq!(player_by_name("Steve_1"), "/v1/players?name=Steve_1");
        assert_eq!(
            player_by_name(".Bedrock Guy"),
            "/v1/players?name=.Bedrock%20Guy"
        );
        assert_eq!(
            player(&uuid::Uuid::nil()),
            "/v1/players/00000000-0000-0000-0000-000000000000"
        );
    }
}
