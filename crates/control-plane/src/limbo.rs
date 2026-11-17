//! Never dropped: wherever the network can't place a player, they wait in
//! limbo and are moved on by themselves (docs/ARCHITECTURE.md, "Never
//! dropped").
//!
//! - **Limbo** is the proxies' built-in server ([`Config::limbo`]); Vine
//!   answers it itself, so it is there whenever the proxy is. Players wait
//!   there when the network is full, no hub is free, a kick holds them, or
//!   the server they were on went away and nothing else had room.
//! - **Waiting** players are kept in Redis oldest first ([`Store::waiting`]).
//!   Every matchmaker pass ([`tend`]) sends each one on as soon as there's
//!   room: a hub, or a banned lobby for a banned player, and tells those
//!   still waiting their place now and then.
//! - **Banned players** are `restricted`: only the control plane moves them,
//!   to a banned lobby ([`Config::banned_kind`]) or limbo. Their proxy
//!   refuses `/server`, and the control plane refuses `/lobby`, `/hub`,
//!   queues, party warps and sends for them. When the ban ends ([`tend`])
//!   or is lifted ([`unbanned`]) they go to a hub without reconnecting.
//! - **Fallbacks.** A server that kicks a player, crashes or stops sends
//!   them, through their proxy, to [`fallback`]: a game's lobby, another
//!   lobby of the same kind, a hub, or limbo.
//! - **Kicks** ([`kick`]) are never disconnects: the player waits in limbo
//!   for [`Config::kick_hold`] with the reason on screen, then goes to a hub.
//!
//! [`Config::limbo`]: crate::Config::limbo
//! [`Config::banned_kind`]: crate::Config::banned_kind
//! [`Config::kick_hold`]: crate::Config::kick_hold

use std::time::{SystemTime, UNIX_EPOCH};

use hallow_proto::{
    command::ProxyCommand,
    presence::{Fallback, FallbackRequest},
    punishment::BanInfo,
    server::{ServerInfo, ServerRole},
};
use uuid::Uuid;

use crate::{
    AppState, Config,
    lobbies::Catalog,
    store::{Result, Store, lobby::least_full},
};

/// A waiting player sent on isn't sent again for this long: their proxy
/// keeps them in limbo if the move fails, and the next try comes then.
pub const RESEND_MS: u64 = 10_000;
/// How often a waiting player is told their place.
pub const TELL_EVERY_MS: u64 = 30_000;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The kind a server ran, even after it unregistered: the server list, then
/// the instance records, then its name (`<kind>-<6 hex>`).
#[must_use]
pub fn kind_of(
    server: &str,
    servers: &[ServerInfo],
    instances: &[(String, String)],
    known: &dyn Fn(&str) -> bool,
) -> Option<String> {
    if let Some(s) = servers.iter().find(|s| s.name == server) {
        return Some(s.kind.clone());
    }
    if let Some((_, kind)) = instances.iter().find(|(name, _)| name == server) {
        return Some(kind.clone());
    }
    let (kind, suffix) = server.rsplit_once('-')?;
    (suffix.len() == 6 && suffix.chars().all(|c| c.is_ascii_hexdigit()) && known(kind))
        .then(|| kind.to_owned())
}

/// Where a player who lost `from` should go, in order of preference: the
/// kinds to try. A game goes back to its lobby, a lobby to another of its
/// kind, and everything ends at the hub. Banned players are handled apart.
#[must_use]
pub fn fallback_kinds(
    kind: Option<&str>,
    role: Option<ServerRole>,
    catalog: &Catalog,
    banned_kind: &str,
) -> Vec<String> {
    let mut kinds = Vec::new();
    if let Some(kind) = kind {
        if let Some(lobby) = catalog.game_lobby.get(kind) {
            kinds.push(lobby.clone());
        } else if role != Some(ServerRole::Game)
            && kind != banned_kind
            && (catalog.lobbies.contains(kind) || kind == catalog.hub)
        {
            kinds.push(kind.to_owned());
        }
    }
    if !kinds.contains(&catalog.hub) {
        kinds.push(catalog.hub.clone());
    }
    kinds
}

/// `POST /v1/presence/fallback`: a server dropped the player. `None` if
/// they aren't online.
pub async fn fallback(state: &AppState, req: &FallbackRequest) -> Result<Option<Fallback>> {
    let store = &state.store;
    let config = &state.config;
    if store.player(&req.uuid).await?.is_none() {
        return Ok(None);
    }
    let mut closed = store.closed_instances().await?;
    closed.push(req.from.clone());
    let (_, servers) = store.read_servers().await?;
    let what = match req.reason.as_deref().map(str::trim) {
        Some(reason) if !reason.is_empty() => {
            format!("§c{} sent you away: §f{reason}", req.from)
        }
        _ => format!("§c{} went offline.", req.from),
    };
    if store.restriction(&req.uuid).await?.is_some() {
        let server = least_full(&servers, &config.banned_kind, &closed).map(|s| s.name.clone());
        return Ok(Some(
            settle(store, config, &req.uuid, server, String::new()).await?,
        ));
    }
    let kinds = crate::lobbies::kinds(state).await.unwrap_or_default();
    let catalog = Catalog::new(&config.hub_kind, &config.banned_kind, &servers, &kinds);
    let instances: Vec<(String, String)> = store
        .instances()
        .await?
        .into_iter()
        .map(|i| (i.name, i.kind))
        .collect();
    let known = |kind: &str| {
        kind == catalog.hub
            || catalog.lobbies.contains(kind)
            || catalog.game_lobby.contains_key(kind)
            || kinds.iter().any(|(name, _)| name == kind)
    };
    let kind = kind_of(&req.from, &servers, &instances, &known);
    let role = servers.iter().find(|s| s.name == req.from).map(|s| s.role);
    let mut server = None;
    let mut label = String::new();
    for kind in fallback_kinds(kind.as_deref(), role, &catalog, &config.banned_kind) {
        if let Some(found) = least_full(&servers, &kind, &closed) {
            server = Some(found.name.clone());
            label = catalog.label(&kind);
            break;
        }
    }
    let message = match &server {
        Some(_) => format!("{what} §7You're in {label} now."),
        None => format!("{what} §7Nowhere has room yet; you'll be moved on in a moment."),
    };
    Ok(Some(
        settle(store, config, &req.uuid, server, message).await?,
    ))
}

/// The answer for `server`, or limbo (and a place in line) without one.
async fn settle(
    store: &Store,
    config: &Config,
    player: &Uuid,
    server: Option<String>,
    message: String,
) -> Result<Fallback> {
    let server = match server {
        Some(server) => server,
        None => {
            store.start_waiting(player, now_ms()).await?;
            config.limbo.clone()
        }
    };
    Ok(Fallback { server, message })
}

/// One pass over bans that ran out and players waiting in limbo. Runs under
/// the matchmaker lock, so only one control plane instance does it at a time.
pub async fn tend(state: &AppState) -> Result<()> {
    let store = &state.store;
    let config = &state.config;
    let now = now_ms();
    for (player, ban) in store.restricted().await? {
        if ban.expires_ms.is_some_and(|at| at <= now) {
            tracing::info!(%player, id = ban.id, "ban ended; releasing the player");
            release(
                store,
                config,
                &player,
                "§aYour ban has ended. Welcome back!",
            )
            .await?;
        }
    }

    let waiting = store.waiting().await?;
    if waiting.is_empty() {
        return Ok(());
    }
    let closed = store.closed_instances().await?;
    let (_, mut servers) = store.read_servers().await?;
    let online = store.online().await?;
    let mut placed = online.saturating_sub(u32::try_from(waiting.len()).unwrap_or(u32::MAX));
    let mut position = 0u32;
    for entry in waiting {
        let Some(location) = store.player(&entry.uuid).await? else {
            store.stop_waiting(&entry.uuid).await?;
            continue;
        };
        let restricted = store.restriction(&entry.uuid).await?.is_some();
        if !restricted {
            position += 1;
        }
        match location.server.as_deref() {
            // Still on their way to limbo (or to where we sent them).
            None => continue,
            Some(server) if server != config.limbo => {
                // Placed somewhere since; they no longer wait.
                store.stop_waiting(&entry.uuid).await?;
                continue;
            }
            Some(_) => {}
        }
        if entry.hold_until_ms.is_some_and(|until| until > now)
            || entry
                .sent_ms
                .is_some_and(|at| now.saturating_sub(at) < RESEND_MS)
        {
            continue;
        }
        let target = if restricted {
            least_full(&servers, &config.banned_kind, &closed).map(|s| s.name.clone())
        } else if placed >= config.max_players {
            None
        } else {
            least_full(&servers, &config.hub_kind, &closed).map(|s| s.name.clone())
        };
        if let Some(target) = target {
            store
                .move_player(&location.proxy, entry.uuid, &target)
                .await?;
            store.mark_sent(&entry.uuid, now).await?;
            if let Some(s) = servers.iter_mut().find(|s| s.name == target) {
                s.players += 1;
            }
            if !restricted {
                placed += 1;
            }
            tracing::info!(player = %entry.uuid, server = %target, "moved out of limbo");
            continue;
        }
        if entry
            .told_ms
            .is_none_or(|at| now.saturating_sub(at) >= TELL_EVERY_MS)
        {
            let text = if restricted {
                "§7Waiting for the banned lobby to open...".to_owned()
            } else if placed >= config.max_players {
                format!(
                    "§eThe network is full. §7You're §f#{position}§7 in line and will be moved on by yourself."
                )
            } else {
                format!(
                    "§eNo hub is free yet. §7You're §f#{position}§7 in line and will be moved on by yourself."
                )
            };
            store
                .send_to_proxy(
                    &location.proxy,
                    &ProxyCommand::Message {
                        player: entry.uuid,
                        text,
                        from: None,
                    },
                )
                .await?;
            store.mark_told(&entry.uuid, now).await?;
        }
    }
    Ok(())
}

/// Players waiting in limbo per kind they wait for: the hub, or the banned
/// lobby. The instance planner starts instances for them.
pub async fn waiting_by_kind(
    store: &Store,
    config: &Config,
) -> Result<std::collections::BTreeMap<String, u32>> {
    let mut by_kind = std::collections::BTreeMap::new();
    for entry in store.waiting().await? {
        let kind = if store.restriction(&entry.uuid).await?.is_some() {
            &config.banned_kind
        } else {
            &config.hub_kind
        };
        *by_kind.entry(kind.clone()).or_insert(0) += 1;
    }
    Ok(by_kind)
}

/// A banned player is online: lock them in and move them to a banned lobby
/// (or limbo, waiting for one).
pub async fn banned(store: &Store, config: &Config, player: &Uuid, ban: &BanInfo) -> Result<()> {
    let Some(location) = store.player(player).await? else {
        return Ok(());
    };
    store.restrict(player, ban).await?;
    store.stop_waiting(player).await?;
    let proxy = &location.proxy;
    store
        .send_to_proxy(
            proxy,
            &ProxyCommand::Restrict {
                player: *player,
                ban: Some(ban.clone()),
            },
        )
        .await?;
    store
        .send_to_proxy(
            proxy,
            &ProxyCommand::Title {
                player: *player,
                title: "§cYou are banned".into(),
                subtitle: format!("§7{}", ban.reason),
            },
        )
        .await?;
    let target = store.pick_lobby(&config.banned_kind).await?;
    let server = match target {
        Some(lobby) => lobby.name,
        None => {
            store.start_waiting(player, now_ms()).await?;
            config.limbo.clone()
        }
    };
    if location.server.as_deref() != Some(server.as_str()) {
        store
            .send_to_proxy(
                proxy,
                &ProxyCommand::Send {
                    player: *player,
                    server,
                },
            )
            .await?;
    }
    Ok(())
}

/// Staff lifted the player's ban: unlock them and send them to a hub.
pub async fn unbanned(store: &Store, config: &Config, player: &Uuid) -> Result<()> {
    release(
        store,
        config,
        player,
        "§aYour ban was lifted. Welcome back!",
    )
    .await
}

async fn release(store: &Store, config: &Config, player: &Uuid, text: &str) -> Result<()> {
    store.unrestrict(player).await?;
    let Some(location) = store.player(player).await? else {
        return Ok(());
    };
    let proxy = &location.proxy;
    store
        .send_to_proxy(
            proxy,
            &ProxyCommand::Restrict {
                player: *player,
                ban: None,
            },
        )
        .await?;
    store
        .send_to_proxy(
            proxy,
            &ProxyCommand::Message {
                player: *player,
                text: text.to_owned(),
                from: None,
            },
        )
        .await?;
    match store.pick_lobby(&config.hub_kind).await? {
        Some(hub) => store.move_player(proxy, *player, &hub.name).await?,
        None => {
            // Wait in limbo; the next pass sends them on.
            store.start_waiting(player, now_ms()).await?;
            if location.server.as_deref() != Some(config.limbo.as_str()) {
                store
                    .send_to_proxy(
                        proxy,
                        &ProxyCommand::Send {
                            player: *player,
                            server: config.limbo.clone(),
                        },
                    )
                    .await?;
            }
        }
    }
    Ok(())
}

/// A kick: the player waits in limbo for [`Config::kick_hold`] with the
/// reason on screen, then goes to a hub. False if they aren't online.
pub async fn kick(store: &Store, config: &Config, player: &Uuid, reason: &str) -> Result<bool> {
    let Some(location) = store.player(player).await? else {
        return Ok(false);
    };
    let proxy = &location.proxy;
    let now = now_ms();
    let hold_ms = u64::try_from(config.kick_hold.as_millis()).unwrap_or(u64::MAX);
    store.start_waiting(player, now).await?;
    store.hold(player, now.saturating_add(hold_ms)).await?;
    store
        .send_to_proxy(
            proxy,
            &ProxyCommand::Title {
                player: *player,
                title: "§cYou were kicked".into(),
                subtitle: format!("§7{reason}"),
            },
        )
        .await?;
    store
        .send_to_proxy(
            proxy,
            &ProxyCommand::Message {
                player: *player,
                text: format!(
                    "§cYou were kicked. §7Reason: §f{reason}\n§7You'll be sent on in {}s.",
                    config.kick_hold.as_secs()
                ),
                from: None,
            },
        )
        .await?;
    if location.server.as_deref() != Some(config.limbo.as_str()) {
        store
            .send_to_proxy(
                proxy,
                &ProxyCommand::Send {
                    player: *player,
                    server: config.limbo.clone(),
                },
            )
            .await?;
    }
    Ok(true)
}

/// Why a restricted player can't go where they asked, for the player.
pub const LOCKED: &str = "You can't leave while you're banned.";

/// Whether `kind` is somewhere a banned player may be sent.
#[must_use]
pub fn allowed_while_banned(config: &Config, server_or_kind: &str) -> bool {
    server_or_kind == config.banned_kind || server_or_kind == config.limbo
}

#[cfg(test)]
mod tests {
    use super::*;
    use hallow_proto::{
        instance::{KindPlayers, KindRole, KindScaling, KindSpec},
        server::ServerState,
    };
    use std::collections::BTreeMap;

    fn server(name: &str, kind: &str, role: ServerRole) -> ServerInfo {
        ServerInfo {
            name: name.into(),
            kind: kind.into(),
            version: "dev".into(),
            role,
            address: format!("{name}:25565"),
            max_players: 10,
            players: 0,
            state: ServerState::Ready,
            tick_ms: None,
        }
    }

    fn spec(role: KindRole, lobby: Option<&str>) -> KindSpec {
        KindSpec {
            image: "img".into(),
            role,
            lobby: lobby.map(str::to_owned),
            players: KindPlayers { min: 1, max: 16 },
            scaling: KindScaling::default(),
            env: BTreeMap::new(),
            memory_mb: None,
            updated_by: None,
        }
    }

    #[test]
    fn kinds_survive_their_servers() {
        let servers = vec![server("lobby", "hub", ServerRole::Lobby)];
        let instances = vec![("sw-abc123".to_owned(), "skywars".to_owned())];
        let known = |kind: &str| kind == "bedwars";
        assert_eq!(
            kind_of("lobby", &servers, &instances, &known).as_deref(),
            Some("hub")
        );
        assert_eq!(
            kind_of("sw-abc123", &servers, &instances, &known).as_deref(),
            Some("skywars")
        );
        assert_eq!(
            kind_of("bedwars-0f9e8d", &servers, &instances, &known).as_deref(),
            Some("bedwars")
        );
        assert_eq!(
            kind_of("bedwars-zzzzzz", &servers, &instances, &known),
            None
        );
        assert_eq!(kind_of("cave-0f9e8d", &servers, &instances, &known), None);
    }

    #[test]
    fn games_fall_back_to_their_lobby_then_the_hub() {
        let servers = vec![
            server("lobby", "hub", ServerRole::Lobby),
            server("swl-1", "skywars-lobby", ServerRole::Lobby),
            server("banned", "banned", ServerRole::Lobby),
        ];
        let kinds = vec![
            (
                "skywars".to_owned(),
                spec(KindRole::Game, Some("skywars-lobby")),
            ),
            ("skywars-lobby".to_owned(), spec(KindRole::Lobby, None)),
        ];
        let catalog = Catalog::new("hub", "banned", &servers, &kinds);
        assert!(!catalog.lobbies.contains("banned"));
        let kinds_for = |kind: Option<&str>, role| fallback_kinds(kind, role, &catalog, "banned");
        assert_eq!(
            kinds_for(Some("skywars"), Some(ServerRole::Game)),
            ["skywars-lobby", "hub"]
        );
        assert_eq!(
            kinds_for(Some("skywars-lobby"), Some(ServerRole::Lobby)),
            ["skywars-lobby", "hub"]
        );
        assert_eq!(kinds_for(Some("hub"), Some(ServerRole::Lobby)), ["hub"]);
        // A game with no lobby, an unknown server, the banned lobby.
        assert_eq!(kinds_for(Some("duels"), Some(ServerRole::Game)), ["hub"]);
        assert_eq!(kinds_for(None, None), ["hub"]);
        assert_eq!(kinds_for(Some("banned"), Some(ServerRole::Lobby)), ["hub"]);
    }
}
