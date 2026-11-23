//! Server kind versions end to end (docs/SERVERS.md#versions): a kind gets
//! a new version as canary, a share of new instances start on it, promote
//! makes it live, and a rollback starts new instances on the previous
//! version while running games finish on theirs. The mock orchestrator
//! stands in for Docker or Wings and the test plays each instance's server.
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
        mock::MockOrchestrator,
    },
    serve_with,
};
use hallow_db::sqlx::{self, Executor, postgres::PgPoolOptions};
use hallow_proto::{
    ApiError,
    instance::{InstanceInfo, InstanceState, KindPlayers, KindRole, KindScaling, KindSpec},
    presence::{LoginDecision, LoginRequest},
    proxy::RegisterProxy,
    routes,
    server::{RegisterServer, ServerHeartbeat, ServerPlayer, ServerRole, ServerState},
    version::{
        Actor, AddVersion, KindVersions, Promote, Rollback, SetCanary, VersionAction,
        VersionAuditEntry,
    },
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
        let db_name = format!("hallow_ver_test_{}", Uuid::new_v4().simple());
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

    async fn post_json<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> T {
        let res = self.post(path, body).await;
        let status = res.status();
        let text = res.text().await.unwrap();
        assert!(status.is_success(), "POST {path}: {status} {text}");
        serde_json::from_str(&text).unwrap()
    }

    /// POSTs and expects a failure with `status`; returns its message.
    async fn refused<B: Serialize>(&self, path: &str, body: &B, status: StatusCode) -> String {
        let res = self.post(path, body).await;
        assert_eq!(res.status(), status, "POST {path}");
        let err: ApiError = res.json().await.unwrap();
        err.message
    }

    /// Plays every instance's server: one that is starting registers, like
    /// the server plugin does.
    async fn boot_starting(&self) {
        for instance in self.instances().await {
            if instance.state == InstanceState::Starting {
                let res = self
                    .post(
                        routes::SERVERS,
                        &RegisterServer {
                            name: instance.name.clone(),
                            kind: instance.kind.clone(),
                            version: instance.version.clone().unwrap_or_default(),
                            role: ServerRole::Game,
                            address: format!("{}:25565", instance.name),
                            max_players: 4,
                        },
                    )
                    .await;
                assert_eq!(res.status(), StatusCode::NO_CONTENT);
            }
        }
    }

    /// Boots instances as they start until `check` holds, or panics.
    async fn settle(
        &self,
        what: &str,
        check: impl Fn(&[InstanceInfo]) -> bool,
    ) -> Vec<InstanceInfo> {
        for _ in 0..150 {
            self.boot_starting().await;
            let instances = self.instances().await;
            if check(&instances) {
                return instances;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "timed out waiting for {what}: {:#?}",
            self.instances().await
        );
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

const KIND: &str = "test-game";

fn image(version: &str) -> String {
    format!("ghcr.io/xena-studios/test-game:{version}")
}

fn game_kind(warm: u32) -> KindSpec {
    KindSpec {
        image: image("1.0.0"),
        role: KindRole::Game,
        lobby: Some("hub".into()),
        players: KindPlayers { min: 1, max: 4 },
        scaling: KindScaling {
            warm,
            max_instances: Some(10),
        },
        env: std::collections::BTreeMap::new(),
        memory_mb: None,
        updated_by: Some("deploy".into()),
    }
}

fn version(version: &str, base: &str) -> AddVersion {
    AddVersion {
        version: version.into(),
        image: image(version),
        base: base.into(),
        base_version: None,
        commit: Some("3f2a9c1".into()),
        actor: Actor::new("ci"),
    }
}

fn staff(name: &str) -> Actor {
    Actor {
        by: name.into(),
        panel_user: Some("42".into()),
    }
}

/// Live instances (creating, starting, ready) by version.
fn by_version(list: &[InstanceInfo]) -> std::collections::BTreeMap<String, usize> {
    let mut counts = std::collections::BTreeMap::new();
    for i in list.iter().filter(|i| i.state.is_live()) {
        *counts
            .entry(i.version.clone().unwrap_or_default())
            .or_default() += 1;
    }
    counts
}

fn only(list: &[InstanceInfo], version: &str, count: usize) -> bool {
    by_version(list) == [(version.to_owned(), count)].into()
        && list
            .iter()
            .filter(|i| i.state.is_live())
            .all(|i| i.state == InstanceState::Ready && i.image == image(version))
}

/// The issue #20 path: canary, promote, roll back.
#[tokio::test]
async fn canary_promote_and_rollback() {
    let Some(env) = Env::start().await else {
        return;
    };
    let versions = routes::kind_versions(KIND);
    let canary = routes::kind_canary(KIND);
    let promote = routes::kind_promote(KIND);
    let rollback = routes::kind_rollback(KIND);

    // Versions belong to a registered kind.
    env.refused(&versions, &version("1.0.0", "^0.2"), StatusCode::NOT_FOUND)
        .await;
    let res = env.put(&routes::kind(KIND), &game_kind(2)).await;
    assert_eq!(res.status(), StatusCode::OK);

    // CI records versions; the same one again is a no-op, another image
    // under the same name is refused, and fields are checked.
    let res = env.post(&versions, &version("1.0.0", "^0.2")).await;
    assert_eq!(res.status(), StatusCode::CREATED);
    let res = env.post(&versions, &version("1.0.0", "^0.2")).await;
    assert_eq!(res.status(), StatusCode::OK);
    let mut other = version("1.0.0", "^0.2");
    other.image = image("other");
    env.refused(&versions, &other, StatusCode::CONFLICT).await;
    let mut bad = version("not a version", "nope");
    bad.actor.by = String::new();
    let res = env.post(&versions, &bad).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let err: ApiError = res.json().await.unwrap();
    assert_eq!(
        err.fields.keys().map(String::as_str).collect::<Vec<_>>(),
        ["base", "by", "image", "version"]
    );
    let mut built = version("1.1.0", "^0.2");
    built.base_version = Some("0.3.0".into());
    env.refused(&versions, &built, StatusCode::BAD_REQUEST)
        .await;
    built.base_version = Some("0.2.0".into());
    let _: KindVersions = env.post_json(&versions, &built).await;
    let _: KindVersions = env.post_json(&versions, &version("0.9.0", "^0.1")).await;
    let state: KindVersions = env
        .post_json(&versions, &version("sha-3f2a9c1", "^0.2"))
        .await;

    // PUT /v1/kinds set the image before 1.0.0 was recorded, so nothing is
    // live by name yet; the network runs the shipped base image.
    assert_eq!(state.live, None);
    assert_eq!(state.live_image, image("1.0.0"));
    assert_eq!(
        state.base_version,
        hallow_control_plane::versions::SHIPPED_BASE_VERSION.trim()
    );
    let names: Vec<(&str, bool, bool)> = state
        .versions
        .iter()
        .map(|v| (v.version.as_str(), v.compatible, v.dev))
        .collect();
    assert_eq!(
        names,
        [
            ("sha-3f2a9c1", true, true),
            ("0.9.0", false, false),
            ("1.1.0", true, false),
            ("1.0.0", true, false),
        ]
    );

    // Refusals: outside the base range, dev builds, unknown versions,
    // nothing to roll back to, nothing to promote.
    let message = env
        .refused(
            &promote,
            &Promote {
                version: Some("0.9.0".into()),
                actor: staff("alice"),
            },
            StatusCode::CONFLICT,
        )
        .await;
    assert!(message.contains("needs base ^0.1"), "{message}");
    let message = env
        .refused(
            &canary,
            &SetCanary {
                version: "sha-3f2a9c1".into(),
                percent: 10,
                actor: staff("alice"),
            },
            StatusCode::CONFLICT,
        )
        .await;
    assert!(message.contains("dev build"), "{message}");
    env.refused(
        &canary,
        &SetCanary {
            version: "9.9.9".into(),
            percent: 10,
            actor: staff("alice"),
        },
        StatusCode::NOT_FOUND,
    )
    .await;
    env.refused(
        &canary,
        &SetCanary {
            version: "1.1.0".into(),
            percent: 0,
            actor: staff("alice"),
        },
        StatusCode::BAD_REQUEST,
    )
    .await;
    env.refused(
        &rollback,
        &Rollback {
            version: None,
            actor: staff("alice"),
        },
        StatusCode::CONFLICT,
    )
    .await;
    env.refused(
        &promote,
        &Promote {
            version: None,
            actor: staff("alice"),
        },
        StatusCode::CONFLICT,
    )
    .await;

    // 1.0.0 goes live by name.
    let state: KindVersions = env
        .post_json(
            &promote,
            &Promote {
                version: Some("1.0.0".into()),
                actor: staff("alice"),
            },
        )
        .await;
    assert_eq!(state.live.as_deref(), Some("1.0.0"));
    assert_eq!(state.previous, None);

    // Two warm instances while a player is online, both on 1.0.0.
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
    let player = env.online_player("proxy-1").await;
    let list = env
        .settle("two 1.0.0 instances", |l| only(l, "1.0.0", 2))
        .await;
    let ran = env.mock.instances();
    for i in &list {
        assert_eq!(ran[&i.name].spec.image, image("1.0.0"));
    }

    // A scoped instance token can't touch versions.
    let token = ran[&list[0].name].spec.env["HALLOW_CONTROL_PLANE_TOKEN"].clone();
    let res = env
        .http
        .post(format!("{}{promote}", env.base))
        .bearer_auth(&token)
        .json(&Promote {
            version: Some("1.1.0".into()),
            actor: staff("mallory"),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);

    // 1.1.0 as a 50 % canary. Running instances keep 1.0.0; replacing them
    // puts one of the two new ones on the canary.
    let state: KindVersions = env
        .post_json(
            &canary,
            &SetCanary {
                version: "1.1.0".into(),
                percent: 50,
                actor: staff("alice"),
            },
        )
        .await;
    let set = state.canary.as_ref().unwrap();
    assert_eq!((set.version.as_str(), set.percent), ("1.1.0", 50));
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        by_version(&env.instances().await),
        [("1.0.0".into(), 2)].into()
    );
    for i in &list {
        let res = env.delete(&routes::instance(&i.name)).await;
        assert_eq!(res.status(), StatusCode::ACCEPTED);
    }
    let list = env
        .settle("one live and one canary instance", |l| {
            by_version(l) == [("1.0.0".into(), 1), ("1.1.0".into(), 1)].into()
                && l.iter()
                    .filter(|i| i.state.is_live())
                    .all(|i| i.state == InstanceState::Ready)
        })
        .await;
    let ran = env.mock.instances();
    for i in list.iter().filter(|i| i.state.is_live()) {
        assert_eq!(
            ran[&i.name].spec.image,
            image(i.version.as_deref().unwrap())
        );
    }
    let state: KindVersions = env.get_json(&versions).await;
    let counts: Vec<(&str, u32)> = state
        .versions
        .iter()
        .map(|v| (v.version.as_str(), v.instances))
        .collect();
    assert!(
        counts.contains(&("1.0.0", 1)) && counts.contains(&("1.1.0", 1)),
        "{counts:?}"
    );

    // Promote the canary: 1.1.0 is live, 1.0.0 is what a rollback goes back
    // to, and the 1.0.0 instance stops counting as warm.
    let state: KindVersions = env
        .post_json(
            &promote,
            &Promote {
                version: None,
                actor: staff("bob"),
            },
        )
        .await;
    assert_eq!(state.live.as_deref(), Some("1.1.0"));
    assert_eq!(state.live_image, image("1.1.0"));
    assert_eq!(state.previous.as_deref(), Some("1.0.0"));
    assert_eq!(state.canary, None);
    let list = env
        .settle("only 1.1.0 instances", |l| only(l, "1.1.0", 2))
        .await;

    // A game is running on 1.1.0 when it turns out bad.
    let playing = list
        .iter()
        .find(|i| i.state == InstanceState::Ready)
        .unwrap()
        .name
        .clone();
    env.heartbeat(&playing, &[player], ServerState::InGame)
        .await;
    env.settle("the game to count its player", |l| {
        l.iter().any(|i| i.name == playing && i.had_players)
    })
    .await;

    // Roll back: new instances start on 1.0.0; the running game finishes on
    // 1.1.0, and the unused 1.1.0 instance goes.
    let state: KindVersions = env
        .post_json(
            &rollback,
            &Rollback {
                version: None,
                actor: staff("carol"),
            },
        )
        .await;
    assert_eq!(state.live.as_deref(), Some("1.0.0"));
    assert_eq!(state.live_image, image("1.0.0"));
    assert_eq!(state.previous, None);
    let list = env
        .settle("two warm 1.0.0 instances beside the running game", |l| {
            by_version(l) == [("1.0.0".into(), 2), ("1.1.0".into(), 1)].into()
                && l.iter()
                    .filter(|i| i.state.is_live())
                    .all(|i| i.state == InstanceState::Ready)
        })
        .await;
    assert!(
        list.iter()
            .any(|i| i.name == playing && i.version.as_deref() == Some("1.1.0"))
    );
    let kinds: hallow_proto::instance::KindInfo = env.get_json(&routes::kind(KIND)).await;
    assert_eq!(kinds.spec.image, image("1.0.0"));
    // Once its game ends, the 1.1.0 instance goes too.
    env.heartbeat(&playing, &[], ServerState::Draining).await;
    env.settle("only 1.0.0 instances", |l| only(l, "1.0.0", 2))
        .await;

    // Every action is in the audit log, newest first, with who did it.
    let log: Vec<VersionAuditEntry> = env
        .get_json(&format!("{}?kind={KIND}", routes::VERSION_AUDIT))
        .await;
    let actions: Vec<(VersionAction, Option<&str>, Option<&str>, &str)> = log
        .iter()
        .map(|e| {
            (
                e.action,
                e.version.as_deref(),
                e.from_version.as_deref(),
                e.by.as_str(),
            )
        })
        .collect();
    assert_eq!(
        actions,
        [
            (
                VersionAction::Rollback,
                Some("1.0.0"),
                Some("1.1.0"),
                "carol"
            ),
            (VersionAction::Promote, Some("1.1.0"), Some("1.0.0"), "bob"),
            (VersionAction::Canary, Some("1.1.0"), None, "alice"),
            (VersionAction::Promote, Some("1.0.0"), None, "alice"),
            (VersionAction::Add, Some("sha-3f2a9c1"), None, "ci"),
            (VersionAction::Add, Some("0.9.0"), None, "ci"),
            (VersionAction::Add, Some("1.1.0"), None, "ci"),
            (VersionAction::Add, Some("1.0.0"), None, "ci"),
            (VersionAction::Register, None, None, "deploy"),
        ]
    );
    assert_eq!(log[2].percent, Some(50));
    assert_eq!(log[0].panel_user.as_deref(), Some("42"));
    let limited: Vec<VersionAuditEntry> = env
        .get_json(&format!("{}?limit=2", routes::VERSION_AUDIT))
        .await;
    assert_eq!(limited.len(), 2);
    env.drop().await;
}

/// `PUT /v1/kinds` (the kinds file, a deploy script) stays a direct deploy:
/// a new image makes its version live if it is one, and the old live
/// version becomes the rollback target. Stopping a canary.
#[tokio::test]
async fn put_keeps_channels_in_step() {
    let Some(env) = Env::start_with(|config| {
        config.versions.dev_versions = true;
    })
    .await
    else {
        return;
    };
    let res = env.put(&routes::kind(KIND), &game_kind(0)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let versions = routes::kind_versions(KIND);
    for v in ["1.0.0", "1.1.0", "sha-3f2a9c1"] {
        let _: KindVersions = env.post_json(&versions, &version(v, "^0.2")).await;
    }
    let _: KindVersions = env
        .post_json(
            &routes::kind_promote(KIND),
            &Promote {
                version: Some("1.0.0".into()),
                actor: staff("alice"),
            },
        )
        .await;
    // Dev builds may go canary on a network that allows them.
    let state: KindVersions = env
        .post_json(
            &routes::kind_canary(KIND),
            &SetCanary {
                version: "sha-3f2a9c1".into(),
                percent: 100,
                actor: staff("alice"),
            },
        )
        .await;
    assert!(state.dev_versions);
    let state: KindVersions = env
        .post_json(&routes::kind_canary_stop(KIND), &staff("alice"))
        .await;
    assert_eq!(state.canary, None);
    env.refused(
        &routes::kind_canary_stop(KIND),
        &staff("alice"),
        StatusCode::CONFLICT,
    )
    .await;

    // Re-registering the same image changes nothing.
    let res = env.put(&routes::kind(KIND), &game_kind(0)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let state: KindVersions = env.get_json(&versions).await;
    assert_eq!(state.live.as_deref(), Some("1.0.0"));
    // A new image that is a version makes it live.
    let mut spec = game_kind(0);
    spec.image = image("1.1.0");
    let res = env.put(&routes::kind(KIND), &spec).await;
    assert_eq!(res.status(), StatusCode::OK);
    let state: KindVersions = env.get_json(&versions).await;
    assert_eq!(state.live.as_deref(), Some("1.1.0"));
    assert_eq!(state.previous.as_deref(), Some("1.0.0"));
    // One that isn't leaves nothing live by name; rollback still works.
    spec.image = "hallow-test-game:dev".into();
    let res = env.put(&routes::kind(KIND), &spec).await;
    assert_eq!(res.status(), StatusCode::OK);
    let state: KindVersions = env.get_json(&versions).await;
    assert_eq!(state.live, None);
    assert_eq!(state.previous.as_deref(), Some("1.1.0"));
    let state: KindVersions = env
        .post_json(
            &routes::kind_rollback(KIND),
            &Rollback {
                version: Some("1.0.0".into()),
                actor: staff("alice"),
            },
        )
        .await;
    assert_eq!(state.live.as_deref(), Some("1.0.0"));
    assert_eq!(state.live_image, image("1.0.0"));

    // Deleting the kind removes its versions; the audit log stays.
    let res = env.delete(&routes::kind(KIND)).await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let res = env
        .send(env.http.get(format!("{}{versions}", env.base)))
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let log: Vec<VersionAuditEntry> = env.get_json(routes::VERSION_AUDIT).await;
    assert_eq!(log[0].action, VersionAction::Rollback);
    assert_eq!(log[1].action, VersionAction::Register);
    assert_eq!(log[1].image.as_deref(), Some("hallow-test-game:dev"));
    env.drop().await;
}
