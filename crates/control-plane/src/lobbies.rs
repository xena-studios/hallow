//! Hubs and game-mode lobbies (`hallow_proto::lobby`): which modes there
//! are, and moving players to a mode's lobby or back to a hub.
//!
//! The modes are the lobby kinds the control plane knows of: the kinds of
//! registered lobby servers, and lobby kinds registered for instances (so a
//! mode shows while its first instance starts). A name resolves to a lobby
//! kind in this order:
//!
//! 1. the hub kind (or `hub`);
//! 2. a lobby kind by its own name (`skywars-lobby`);
//! 3. a mode name: the lobby kind `<name>-lobby`;
//! 4. a registered game kind, by the `lobby` its `hallow.toml` names.
//!
//! A player is sent to the least full ready instance of the kind
//! (`Store::pick_lobby`). Ending a game instance first sends anyone still on
//! it to its kind's `lobby` ([`evacuate`]).

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};

use axum::extract::State;
use hallow_proto::{
    ErrorCode,
    instance::{KindRole, KindSpec},
    lobby::{GameMode, LobbyOverview, LobbySent, SendToLobby, mode_name},
    server::{ServerInfo, ServerRole, ServerState},
};

use crate::{
    AppState,
    error::Failure,
    store::{Store, StoreError},
};

/// The modes and how names resolve, from the servers and kinds.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    pub hub: String,
    /// Lobby kinds, not counting the hub.
    pub lobbies: BTreeSet<String>,
    /// Game kind → the lobby kind its players return to.
    pub game_lobby: BTreeMap<String, String>,
}

impl Catalog {
    #[must_use]
    pub fn new(hub: &str, servers: &[ServerInfo], kinds: &[(String, KindSpec)]) -> Self {
        let mut lobbies: BTreeSet<String> = servers
            .iter()
            .filter(|s| s.role == ServerRole::Lobby && s.kind != hub)
            .map(|s| s.kind.clone())
            .collect();
        let mut game_lobby = BTreeMap::new();
        for (name, spec) in kinds {
            match spec.role {
                KindRole::Lobby if name != hub => {
                    lobbies.insert(name.clone());
                }
                KindRole::Game => {
                    if let Some(lobby) = &spec.lobby {
                        game_lobby.insert(name.clone(), lobby.clone());
                    }
                }
                _ => {}
            }
        }
        Self {
            hub: hub.to_owned(),
            lobbies,
            game_lobby,
        }
    }

    /// The lobby kind `name` means; see the module docs.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<String> {
        let name = name.trim().to_ascii_lowercase();
        if name == self.hub || name == "hub" {
            return Some(self.hub.clone());
        }
        if self.lobbies.contains(&name) {
            return Some(name);
        }
        let lobby = format!("{name}-lobby");
        if self.lobbies.contains(&lobby) {
            return Some(lobby);
        }
        self.game_lobby.get(&name).cloned()
    }

    /// What players call `kind`: "the hub" or the mode's name.
    #[must_use]
    pub fn label(&self, kind: &str) -> String {
        if kind == self.hub {
            "the hub".to_owned()
        } else {
            mode_name(kind).to_owned()
        }
    }

    /// Every mode with its players (lobbies and games) and ready lobbies.
    #[must_use]
    pub fn modes(&self, servers: &[ServerInfo], closed: &[String]) -> Vec<GameMode> {
        self.lobbies
            .iter()
            .map(|lobby| {
                let name = mode_name(lobby);
                let players = servers
                    .iter()
                    .filter(|s| {
                        s.kind == *lobby
                            || self.game_lobby.get(&s.kind) == Some(lobby)
                            || (s.role == ServerRole::Game && s.kind == name)
                    })
                    .map(|s| s.players)
                    .sum();
                let lobbies = servers
                    .iter()
                    .filter(|s| {
                        s.kind == *lobby
                            && s.role == ServerRole::Lobby
                            && s.state == ServerState::Ready
                            && !closed.contains(&s.name)
                    })
                    .count();
                GameMode {
                    name: name.to_owned(),
                    lobby: lobby.clone(),
                    players,
                    lobbies: u32::try_from(lobbies).unwrap_or(u32::MAX),
                }
            })
            .collect()
    }
}

/// Registered kinds, if there's a database to keep them.
async fn kinds(state: &AppState) -> Result<Vec<(String, KindSpec)>, Failure> {
    let Some(db) = &state.db else {
        return Ok(Vec::new());
    };
    Ok(hallow_db::list_kinds(db)
        .await?
        .into_iter()
        .map(|k| (k.name, k.spec.0))
        .collect())
}

async fn catalog(state: &AppState) -> Result<(Catalog, Vec<ServerInfo>), Failure> {
    let (_, servers) = state.store.read_servers().await?;
    let kinds = kinds(state).await?;
    Ok((
        Catalog::new(&state.config.hub_kind, &servers, &kinds),
        servers,
    ))
}

/// `GET /v1/lobbies`.
pub async fn overview(
    State(state): State<Arc<AppState>>,
) -> Result<axum::Json<LobbyOverview>, Failure> {
    let (catalog, servers) = catalog(&state).await?;
    let closed: Vec<String> = state
        .store
        .instances()
        .await?
        .into_iter()
        .filter(|i| !i.state.is_live())
        .map(|i| i.name)
        .collect();
    Ok(axum::Json(LobbyOverview {
        online: state.store.online().await?,
        modes: catalog.modes(&servers, &closed),
        hub: catalog.hub,
    }))
}

/// `POST /v1/lobbies/send`.
pub async fn send(
    State(state): State<Arc<AppState>>,
    req: Result<axum::Json<SendToLobby>, axum::extract::rejection::JsonRejection>,
) -> Result<axum::Json<LobbySent>, Failure> {
    let axum::Json(req) = req?;
    let Some(location) = state.store.player(&req.player).await? else {
        return Err(Failure::not_found("player isn't online"));
    };
    let (catalog, servers) = catalog(&state).await?;
    let kind = match &req.lobby {
        None => catalog.hub.clone(),
        Some(name) => match catalog.resolve(name) {
            Some(kind) => kind,
            None if req.fallback_hub => catalog.hub.clone(),
            None => {
                return Err(Failure::not_found(format!(
                    "There's no game mode called {name}."
                )));
            }
        },
    };
    let here = location
        .server
        .as_deref()
        .and_then(|name| servers.iter().find(|s| s.name == name));
    if !req.fallback_hub && here.is_some_and(|s| s.kind == kind) {
        return Err(Failure::new(
            ErrorCode::Conflict,
            format!("You're already in {}.", catalog.label(&kind)),
        ));
    }
    let mut target = state.store.pick_lobby(&kind).await?;
    if target.is_none() && req.fallback_hub && kind != catalog.hub {
        target = state.store.pick_lobby(&catalog.hub).await?;
    }
    let Some(server) = target else {
        return Err(Failure::new(
            ErrorCode::Conflict,
            format!(
                "No {} lobby has room right now. Try again in a moment.",
                catalog.label(&kind)
            ),
        ));
    };
    state
        .store
        .move_player(&location.proxy, req.player, &server.name)
        .await?;
    tracing::info!(player = %req.player, server = %server.name, "sent to a lobby");
    Ok(axum::Json(LobbySent {
        server: server.name,
        kind: server.kind,
    }))
}

/// Sends everyone still on `server` to a `lobby` instance (a hub if it's
/// `None` or none has room), then waits up to `wait` for the server to
/// report itself empty. Returns how many players were sent. Used before an
/// instance is stopped, so nobody is dropped by the stop.
pub async fn evacuate(
    store: &Store,
    server: &str,
    lobby: Option<&str>,
    hub: &str,
    wait: Duration,
) -> Result<usize, StoreError> {
    let Some(players) = store.server_players(server).await? else {
        return Ok(0);
    };
    let mut target = None;
    if let Some(lobby) = lobby {
        target = store.pick_lobby(lobby).await?;
    }
    if target.is_none() {
        target = store.pick_lobby(hub).await?;
    }
    let Some(target) = target.filter(|t| t.name != server) else {
        return Ok(0);
    };
    let mut sent = 0;
    for player in &players {
        if let Some(location) = store.player(&player.uuid).await?
            && location.server.as_deref() == Some(server)
        {
            store
                .move_player(&location.proxy, player.uuid, &target.name)
                .await?;
            sent += 1;
        }
    }
    if sent > 0 {
        let deadline = tokio::time::Instant::now() + wait;
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let left = store.server_players(server).await?.map_or(0, |p| p.len());
            if left == 0 {
                break;
            }
        }
        tracing::info!(%server, to = %target.name, sent, "sent players off an ending instance");
    }
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hallow_proto::instance::{KindPlayers, KindScaling};

    fn server(name: &str, kind: &str, role: ServerRole, players: u32) -> ServerInfo {
        ServerInfo {
            name: name.into(),
            kind: kind.into(),
            version: "dev".into(),
            role,
            address: format!("{name}:25565"),
            max_players: 16,
            players,
            state: ServerState::Ready,
            tick_ms: None,
        }
    }

    fn kind(role: KindRole, lobby: Option<&str>) -> KindSpec {
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

    fn world() -> (Catalog, Vec<ServerInfo>) {
        let servers = vec![
            server("lobby", "hub", ServerRole::Lobby, 3),
            server("sw-lobby-1", "skywars-lobby", ServerRole::Lobby, 4),
            server("sw-lobby-2", "skywars-lobby", ServerRole::Lobby, 1),
            server("skywars-a1", "skywars", ServerRole::Game, 8),
            server("duels-b2", "duels", ServerRole::Game, 2),
        ];
        let kinds = vec![
            ("hub".to_owned(), kind(KindRole::Hub, None)),
            ("bedwars-lobby".to_owned(), kind(KindRole::Lobby, None)),
            (
                "skywars".to_owned(),
                kind(KindRole::Game, Some("skywars-lobby")),
            ),
            ("duels".to_owned(), kind(KindRole::Game, Some("hub"))),
        ];
        (Catalog::new("hub", &servers, &kinds), servers)
    }

    #[test]
    fn names_resolve() {
        let (catalog, _) = world();
        assert_eq!(
            catalog.lobbies,
            BTreeSet::from(["bedwars-lobby".to_owned(), "skywars-lobby".to_owned()])
        );
        let resolve = |name| catalog.resolve(name);
        assert_eq!(resolve("hub").as_deref(), Some("hub"));
        assert_eq!(resolve("SkyWars").as_deref(), Some("skywars-lobby"));
        assert_eq!(resolve("skywars-lobby").as_deref(), Some("skywars-lobby"));
        // Known only as a registered kind, with no server yet.
        assert_eq!(resolve("bedwars").as_deref(), Some("bedwars-lobby"));
        // A game whose lobby is the hub.
        assert_eq!(resolve("duels").as_deref(), Some("hub"));
        assert_eq!(resolve("nope"), None);
        assert_eq!(catalog.label("hub"), "the hub");
        assert_eq!(catalog.label("skywars-lobby"), "skywars");
    }

    #[test]
    fn modes_count_lobbies_and_games() {
        let (catalog, servers) = world();
        let modes = catalog.modes(&servers, &["sw-lobby-2".to_owned()]);
        assert_eq!(
            modes,
            vec![
                GameMode {
                    name: "bedwars".into(),
                    lobby: "bedwars-lobby".into(),
                    players: 0,
                    lobbies: 0,
                },
                GameMode {
                    name: "skywars".into(),
                    lobby: "skywars-lobby".into(),
                    players: 13,
                    lobbies: 1,
                },
            ]
        );
    }

    #[test]
    fn without_kinds_games_count_by_name() {
        let servers = vec![
            server("tg-lobby", "test-game-lobby", ServerRole::Lobby, 1),
            server("test-game-1", "test-game", ServerRole::Game, 2),
        ];
        let catalog = Catalog::new("hub", &servers, &[]);
        assert_eq!(catalog.modes(&servers, &[])[0].players, 3);
        assert_eq!(
            catalog.resolve("test-game").as_deref(),
            Some("test-game-lobby")
        );
    }
}
