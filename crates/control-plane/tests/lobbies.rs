//! Hubs and game-mode lobbies end to end: lobby kinds run as instances,
//! `/v1/lobbies` lists the modes, `/v1/lobbies/send` moves players to the
//! least full lobby (or a hub), and an ending game sends its players to its
//! lobby before the instance stops. The mock orchestrator stands in for
//! Docker or Wings and the test plays each server and proxy.
//!
//! Needs `HALLOW_TEST_REDIS_URL` and `HALLOW_TEST_DATABASE_URL`, like
//! `instances.rs`; without them the tests pass without doing anything
//! (except in CI, where they fail).

use std::{sync::Arc, time::Duration};

use hallow_control_plane::{
    Config, InstancesSetup, OrchestratorChoice,
    instances::Settings,
    orchestrator::{
        Orchestrator,
        docker::{DockerConfig, Endpoint},
        mock::{Call, MockOrchestrator},
    },
    serve_with,
};
use hallow_db::sqlx::{self, Executor, postgres::PgPoolOptions};
use hallow_proto::{
    ApiError, ErrorCode, Poll,
    command::ProxyCommand,
    instance::{InstanceInfo, InstanceState, KindPlayers, KindRole, KindScaling, KindSpec},
    lobby::{GameMode, LobbyOverview, LobbySent, SendToLobby},
    presence::{LoginDecision, LoginRequest, PlayerSwitched},
    proxy::RegisterProxy,
    queue::JoinQueue,
    routes,
    server::{RegisterServer, ServerHeartbeat, ServerPlayer, ServerRole, ServerState},
};
use reqwest::StatusCode;
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

const TOKEN: &str = "test-token-0123456789";
/// The hub server's scoped token.
const HUB_TOKEN: &str = "hub-scoped-token-0123456789";

fn env_or_skip(var: &str) -> Option<String> {
    let value = std::env::var(var).ok();
    if value.is_none() {
        assert!(std::env::var_os("CI").is_none(), "{var} isn't set in CI");
        eprintln!("{var} isn't set; skipping");
    }
    value
}

struct Env {
    base: String,
    http: reqwest::Client,
    mock: Arc<MockOrchestrator>,
    admin: hallow_db::PgPool,
    db_name: String,
}

impl Env {
    async fn start() -> Option<Self> {
        let redis = env_or_skip("HALLOW_TEST_REDIS_URL")?;
        let db_url = env_or_skip("HALLOW_TEST_DATABASE_URL")?;
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&db_url)
            .await
            .unwrap();
        let db_name = format!("hallow_inst_test_{}", Uuid::new_v4().simple());
        admin
            .execute(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db_name}")))
            .await
            .unwrap();
        let (base_url, _) = db_url.rsplit_once('/').unwrap();

        let mut config = Config::new(redis, TOKEN);
        config.key_prefix = format!("test-{}", Uuid::new_v4());
        // The hub's own scoped token, as the compose hub would have.
        config.server_tokens =
            hallow_control_plane::auth::parse_static_tokens(&format!("lobby:hub:{HUB_TOKEN}"))
                .unwrap();
        config.database_url = Some(format!("{base_url}/{db_name}"));
        // The choice is replaced by the mock below.
        let mut setup = InstancesSetup::new(
            OrchestratorChoice::Docker(DockerConfig::new(
                Endpoint::Unix("/nonexistent".into()),
                "unused",
            )),
            "control-plane:8080",
        );
        setup
            .env
            .insert("HALLOW_VINE_SECRET".into(), "s3cret".into());
        setup.settings = Settings {
            start_timeout_ms: 3000,
            empty_grace_ms: 500,
            idle_ms: 500,
            retry_ms: 300,
            stuck_ms: 5000,
        };
        config.instances = Some(setup);
        let mock = Arc::new(MockOrchestrator::new(&["dock"]));

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let orchestrator: Arc<dyn Orchestrator> = mock.clone();
        tokio::spawn(async move {
            serve_with(listener, None, config, None, Some(orchestrator))
                .await
                .unwrap();
        });
        let http = reqwest::Client::new();
        for _ in 0..100 {
            if http
                .get(format!("{base}/healthz"))
                .send()
                .await
                .is_ok_and(|r| r.status() == StatusCode::NO_CONTENT)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Some(Self {
            base,
            http,
            mock,
            admin,
            db_name,
        })
    }

    async fn drop(self) {
        self.admin
            .execute(sqlx::AssertSqlSafe(format!(
                "DROP DATABASE {} WITH (FORCE)",
                self.db_name
            )))
            .await
            .unwrap();
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> reqwest::Response {
        req.bearer_auth(TOKEN).send().await.unwrap()
    }

    async fn post<B: Serialize>(&self, path: &str, body: &B) -> reqwest::Response {
        self.send(self.http.post(format!("{}{path}", self.base)).json(body))
            .await
    }

    async fn put<B: Serialize>(&self, path: &str, body: &B) -> reqwest::Response {
        self.send(self.http.put(format!("{}{path}", self.base)).json(body))
            .await
    }

    async fn delete(&self, path: &str) -> reqwest::Response {
        self.send(self.http.delete(format!("{}{path}", self.base)))
            .await
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> T {
        let res = self
            .send(self.http.get(format!("{}{path}", self.base)))
            .await;
        assert!(res.status().is_success(), "GET {path}: {}", res.status());
        res.json().await.unwrap()
    }

    async fn instances(&self) -> Vec<InstanceInfo> {
        self.get_json(routes::INSTANCES).await
    }

    /// Polls until `check` holds for the instance list, or panics.
    async fn wait_for(
        &self,
        what: &str,
        check: impl Fn(&[InstanceInfo]) -> bool,
    ) -> Vec<InstanceInfo> {
        for _ in 0..100 {
            let instances = self.instances().await;
            if check(&instances) {
                return instances;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("timed out waiting for {what}: {:?}", self.instances().await);
    }

    async fn online_player(&self, proxy: &str) -> Uuid {
        let uuid = Uuid::new_v4();
        let res = self
            .post(
                routes::PRESENCE_LOGIN,
                &LoginRequest {
                    uuid,
                    username: format!("p{}", &uuid.simple().to_string()[..6]),
                    proxy: proxy.into(),
                    protocol: Some(777),
                    ip: None,
                },
            )
            .await;
        let decision: LoginDecision = res.json().await.unwrap();
        assert!(
            matches!(decision, LoginDecision::Allow { .. }),
            "{decision:?}"
        );
        uuid
    }

    /// Plays an instance's server: registers it like the server plugin does.
    async fn boot(&self, name: &str, kind: &str, role: ServerRole, max_players: u32) {
        let res = self
            .post(
                routes::SERVERS,
                &RegisterServer {
                    name: name.into(),
                    kind: kind.into(),
                    version: "dev".into(),
                    role,
                    address: format!("{name}:25565"),
                    max_players,
                },
            )
            .await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    async fn heartbeat(&self, name: &str, players: &[Uuid], state: ServerState) {
        let roster = players
            .iter()
            .map(|uuid| ServerPlayer {
                uuid: *uuid,
                username: format!("p{}", &uuid.simple().to_string()[..6]),
            })
            .collect::<Vec<_>>();
        let beat = ServerHeartbeat {
            players: u32::try_from(players.len()).unwrap(),
            state,
            tick_ms: None,
            roster: Some(roster),
        };
        let res = self.post(&routes::server_heartbeat(name), &beat).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }
}

impl Env {
    /// Every `Send` queued for proxy-1 so far, in order.
    async fn sends(&self) -> Vec<(Uuid, String)> {
        let poll: Poll<Vec<ProxyCommand>> = self
            .get_json(&routes::since_wait(
                &routes::proxy_commands("proxy-1"),
                0,
                0,
            ))
            .await;
        match poll {
            Poll::Changed { data, .. } => data
                .into_iter()
                .filter_map(|c| match c {
                    ProxyCommand::Send { player, server } => Some((player, server)),
                    _ => None,
                })
                .collect(),
            Poll::Unchanged { .. } => Vec::new(),
        }
    }

    /// Polls until `player` has been sent to `server`, or panics.
    async fn wait_sent(&self, player: Uuid, server: &str) {
        for _ in 0..100 {
            if self
                .sends()
                .await
                .iter()
                .any(|(p, s)| *p == player && s == server)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "{player} was never sent to {server}: {:?}",
            self.sends().await
        );
    }

    async fn switched(&self, player: Uuid, server: &str) {
        let res = self
            .post(
                routes::PRESENCE_SWITCH,
                &PlayerSwitched {
                    uuid: player,
                    server: server.into(),
                },
            )
            .await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    async fn send_to(
        &self,
        player: Uuid,
        lobby: Option<&str>,
        fallback_hub: bool,
    ) -> reqwest::Response {
        self.post(
            routes::LOBBIES_SEND,
            &SendToLobby {
                player,
                lobby: lobby.map(str::to_owned),
                fallback_hub,
            },
        )
        .await
    }
}

fn kind(role: KindRole, lobby: Option<&str>, warm: u32, max: u32) -> KindSpec {
    KindSpec {
        image: "hallow-test:dev".into(),
        role,
        lobby: lobby.map(str::to_owned),
        players: KindPlayers { min: 1, max },
        scaling: KindScaling {
            warm,
            max_instances: Some(4),
        },
        env: std::collections::BTreeMap::new(),
        memory_mb: None,
        updated_by: Some("test".into()),
    }
}

async fn setup_network(env: &Env) {
    let res = env
        .post(
            routes::PROXIES,
            &RegisterProxy {
                id: "proxy-1".into(),
                address: "proxy-1:25565".into(),
                version: "test".into(),
            },
        )
        .await;
    assert!(res.status().is_success());
    env.boot("lobby", "hub", ServerRole::Lobby, 100).await;
}

/// The hub's game menu and `/lobby`: a lobby kind keeps a warm instance,
/// players reach it by mode name, and `/hub` brings them back.
#[tokio::test]
async fn players_reach_a_modes_lobby_and_come_back() {
    let Some(env) = Env::start().await else {
        return;
    };
    for (name, spec) in [
        ("test-game-lobby", kind(KindRole::Lobby, Some("hub"), 1, 10)),
        (
            "test-game",
            kind(KindRole::Game, Some("test-game-lobby"), 0, 4),
        ),
    ] {
        let res = env.put(&routes::kind(name), &spec).await;
        assert_eq!(res.status(), StatusCode::OK, "{name}");
    }
    setup_network(&env).await;
    let player = env.online_player("proxy-1").await;
    env.switched(player, "lobby").await;

    // The lobby kind keeps one warm instance; the mode shows while it starts.
    let started = env
        .wait_for("a lobby instance", |list| {
            list.len() == 1 && list[0].kind == "test-game-lobby"
        })
        .await;
    let lobby = started[0].name.clone();
    let overview: LobbyOverview = env.get_json(routes::LOBBIES).await;
    assert_eq!(overview.online, 1);
    assert_eq!(overview.hub, "hub");
    assert_eq!(
        overview.modes,
        [GameMode {
            name: "test-game".into(),
            lobby: "test-game-lobby".into(),
            players: 0,
            lobbies: 0,
        }]
    );
    // Not up yet: the player is told to try again.
    let res = env.send_to(player, Some("test-game"), false).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let err: ApiError = res.json().await.unwrap();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert!(
        err.message.contains("No test-game lobby"),
        "{}",
        err.message
    );

    env.boot(&lobby, "test-game-lobby", ServerRole::Lobby, 10)
        .await;
    env.wait_for("the lobby to be ready", |list| {
        list.iter()
            .any(|i| i.name == lobby && i.state == InstanceState::Ready)
    })
    .await;
    let overview: LobbyOverview = env.get_json(routes::LOBBIES).await;
    assert_eq!(overview.modes[0].lobbies, 1);

    // By mode name, by lobby kind, or by the game's kind.
    for name in ["test-game", "test-game-lobby", "TEST-GAME"] {
        let res = env.send_to(player, Some(name), false).await;
        assert_eq!(res.status(), StatusCode::OK, "{name}");
        let sent: LobbySent = res.json().await.unwrap();
        assert_eq!(sent.server, lobby);
        assert_eq!(sent.kind, "test-game-lobby");
    }
    env.wait_sent(player, &lobby).await;
    env.switched(player, &lobby).await;
    env.heartbeat(&lobby, &[player], ServerState::Ready).await;
    let overview: LobbyOverview = env.get_json(routes::LOBBIES).await;
    assert_eq!(overview.modes[0].players, 1);

    // Already there.
    let res = env.send_to(player, Some("test-game"), false).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let err: ApiError = res.json().await.unwrap();
    assert_eq!(err.message, "You're already in test-game.");
    // Unknown modes are refused, unless a hub will do.
    let res = env.send_to(player, Some("nope"), false).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let err: ApiError = res.json().await.unwrap();
    assert_eq!(err.message, "There's no game mode called nope.");
    let res = env.send_to(player, Some("nope"), true).await;
    let sent: LobbySent = res.json().await.unwrap();
    assert_eq!(sent.server, "lobby");

    // `/hub`.
    let res = env.send_to(player, None, false).await;
    assert_eq!(res.status(), StatusCode::OK);
    let sent: LobbySent = res.json().await.unwrap();
    assert_eq!((sent.server.as_str(), sent.kind.as_str()), ("lobby", "hub"));
    env.switched(player, "lobby").await;
    let res = env.send_to(player, Some("hub"), false).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let err: ApiError = res.json().await.unwrap();
    assert_eq!(err.message, "You're already in the hub.");

    // Players who aren't online can't be sent.
    let res = env.send_to(Uuid::new_v4(), None, false).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    env.drop().await;
}

/// A game instance that ends with players on it (staff ended it, it timed
/// out) sends them to its kind's lobby before it stops.
#[tokio::test]
async fn an_ending_game_sends_its_players_to_its_lobby_first() {
    let Some(env) = Env::start().await else {
        return;
    };
    for (name, spec) in [
        ("test-game-lobby", kind(KindRole::Lobby, Some("hub"), 1, 10)),
        (
            "test-game",
            kind(KindRole::Game, Some("test-game-lobby"), 0, 4),
        ),
    ] {
        let res = env.put(&routes::kind(name), &spec).await;
        assert_eq!(res.status(), StatusCode::OK, "{name}");
    }
    setup_network(&env).await;
    let player = env.online_player("proxy-1").await;
    let started = env
        .wait_for("a lobby instance", |list| list.len() == 1)
        .await;
    let lobby = started[0].name.clone();
    env.boot(&lobby, "test-game-lobby", ServerRole::Lobby, 10)
        .await;

    let res = env
        .post(
            routes::QUEUES,
            &JoinQueue {
                kind: "test-game".into(),
                player,
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let list = env
        .wait_for("a game instance", |list| {
            list.iter().any(|i| i.kind == "test-game")
        })
        .await;
    let game = list
        .iter()
        .find(|i| i.kind == "test-game")
        .unwrap()
        .name
        .clone();
    env.boot(&game, "test-game", ServerRole::Game, 4).await;
    env.wait_sent(player, &game).await;
    env.switched(player, &game).await;
    env.heartbeat(&game, &[player], ServerState::InGame).await;
    env.wait_for("the game to count its player", |list| {
        list.iter().any(|i| i.name == game && i.players == 1)
    })
    .await;

    // Staff end it: the player goes to the lobby, then it stops.
    let res = env.delete(&routes::instance(&game)).await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    env.wait_sent(player, &lobby).await;
    assert!(
        !env.mock.calls().contains(&Call::Stop(game.clone())),
        "stopped before its player left"
    );
    env.switched(player, &lobby).await;
    env.heartbeat(&game, &[], ServerState::InGame).await;
    env.wait_for("the game to be deleted", |list| {
        list.iter().all(|i| i.name != game)
    })
    .await;
    assert!(env.mock.calls().contains(&Call::Stop(game.clone())));
    env.drop().await;
}

/// A kind registered a moment ago can be queued for at once, before the
/// next plan has read it.
#[tokio::test]
async fn a_game_kind_can_be_queued_for_as_soon_as_it_is_registered() {
    let Some(env) = Env::start().await else {
        return;
    };
    setup_network(&env).await;
    let player = env.online_player("proxy-1").await;
    let join = JoinQueue {
        kind: "fresh-game".into(),
        player,
    };
    // Not registered: refused.
    assert_eq!(
        env.post(routes::QUEUES, &join).await.status(),
        StatusCode::CONFLICT
    );
    let res = env
        .put(
            &routes::kind("fresh-game"),
            &kind(KindRole::Game, Some("hub"), 0, 4),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        env.post(routes::QUEUES, &join).await.status(),
        StatusCode::OK
    );
    env.drop().await;
}

/// The hub's scoped token reads the modes and sends its own players, and
/// nobody else's.
#[tokio::test]
async fn a_servers_token_sends_only_its_own_players() {
    let Some(env) = Env::start().await else {
        return;
    };
    setup_network(&env).await;
    let here = env.online_player("proxy-1").await;
    env.switched(here, "lobby").await;
    let elsewhere = env.online_player("proxy-1").await;
    env.switched(elsewhere, "some-game").await;
    let as_hub = |req: reqwest::RequestBuilder| async move {
        req.bearer_auth(HUB_TOKEN).send().await.unwrap()
    };
    let url = |path: &str| format!("{}{path}", env.base);

    let res = as_hub(env.http.get(url(routes::LOBBIES))).await;
    assert_eq!(res.status(), StatusCode::OK);
    let overview: LobbyOverview = res.json().await.unwrap();
    assert_eq!(overview.online, 2);
    let send = |player| SendToLobby {
        player,
        lobby: Some("nope".into()),
        fallback_hub: true,
    };
    // Its own player: through to the handler (already on the hub, so a
    // fallback finds no other hub with room than itself).
    let res = as_hub(env.http.post(url(routes::LOBBIES_SEND)).json(&send(here))).await;
    assert_eq!(res.status(), StatusCode::OK);
    let res = as_hub(
        env.http
            .post(url(routes::LOBBIES_SEND))
            .json(&send(elsewhere)),
    )
    .await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let res = as_hub(env.http.get(url(routes::INSTANCES))).await;
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    env.drop().await;
}
