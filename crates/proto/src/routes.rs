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
//! | `POST` | [`server_players`] | [`ServerPlayersChanged`](crate::server::ServerPlayersChanged) | `204`, or `404` if the server isn't registered |
//! | `GET` | [`server_players`] | | `Vec<`[`ServerPlayer`](crate::server::ServerPlayer)`>` by username, or `404` |
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
//! | `GET` | [`PUNISHMENTS_SEARCH`]`?player=&kind=&active=&limit=&offset=` | | [`PunishmentPage`](crate::punishment::PunishmentPage), newest first |
//! | `GET` | [`PUNISHMENT_TEMPLATES`] | | `Vec<`[`PunishmentTemplate`](crate::punishment::PunishmentTemplate)`>` |
//! | `POST` | [`PUNISHMENT_TEMPLATES`] | [`SaveTemplate`](crate::punishment::SaveTemplate) | [`PunishmentTemplate`](crate::punishment::PunishmentTemplate), or `400` with fields |
//! | `PUT` | [`punishment_template`] | [`SaveTemplate`](crate::punishment::SaveTemplate) | [`PunishmentTemplate`](crate::punishment::PunishmentTemplate), or `400` / `404` |
//! | `DELETE` | [`punishment_template`] | | `204` or `404` |
//! | `POST` | [`REPORTS`] | [`CreateReport`](crate::report::CreateReport) | [`ReportCreated`](crate::report::ReportCreated); `404` if the target never joined, `400` for yourself or a bad reason, `409` with an open report on them already, `429` past 5 reports in 10 minutes |
//! | `GET` | [`REPORTS`]`?status=&target=&limit=&offset=` | | [`ReportPage`](crate::report::ReportPage), newest first; `status` is one status or `active` |
//! | `GET` | [`report`] | | [`Report`](crate::report::Report) or `404` |
//! | `POST` | [`report_claim`] | [`ClaimReport`](crate::report::ClaimReport) | [`Report`](crate::report::Report); `409` once closed |
//! | `POST` | [`report_unclaim`] | | [`Report`](crate::report::Report), open again; `409` once closed |
//! | `POST` | [`report_resolve`] | [`CloseReport`](crate::report::CloseReport) | [`Report`](crate::report::Report); `400` if the punishment isn't the target's, `409` once closed |
//! | `POST` | [`report_dismiss`] | [`CloseReport`](crate::report::CloseReport) | [`Report`](crate::report::Report); `409` once closed |
//! | `GET` | [`RANKS`] | | `Vec<`[`RankInfo`](crate::rank::RankInfo)`>`, highest weight first |
//! | `PUT` | [`rank`] | [`SaveRank`](crate::rank::SaveRank) | [`RankInfo`](crate::rank::RankInfo), or `400` with fields |
//! | `DELETE` | [`rank`] | | `204`, `404`, or `409` for the default rank |
//! | `GET` | [`rank_grants_of`] | | [`PlayerRanks`](crate::rank::PlayerRanks) or `404` |
//! | `POST` | [`RANK_GRANTS`] | [`GrantRank`](crate::rank::GrantRank) | [`PlayerRanks`](crate::rank::PlayerRanks), or `404` |
//! | `POST` | [`RANK_GRANTS_REVOKE`] | [`RevokeRank`](crate::rank::RevokeRank) | [`PlayerRanks`](crate::rank::PlayerRanks), or `404` |
//! | `POST` | [`SIGNALS`] | [`SignalBatch`](crate::signal::SignalBatch) | `204` |
//! | `GET` | [`signals_of`] | | `Vec<`[`SignalRecord`](crate::signal::SignalRecord)`>` |
//! | `POST` | [`STATS`] | [`StatsBatch`](crate::stats::StatsBatch) | `204` |
//! | `GET` | [`stats_of`] | | [`PlayerStats`](crate::stats::PlayerStats) or `404`; by UUID or username |
//! | `POST` | [`QUEUES`] | [`JoinQueue`](crate::queue::JoinQueue) | [`QueueStatus`](crate::queue::QueueStatus) |
//! | `DELETE` | [`queue_entry`] | | `204` |
//! | `GET` | [`LOBBIES`] | | [`LobbyOverview`](crate::lobby::LobbyOverview) |
//! | `POST` | [`LOBBIES_SEND`] | [`SendToLobby`](crate::lobby::SendToLobby) | [`LobbySent`](crate::lobby::LobbySent); `404` for an unknown mode or a player who isn't online, `409` with a message for the player if no lobby has room or they're already there |
//! | `POST` | [`COMMANDS`] | [`NetworkCommand`](crate::command::NetworkCommand) | `202` |
//! | `GET` | [`FLEET_SETTINGS`] | | [`FleetSettingsRecord`](crate::fleet::FleetSettingsRecord); the defaults if never changed |
//! | `PUT` | [`FLEET_SETTINGS`] | [`UpdateFleetSettings`](crate::fleet::UpdateFleetSettings) | [`FleetSettingsRecord`](crate::fleet::FleetSettingsRecord), or `400` with [`ApiError::fields`](crate::ApiError::fields) |
//! | `GET` | [`fleet_settings_history`] | | `Vec<`[`FleetSettingsChange`](crate::fleet::FleetSettingsChange)`>`, newest first |
//! | `POST` | [`NODE_JOIN_TOKENS`] | [`IssueJoinToken`](crate::node::IssueJoinToken) | [`JoinToken`](crate::node::JoinToken) |
//! | `POST` | [`NODE_JOIN`] | [`JoinRequest`](crate::node::JoinRequest), bearer is the join token | [`JoinReply`](crate::node::JoinReply) |
//! | `GET` | [`NODES`] | | `Vec<`[`NodeInfo`](crate::node::NodeInfo)`>` |
//! | `GET` | [`KINDS`] | | `Vec<`[`KindInfo`](crate::instance::KindInfo)`>` by name |
//! | `PUT` | [`kind`] | [`KindSpec`](crate::instance::KindSpec) | [`KindInfo`](crate::instance::KindInfo) |
//! | `GET` | [`kind`] | | [`KindInfo`](crate::instance::KindInfo) or `404` |
//! | `DELETE` | [`kind`] | | `204`; running instances finish, no new ones start |
//! | `GET` | [`INSTANCES`] | | `Vec<`[`InstanceInfo`](crate::instance::InstanceInfo)`>` by name |
//! | `DELETE` | [`instance`] | | `202`: the instance ends (its players are sent back by their proxy) |
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
pub const LOBBIES: &str = "/v1/lobbies";
pub const LOBBIES_SEND: &str = "/v1/lobbies/send";
pub const COMMANDS: &str = "/v1/commands";
pub const PLAYERS: &str = "/v1/players";
pub const PARTIES: &str = "/v1/parties";
pub const FRIENDS: &str = "/v1/friends";
pub const SIGNALS: &str = "/v1/signals";
pub const STATS: &str = "/v1/stats";
pub const PUNISHMENTS: &str = "/v1/punishments";
pub const PUNISHMENTS_REVOKE: &str = "/v1/punishments/revoke";
pub const PUNISHMENTS_SEARCH: &str = "/v1/punishments/search";
pub const PUNISHMENT_TEMPLATES: &str = "/v1/punishments/templates";
pub const RANKS: &str = "/v1/ranks";
pub const REPORTS: &str = "/v1/reports";

/// How many reports [`REPORTS`] returns when `limit` is left out, and the
/// most it returns.
pub const REPORTS_DEFAULT: u32 = 50;
pub const REPORTS_MAX: u32 = 200;
pub const RANK_GRANTS: &str = "/v1/rank-grants";
pub const RANK_GRANTS_REVOKE: &str = "/v1/rank-grants/revoke";

/// How many punishments [`PUNISHMENTS_SEARCH`] returns when `limit` is left
/// out, and the most it returns.
pub const PUNISHMENT_SEARCH_DEFAULT: u32 = 50;
pub const PUNISHMENT_SEARCH_MAX: u32 = 200;
pub const FLEET_SETTINGS: &str = "/v1/fleet/settings";
pub const FLEET_SETTINGS_HISTORY: &str = "/v1/fleet/settings/history";

/// How many changes [`fleet_settings_history`] returns when `limit` is
/// left out, and the most it returns.
pub const FLEET_SETTINGS_HISTORY_DEFAULT: u32 = 50;
pub const FLEET_SETTINGS_HISTORY_MAX: u32 = 500;
pub const NODES: &str = "/v1/nodes";
pub const NODE_JOIN_TOKENS: &str = "/v1/nodes/join-tokens";
/// The one route that takes a join token instead of the control plane's.
pub const NODE_JOIN: &str = "/v1/nodes/join";
pub const KINDS: &str = "/v1/kinds";
pub const INSTANCES: &str = "/v1/instances";

#[must_use]
pub fn kind(name: &str) -> String {
    format!("{KINDS}/{name}")
}

#[must_use]
pub fn instance(name: &str) -> String {
    format!("{INSTANCES}/{name}")
}

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

/// Who is on a server: `POST` reports joins and leaves, `GET` lists them.
#[must_use]
pub fn server_players(name: &str) -> String {
    format!("{SERVERS}/{name}/players")
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
    encode_into(&mut path, name);
    path
}

/// A player's punishments by username, newest first.
#[must_use]
pub fn punishment_history(name: &str) -> String {
    player_by_name(name).replacen(PLAYERS, PUNISHMENTS, 1)
}

#[must_use]
pub fn punishment_template(id: i64) -> String {
    punishment_template_path(&id.to_string())
}

/// [`punishment_template`] with any segment, such as `{id}` for a router.
#[must_use]
pub fn punishment_template_path(id: &str) -> String {
    format!("{PUNISHMENT_TEMPLATES}/{id}")
}

#[must_use]
pub fn report(id: i64) -> String {
    report_path(&id.to_string())
}

/// [`report`] with any segment, such as `{id}` for a router.
#[must_use]
pub fn report_path(id: &str) -> String {
    format!("{REPORTS}/{id}")
}

#[must_use]
pub fn report_claim(id: i64) -> String {
    format!("{}/claim", report(id))
}

#[must_use]
pub fn report_unclaim(id: i64) -> String {
    format!("{}/unclaim", report(id))
}

#[must_use]
pub fn report_resolve(id: i64) -> String {
    format!("{}/resolve", report(id))
}

#[must_use]
pub fn report_dismiss(id: i64) -> String {
    format!("{}/dismiss", report(id))
}

#[must_use]
pub fn rank(name: &str) -> String {
    format!("{RANKS}/{name}")
}

/// A player's ranks by username.
#[must_use]
pub fn rank_grants_of(name: &str) -> String {
    player_by_name(name).replacen(PLAYERS, RANK_GRANTS, 1)
}

/// A player's recent anticheat signal samples by username, newest first.
#[must_use]
pub fn signals_of(name: &str) -> String {
    player_by_name(name).replacen(PLAYERS, SIGNALS, 1)
}

/// A player's stats, by UUID or username, percent-encoded like
/// [`player_by_name`].
#[must_use]
pub fn stats_of(player: &str) -> String {
    let mut path = format!("{STATS}/");
    encode_into(&mut path, player);
    path
}

fn encode_into(path: &mut String, text: &str) {
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.') {
            path.push(char::from(byte));
        } else {
            path.push_str(&format!("%{byte:02X}"));
        }
    }
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
        assert_eq!(server_players("hub-1"), "/v1/servers/hub-1/players");
        assert_eq!(stats_of("Steve_1"), "/v1/stats/Steve_1");
        assert_eq!(stats_of(".Bedrock Guy"), "/v1/stats/.Bedrock%20Guy");
        assert_eq!(since(SERVERS, 12), "/v1/servers?since=12");
        assert_eq!(
            fleet_settings_history(20),
            "/v1/fleet/settings/history?limit=20"
        );
        assert_eq!(since_wait(SERVERS, 12, 0), "/v1/servers?since=12&wait=0");
        assert_eq!(player_by_name("Steve_1"), "/v1/players?name=Steve_1");
        assert_eq!(rank_grants_of("Steve"), "/v1/rank-grants?name=Steve");
        assert_eq!(punishment_template(7), "/v1/punishments/templates/7");
        assert_eq!(report(4), "/v1/reports/4");
        assert_eq!(report_resolve(4), "/v1/reports/4/resolve");
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
