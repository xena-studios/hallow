//! The instance lifecycle end to end, with the mock orchestrator standing in
//! for Docker or Wings and the test playing each instance's server.
//!
//! Needs `HALLOW_TEST_REDIS_URL` and `HALLOW_TEST_DATABASE_URL`, like
//! `nodes.rs`; without them the tests pass without doing anything (except
//! in CI, where they fail). Each test gets its own database and key prefix.

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
    instance::{
        InstanceInfo, InstanceState, KindInfo, KindPlayers, KindRole, KindScaling, KindSpec,
    },
    presence::{LoginDecision, LoginRequest},
    proxy::RegisterProxy,
    queue::JoinQueue,
    routes,
    server::{RegisterServer, ServerHeartbeat, ServerInfo, ServerPlayer, ServerRole, ServerState},
};
use reqwest::StatusCode;
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

const TOKEN: &str = "test-token-0123456789";

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
        Self::start_with(|_| {}).await
    }

    async fn start_with(change: impl FnOnce(&mut Config)) -> Option<Self> {
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
        change(&mut config);
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
    async fn boot(&self, name: &str, kind: &str, max_players: u32) {
        let res = self
            .post(
                routes::SERVERS,
                &RegisterServer {
                    name: name.into(),
                    kind: kind.into(),
                    version: "dev".into(),
                    role: ServerRole::Game,
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

fn game_kind(warm: u32, max: u32, max_instances: u32) -> KindSpec {
    KindSpec {
        image: "hallow-test-game:dev".into(),
        role: KindRole::Game,
        lobby: Some("hub".into()),
        players: KindPlayers { min: 1, max },
        scaling: KindScaling {
            warm,
            max_instances: Some(max_instances),
        },
        env: [("GAME_MODE".to_owned(), "solo".to_owned())].into(),
        memory_mb: Some(512),
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
    let res = env
        .post(
            routes::SERVERS,
            &RegisterServer {
                name: "lobby".into(),
                kind: "hub".into(),
                version: "dev".into(),
                role: ServerRole::Lobby,
                address: "lobby:25565".into(),
                max_players: 100,
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
}

/// The Phase 1 gate's path: a player queues for a kind with no server, the
/// control plane starts an instance, its server registers, the player is
/// sent there, and once the game empties the instance is deleted.
#[tokio::test]
async fn a_queue_starts_a_fresh_instance_and_an_empty_game_deletes_it() {
    let Some(env) = Env::start().await else {
        return;
    };
    // The kinds registry checks what it's given.
    let mut bad = game_kind(0, 4, 2);
    bad.players.min = 9;
    bad.env.insert("HALLOW_SERVER_NAME".into(), "x".into());
    let res = env.put(&routes::kind("test-game"), &bad).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let err: ApiError = res.json().await.unwrap();
    assert_eq!(err.code, ErrorCode::BadRequest);
    assert!(err.fields.contains_key("players.min"));
    assert!(err.fields.contains_key("env.HALLOW_SERVER_NAME"));
    let res = env
        .put(&routes::kind("Bad_Name"), &game_kind(0, 4, 2))
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = env
        .put(&routes::kind("test-game"), &game_kind(0, 4, 2))
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let kinds: Vec<KindInfo> = env.get_json(routes::KINDS).await;
    assert_eq!(kinds.len(), 1);
    assert_eq!(kinds[0].name, "test-game");

    setup_network(&env).await;
    let player = env.online_player("proxy-1").await;
    // warm = 0: nothing runs until someone queues.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(env.instances().await.is_empty());

    // No test-game server exists, but the kind is startable.
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
    let started = env
        .wait_for("an instance to start", |list| {
            list.len() == 1 && list[0].state == InstanceState::Starting
        })
        .await;
    let instance = &started[0];
    assert!(instance.name.starts_with("test-game-"));
    assert_eq!(instance.name.len(), "test-game-".len() + 6);
    assert_eq!(instance.node, "dock");
    assert_eq!(instance.image, "hallow-test-game:dev");
    assert_eq!(
        instance.address.as_deref(),
        Some(&*format!("{}:25565", instance.name))
    );
    let ran = env.mock.instances();
    let spec = &ran[&instance.name].spec;
    assert_eq!(spec.env["HALLOW_SERVER_NAME"], instance.name);
    assert_eq!(spec.env["HALLOW_CONTROL_PLANE"], "control-plane:8080");
    // Its own scoped token, never the shared one.
    let instance_token = &spec.env["HALLOW_CONTROL_PLANE_TOKEN"];
    assert!(instance_token.starts_with("hit_"), "{instance_token}");
    assert_ne!(instance_token, TOKEN);
    assert_eq!(spec.env["HALLOW_VINE_SECRET"], "s3cret");
    assert_eq!(spec.env["GAME_MODE"], "solo");
    assert_eq!(spec.memory_mb, 512);
    // One is enough for one player, however many passes run.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(env.instances().await.len(), 1);

    // Its server boots and registers; the matchmaker sends the player.
    env.boot(&instance.name, "test-game", 4).await;
    env.wait_for("the instance to be ready", |list| {
        list.len() == 1 && list[0].state == InstanceState::Ready
    })
    .await;
    let mut sent_to = None;
    for _ in 0..50 {
        let poll: Poll<Vec<ProxyCommand>> = env
            .get_json(&routes::since_wait(
                &routes::proxy_commands("proxy-1"),
                0,
                0,
            ))
            .await;
        if let Poll::Changed { data, .. } = poll {
            sent_to = data.into_iter().find_map(|c| match c {
                ProxyCommand::Send { player: p, server } if p == player => Some(server),
                _ => None,
            });
        }
        if sent_to.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(sent_to.as_deref(), Some(instance.name.as_str()));

    // The player plays, then leaves: the game is over after the grace.
    env.heartbeat(&instance.name, &[player], ServerState::InGame)
        .await;
    let playing = env
        .wait_for("the game to count its player", |list| {
            list.first()
                .is_some_and(|i| i.had_players && i.players == 1)
        })
        .await;
    assert_eq!(playing[0].empty_since_ms, None);
    env.heartbeat(&instance.name, &[], ServerState::InGame)
        .await;
    env.wait_for("the instance to be deleted", <[InstanceInfo]>::is_empty)
        .await;
    let calls = env.mock.calls();
    assert_eq!(
        calls,
        [
            Call::Create {
                node: "dock".into(),
                name: instance.name.clone()
            },
            Call::Stop(instance.name.clone()),
            Call::Delete(instance.name.clone()),
        ]
    );
    assert!(env.mock.instances().is_empty());
    // Its server is off the list at once.
    let servers: Poll<Vec<ServerInfo>> = env.get_json(&routes::since(routes::SERVERS, 0)).await;
    let Poll::Changed { data, .. } = servers else {
        panic!("no server list");
    };
    assert!(data.iter().all(|s| s.name != instance.name));
    env.drop().await;
}

/// Warm instances while players are online, `max_instances`, a server
/// reporting its game over, staff ending one, and failed creates retried.
#[tokio::test]
async fn warm_pools_limits_endings_and_failures() {
    let Some(env) = Env::start().await else {
        return;
    };
    let res = env.put(&routes::kind("arena"), &game_kind(2, 4, 2)).await;
    assert_eq!(res.status(), StatusCode::OK);
    setup_network(&env).await;
    // Nobody online: no warm pool.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(env.instances().await.is_empty());

    let player = env.online_player("proxy-1").await;
    let warm = env
        .wait_for("two warm instances", |list| {
            list.len() == 2 && list.iter().all(|i| i.state == InstanceState::Starting)
        })
        .await;
    for instance in &warm {
        env.boot(&instance.name, "arena", 4).await;
    }
    env.wait_for("both ready", |list| {
        list.len() == 2 && list.iter().all(|i| i.state == InstanceState::Ready)
    })
    .await;

    // One reports its game over (draining, empty): it goes, and a new warm
    // instance replaces it.
    let first = warm[0].name.clone();
    env.heartbeat(&first, &[player], ServerState::InGame).await;
    env.heartbeat(&first, &[], ServerState::Draining).await;
    env.wait_for("a replacement", |list| {
        list.len() == 2 && list.iter().all(|i| i.name != first)
    })
    .await;
    assert!(env.mock.calls().contains(&Call::Delete(first.clone())));

    // Staff end one by hand.
    let second = warm[1].name.clone();
    let res = env.delete(&routes::instance(&second)).await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let res = env.delete(&routes::instance("arena-nope00")).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    env.wait_for("the staff-ended one to go", |list| {
        list.iter().all(|i| i.name != second)
    })
    .await;

    // Creates fail: the instance shows failed with the reason, then is
    // cleaned up and tried again.
    env.mock.fail_creates(true);
    let res = env.put(&routes::kind("broken"), &game_kind(1, 4, 1)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let failed = env
        .wait_for("a failed instance", |list| {
            list.iter()
                .any(|i| i.kind == "broken" && i.state == InstanceState::Failed)
        })
        .await;
    let failed = failed.iter().find(|i| i.kind == "broken").unwrap();
    assert_eq!(
        failed.reason.as_deref(),
        Some("refused: creates are failing")
    );
    env.mock.fail_creates(false);
    env.wait_for("a working retry", |list| {
        list.iter()
            .any(|i| i.kind == "broken" && i.state == InstanceState::Starting)
    })
    .await;
    // max_instances = 1 for broken, 2 for arena.
    tokio::time::sleep(Duration::from_millis(700)).await;
    let list = env.instances().await;
    assert_eq!(list.iter().filter(|i| i.kind == "broken").count(), 1);
    assert!(list.iter().filter(|i| i.kind == "arena").count() <= 2);

    // A removed kind's unused instances end once they're up.
    let res = env.delete(&routes::kind("broken")).await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let broken = list
        .iter()
        .find(|i| i.kind == "broken")
        .unwrap()
        .name
        .clone();
    env.boot(&broken, "broken", 4).await;
    env.wait_for("the removed kind's instance to go", |list| {
        list.iter().all(|i| i.kind != "broken")
    })
    .await;
    env.drop().await;
}

/// What an instance's own token can and can't do: only its own server's
/// routes, and nothing once the instance is deleted.
#[tokio::test]
async fn an_instance_token_reaches_only_its_own_server() {
    let Some(env) = Env::start().await else {
        return;
    };
    let mut lobby = game_kind(1, 16, 1);
    lobby.role = KindRole::Lobby;
    lobby.lobby = None;
    let res = env.put(&routes::kind("skylobby"), &lobby).await;
    assert_eq!(res.status(), StatusCode::OK);
    setup_network(&env).await;
    let started = env
        .wait_for("the lobby instance", |list| {
            list.len() == 1 && list[0].state == InstanceState::Starting
        })
        .await;
    let name = started[0].name.clone();
    let token = env.mock.instances()[&name].spec.env["HALLOW_CONTROL_PLANE_TOKEN"].clone();
    assert!(token.starts_with("hit_"));

    let url = |path: &str| format!("{}{path}", env.base);
    let as_instance = |req: reqwest::RequestBuilder| {
        let token = token.clone();
        async move { req.bearer_auth(token).send().await.unwrap().status() }
    };
    let register = |name: &str, kind: &str| RegisterServer {
        name: name.into(),
        kind: kind.into(),
        version: "dev".into(),
        role: ServerRole::Lobby,
        address: format!("{name}:25565"),
        max_players: 16,
    };
    let beat = ServerHeartbeat {
        players: 0,
        state: ServerState::Ready,
        tick_ms: None,
        roster: Some(Vec::new()),
    };

    // Everything the server plugin does, as itself.
    let ok = as_instance(
        env.http
            .post(url(routes::SERVERS))
            .json(&register(&name, "skylobby")),
    )
    .await;
    assert_eq!(ok, StatusCode::NO_CONTENT);
    let ok = as_instance(
        env.http
            .post(url(&routes::server_heartbeat(&name)))
            .json(&beat),
    )
    .await;
    assert_eq!(ok, StatusCode::NO_CONTENT);
    let ok = as_instance(env.http.get(url(&routes::server_players(&name)))).await;
    assert_eq!(ok, StatusCode::OK);
    let stats = hallow_proto::stats::StatsBatch {
        source: name.clone(),
        kind: "skylobby".into(),
        players: Vec::new(),
    };
    let ok = as_instance(env.http.post(url(routes::STATS)).json(&stats)).await;
    assert_ne!(ok, StatusCode::FORBIDDEN);
    assert_ne!(ok, StatusCode::UNAUTHORIZED);
    let signals = hallow_proto::signal::SignalBatch {
        source: name.clone(),
        samples: Vec::new(),
    };
    let ok = as_instance(env.http.post(url(routes::SIGNALS)).json(&signals)).await;
    assert_ne!(ok, StatusCode::FORBIDDEN);
    assert_ne!(ok, StatusCode::UNAUTHORIZED);
    env.wait_for("the lobby to be ready", |list| {
        list.len() == 1 && list[0].state == InstanceState::Ready
    })
    .await;

    // Another server: refused, by path or by body.
    let forbidden =
        [
            as_instance(
                env.http
                    .post(url(routes::SERVERS))
                    .json(&register("lobby", "hub")),
            )
            .await,
            as_instance(
                env.http
                    .post(url(routes::SERVERS))
                    .json(&register(&name, "hub")),
            )
            .await,
            as_instance(
                env.http
                    .post(url(&routes::server_heartbeat("lobby")))
                    .json(&beat),
            )
            .await,
            as_instance(env.http.delete(url(&routes::server("lobby")))).await,
            as_instance(env.http.get(url(&routes::server_players("lobby")))).await,
            as_instance(
                env.http
                    .post(url(routes::STATS))
                    .json(&hallow_proto::stats::StatsBatch {
                        source: "lobby".into(),
                        kind: "skylobby".into(),
                        players: Vec::new(),
                    }),
            )
            .await,
            as_instance(
                env.http
                    .post(url(routes::STATS))
                    .json(&hallow_proto::stats::StatsBatch {
                        source: name.clone(),
                        kind: "skywars".into(),
                        players: Vec::new(),
                    }),
            )
            .await,
            as_instance(env.http.post(url(routes::SIGNALS)).json(
                &hallow_proto::signal::SignalBatch {
                    source: "proxy-1".into(),
                    samples: Vec::new(),
                },
            ))
            .await,
            // Admin and proxy routes.
            as_instance(env.http.get(url(routes::INSTANCES))).await,
            as_instance(env.http.delete(url(&routes::instance(&name)))).await,
            as_instance(env.http.get(url(routes::KINDS))).await,
            as_instance(env.http.put(url(&routes::kind("evil"))).json(&lobby)).await,
            as_instance(env.http.get(url(&routes::since(routes::SERVERS, 0)))).await,
            as_instance(env.http.get(url(routes::PROXIES))).await,
            as_instance(env.http.get(url(&routes::proxy_commands("proxy-1")))).await,
            as_instance(
                env.http
                    .post(url(routes::COMMANDS))
                    .json(&serde_json::json!({})),
            )
            .await,
            as_instance(
                env.http
                    .post(url(routes::NODE_JOIN_TOKENS))
                    .json(&serde_json::json!({"role": "burst", "region": "x"})),
            )
            .await,
            as_instance(env.http.get(url(routes::RANKS))).await,
            as_instance(env.http.get(url(routes::FLEET_SETTINGS))).await,
        ];
    for (i, status) in forbidden.iter().enumerate() {
        assert_eq!(*status, StatusCode::FORBIDDEN, "call {i}");
    }
    // The other server is untouched.
    let servers: Poll<Vec<ServerInfo>> = env.get_json(&routes::since(routes::SERVERS, 0)).await;
    let Poll::Changed { data, .. } = servers else {
        panic!("no server list");
    };
    let other = data.iter().find(|s| s.name == "lobby").unwrap();
    assert_eq!(other.kind, "hub");
    assert_eq!(other.address, "lobby:25565");

    // A made-up token of the same shape is nobody.
    let fake = format!("hit_{}", "0".repeat(64));
    let status = env
        .http
        .post(url(&routes::server_heartbeat(&name)))
        .bearer_auth(fake)
        .json(&beat)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Deleted: the token is revoked with the record.
    let res = env.delete(&routes::instance(&name)).await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    env.wait_for("the lobby to go", |list| {
        list.iter().all(|i| i.name != name)
    })
    .await;
    let gone = as_instance(
        env.http
            .post(url(&routes::server_heartbeat(&name)))
            .json(&beat),
    )
    .await;
    assert_eq!(gone, StatusCode::UNAUTHORIZED);
    let gone = as_instance(
        env.http
            .post(url(routes::SERVERS))
            .json(&register(&name, "skylobby")),
    )
    .await;
    assert_eq!(gone, StatusCode::UNAUTHORIZED);
    env.drop().await;
}

/// A static scoped token from `HALLOW_SERVER_TOKENS` acts like an
/// instance's, for the server it names.
#[tokio::test]
async fn a_static_server_token_reaches_only_its_server() {
    let Some(env) = Env::start_with(|config| {
        config.server_tokens =
            hallow_control_plane::auth::parse_static_tokens("lobby:hub:static-lobby-token-0123")
                .unwrap();
    })
    .await
    else {
        return;
    };
    let url = |path: &str| format!("{}{path}", env.base);
    let register = |name: &str| RegisterServer {
        name: name.into(),
        kind: "hub".into(),
        version: "dev".into(),
        role: ServerRole::Lobby,
        address: format!("{name}:25565"),
        max_players: 100,
    };
    let send = |req: reqwest::RequestBuilder| async move {
        req.bearer_auth("static-lobby-token-0123")
            .send()
            .await
            .unwrap()
            .status()
    };
    assert_eq!(
        send(env.http.post(url(routes::SERVERS)).json(&register("lobby"))).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        send(
            env.http
                .post(url(routes::SERVERS))
                .json(&register("lobby-2"))
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(env.http.get(url(routes::INSTANCES))).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(env.http.delete(url(&routes::server("lobby")))).await,
        StatusCode::NO_CONTENT
    );
    env.drop().await;
}
