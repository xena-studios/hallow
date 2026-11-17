//! The control plane side of the panel's Reports, Games and Stats pages
//! (#32): player reports and their staff workflow, enabling and disabling
//! kinds, and the game and queue metrics.
//!
//! Needs `HALLOW_TEST_REDIS_URL`, and `HALLOW_TEST_DATABASE_URL` for all but
//! the metrics without a database; without them the tests pass without doing
//! anything (except in CI, where they fail). Each test gets its own key
//! prefix and database.

use std::time::Duration;

use hallow_control_plane::{Config, serve};
use hallow_db::sqlx::{self, Executor, postgres::PgPoolOptions};
use hallow_proto::{
    ApiError, ErrorCode, Poll,
    command::ProxyCommand,
    instance::{KindInfo, KindPlayers, KindRole, KindScaling, KindSpec},
    lobby::{LobbyOverview, SendToLobby},
    presence::{LoginDecision, LoginRequest, PlayerSwitched},
    proxy::{ProxyRegistered, RegisterProxy},
    punishment::{IssuePunishment, IssuerKind, PunishmentKind, PunishmentReply, StaffMember},
    queue::{JoinQueue, QueueStatus},
    report::{
        ClaimReport, CloseReport, CreateReport, Report, ReportCreated, ReportPage, ReportStatus,
    },
    routes,
    server::{RegisterServer, ServerHeartbeat, ServerRole, ServerState},
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
    admin: Option<(hallow_db::PgPool, String)>,
    pool: Option<hallow_db::PgPool>,
}

impl Env {
    /// A control plane with a database of its own, or `None` to skip.
    async fn start() -> Option<Self> {
        let redis = env_or_skip("HALLOW_TEST_REDIS_URL")?;
        let db_url = env_or_skip("HALLOW_TEST_DATABASE_URL")?;
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(&db_url)
            .await
            .unwrap();
        let db_name = format!("hallow_r32_test_{}", Uuid::new_v4().simple());
        admin
            .execute(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db_name}")))
            .await
            .unwrap();
        let (base_url, _) = db_url.rsplit_once('/').unwrap();
        let url = format!("{base_url}/{db_name}");
        let mut config = Config::new(redis, TOKEN);
        config.key_prefix = format!("test-{}", Uuid::new_v4());
        config.database_url = Some(url.clone());
        config.long_poll = Duration::from_millis(200);
        let mut env = Self::serve(config).await;
        env.pool = Some(hallow_db::connect(&url).await.unwrap());
        env.admin = Some((admin, db_name));
        Some(env)
    }

    /// A control plane without a database, or `None` to skip.
    async fn start_without_db() -> Option<Self> {
        let redis = env_or_skip("HALLOW_TEST_REDIS_URL")?;
        let mut config = Config::new(redis, TOKEN);
        config.key_prefix = format!("test-{}", Uuid::new_v4());
        config.long_poll = Duration::from_millis(200);
        Some(Self::serve(config).await)
    }

    async fn serve(config: Config) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { serve(listener, config).await.unwrap() });
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
        Self {
            base,
            http,
            admin: None,
            pool: None,
        }
    }

    async fn drop(self) {
        if let Some(pool) = self.pool {
            pool.close().await;
        }
        if let Some((admin, name)) = self.admin {
            admin
                .execute(sqlx::AssertSqlSafe(format!(
                    "DROP DATABASE {name} WITH (FORCE)"
                )))
                .await
                .unwrap();
        }
    }

    fn pool(&self) -> &hallow_db::PgPool {
        self.pool.as_ref().unwrap()
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

    async fn get(&self, path: &str) -> reqwest::Response {
        self.send(self.http.get(format!("{}{path}", self.base)))
            .await
    }

    async fn ok<T: DeserializeOwned>(res: reqwest::Response, what: &str) -> T {
        let status = res.status();
        let body = res.text().await.unwrap();
        assert!(status.is_success(), "{what}: {status} {body}");
        serde_json::from_str(&body).unwrap()
    }

    async fn post_json<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> T {
        Self::ok(self.post(path, body).await, path).await
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> T {
        Self::ok(self.get(path).await, path).await
    }

    /// The status and error of a refused call.
    async fn refused(res: reqwest::Response) -> (StatusCode, ApiError) {
        let status = res.status();
        (status, res.json().await.unwrap())
    }

    async fn metrics(&self) -> String {
        self.http
            .get(format!("{}/metrics", self.base))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
    }

    async fn register_proxy(&self, id: &str) -> ProxyRegistered {
        self.post_json(
            routes::PROXIES,
            &RegisterProxy {
                id: id.into(),
                address: format!("{id}:25565"),
                version: "test".into(),
            },
        )
        .await
    }

    async fn register_server(&self, name: &str, kind: &str, role: ServerRole, max: u32) {
        let res = self
            .post(
                routes::SERVERS,
                &RegisterServer {
                    name: name.into(),
                    kind: kind.into(),
                    version: "dev".into(),
                    role,
                    address: format!("{name}:25565"),
                    max_players: max,
                },
            )
            .await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let beat = ServerHeartbeat {
            players: 0,
            state: ServerState::Ready,
            tick_ms: None,
            roster: None,
        };
        let res = self.post(&routes::server_heartbeat(name), &beat).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    async fn login(&self, name: &str, proxy: &str) -> Uuid {
        let uuid = Uuid::new_v4();
        let decision: LoginDecision = self
            .post_json(
                routes::PRESENCE_LOGIN,
                &LoginRequest {
                    uuid,
                    username: name.into(),
                    proxy: proxy.into(),
                    protocol: Some(777),
                    ip: None,
                },
            )
            .await;
        assert!(
            matches!(decision, LoginDecision::Allow { .. }),
            "{decision:?}"
        );
        uuid
    }

    /// Chat messages queued for `proxy` after `since`.
    async fn messages(&self, proxy: &str, since: u64) -> Vec<(Uuid, String)> {
        let poll: Poll<Vec<ProxyCommand>> = self
            .get_json(&routes::since_wait(
                &routes::proxy_commands(proxy),
                since,
                0,
            ))
            .await;
        match poll {
            Poll::Changed { data, .. } => data
                .into_iter()
                .filter_map(|c| match c {
                    ProxyCommand::Message { player, text, .. } => Some((player, text)),
                    _ => None,
                })
                .collect(),
            Poll::Unchanged { .. } => Vec::new(),
        }
    }

    async fn report(&self, reporter: Uuid, target: &str, reason: &str) -> reqwest::Response {
        self.post(
            routes::REPORTS,
            &CreateReport {
                reporter,
                target: target.into(),
                reason: reason.into(),
            },
        )
        .await
    }
}

fn panel_staff() -> StaffMember {
    StaffMember {
        name: "Admin".into(),
        panel_user: Some("12".into()),
    }
}

/// Reports from `/report` to resolution: staff on another proxy hear of
/// them, the limits hold, the panel lists, claims, resolves and dismisses.
#[tokio::test]
async fn reports_from_report_to_resolution() {
    let Some(env) = Env::start().await else {
        return;
    };
    env.register_proxy("proxy-a").await;
    let b = env.register_proxy("proxy-b").await;
    env.register_server("hub-1", "hub", ServerRole::Lobby, 100)
        .await;
    let alice = env.login("alice", "proxy-a").await;
    let _bob = env.login("bob", "proxy-a").await;
    let carol = env.login("carol", "proxy-a").await;
    let moderator = env.login("Moderator", "proxy-b").await;
    let others: Vec<String> = ["dave", "erin", "frank", "gina"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    for name in &others {
        env.login(name, "proxy-a").await;
    }
    let rank = hallow_db::Rank {
        name: "helper".into(),
        display_prefix: "&a[Helper] ".into(),
        weight: 10,
        permissions: vec!["hallow.staff.reports".into()],
        is_default: false,
    };
    hallow_db::upsert_rank(env.pool(), &rank).await.unwrap();
    hallow_db::grant_rank(env.pool(), moderator, "helper", None, None)
        .await
        .unwrap();
    let res = env
        .post(
            routes::PRESENCE_SWITCH,
            &PlayerSwitched {
                uuid: alice,
                server: "hub-1".into(),
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    // A report, and staff hear of it on their own proxy.
    let created: ReportCreated = Env::ok(
        env.report(alice, "BOB", "  flying §cover the map ").await,
        "report",
    )
    .await;
    assert!(created.message.contains("bob"), "{}", created.message);
    let heard = env.messages("proxy-b", b.commands_revision).await;
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert_eq!(heard[0].0, moderator);
    assert!(
        heard[0].1.contains(&format!("[Report #{}]", created.id))
            && heard[0].1.contains("alice")
            && heard[0].1.contains("bob")
            && heard[0].1.contains("hub-1")
            && heard[0].1.contains("flying cover the map"),
        "{}",
        heard[0].1
    );
    // Nobody without the node hears it.
    assert!(
        env.messages("proxy-a", 0)
            .await
            .iter()
            .all(|(_, t)| !t.contains("[Report")),
    );

    // Refusals.
    let (status, err) = Env::refused(env.report(alice, "bob", "again").await).await;
    assert_eq!(status, StatusCode::CONFLICT, "{err:?}");
    let (status, _) = Env::refused(env.report(alice, "Alice", "me").await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, err) = Env::refused(env.report(alice, "nobody", "who").await).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(err.message.contains("nobody"), "{}", err.message);
    let (status, _) = Env::refused(env.report(alice, "carol", "  ").await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = Env::refused(env.report(alice, "carol", &"x".repeat(257)).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // At most 5 in 10 minutes.
    for name in &others {
        let res = env.report(alice, name, "griefing").await;
        assert_eq!(res.status(), StatusCode::OK, "{name}");
    }
    let (status, err) = Env::refused(env.report(alice, "carol", "rude").await).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(err.code, ErrorCode::RateLimited);
    // Someone else may report bob too.
    let second: ReportCreated = Env::ok(env.report(carol, "bob", "spam").await, "carol").await;

    // The panel's list.
    let page: ReportPage = env.get_json(routes::REPORTS).await;
    assert_eq!(page.total, 6);
    assert_eq!(page.reports[0].id, second.id, "newest first");
    let page: ReportPage = env.get_json("/v1/reports?target=bob").await;
    assert_eq!(page.total, 2);
    assert!(page.reports.iter().all(|r| r.target_reports == 2));
    let page: ReportPage = env.get_json("/v1/reports?status=active&limit=2").await;
    assert_eq!((page.total, page.reports.len()), (6, 2));
    let page: ReportPage = env.get_json("/v1/reports?limit=2&offset=5").await;
    assert_eq!((page.total, page.reports.len()), (6, 1));
    let res = env.get("/v1/reports?target=nobody").await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let res = env.get("/v1/reports?status=bogus").await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let report: Report = env.get_json(&routes::report(created.id)).await;
    assert_eq!(report.reporter.name, "alice");
    assert_eq!(report.target.name, "bob");
    assert_eq!(report.reason, "flying cover the map");
    assert_eq!(report.server.as_deref(), Some("hub-1"));
    assert_eq!(report.status, ReportStatus::Open);
    assert_eq!(report.claimed_by, None);
    let raw: serde_json::Value = env.get_json(&routes::report(created.id)).await;
    assert_eq!(raw["closed_by"], serde_json::Value::Null);
    assert!(env.metrics().await.contains("hallow_reports_open 6"));

    // Claim from the panel, hand over to in-game staff, give back.
    let claimed: Report = env
        .post_json(
            &routes::report_claim(created.id),
            &ClaimReport {
                issuer: None,
                staff: Some(panel_staff()),
            },
        )
        .await;
    assert_eq!(claimed.status, ReportStatus::Claimed);
    let by = claimed.claimed_by.unwrap();
    assert_eq!(
        (by.name.as_str(), by.kind, by.panel_user.as_deref()),
        ("Admin", IssuerKind::Staff, Some("12"))
    );
    assert!(claimed.claimed_ms.is_some());
    let claimed: Report = env
        .post_json(
            &routes::report_claim(created.id),
            &ClaimReport {
                issuer: Some(moderator),
                staff: None,
            },
        )
        .await;
    let by = claimed.claimed_by.unwrap();
    assert_eq!(
        (by.name.as_str(), by.kind, by.uuid),
        ("Moderator", IssuerKind::Player, Some(moderator))
    );
    let open: Report = env
        .post_json(&routes::report_unclaim(created.id), &serde_json::json!({}))
        .await;
    assert_eq!((open.status, open.claimed_by), (ReportStatus::Open, None));

    // Resolving names a punishment of the target, and only of the target.
    let ban: PunishmentReply = env
        .post_json(
            routes::PUNISHMENTS,
            &IssuePunishment {
                kind: PunishmentKind::Ban,
                target: "bob".into(),
                issuer: None,
                reason: "flying".into(),
                duration_seconds: Some(86_400),
                staff: Some(panel_staff()),
            },
        )
        .await;
    let warn: PunishmentReply = env
        .post_json(
            routes::PUNISHMENTS,
            &IssuePunishment {
                kind: PunishmentKind::Warn,
                target: "carol".into(),
                issuer: None,
                reason: "spam".into(),
                duration_seconds: None,
                staff: Some(panel_staff()),
            },
        )
        .await;
    for wrong in [warn.id.unwrap(), 999_999] {
        let res = env
            .post(
                &routes::report_resolve(created.id),
                &CloseReport {
                    staff: Some(panel_staff()),
                    punishment_id: Some(wrong),
                    ..CloseReport::default()
                },
            )
            .await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{wrong}");
    }
    let res = env
        .post(
            &routes::report_dismiss(created.id),
            &CloseReport {
                punishment_id: ban.id,
                ..CloseReport::default()
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST, "a dismissal");
    let resolved: Report = env
        .post_json(
            &routes::report_resolve(created.id),
            &CloseReport {
                issuer: None,
                staff: Some(panel_staff()),
                note: Some("banned for a day".into()),
                punishment_id: ban.id,
            },
        )
        .await;
    assert_eq!(resolved.status, ReportStatus::Resolved);
    assert_eq!(resolved.punishment_id, ban.id);
    assert_eq!(resolved.note.as_deref(), Some("banned for a day"));
    assert_eq!(resolved.closed_by.unwrap().name, "Admin");
    assert!(resolved.closed_ms.is_some());

    // Closed is closed.
    for path in [
        routes::report_claim(created.id),
        routes::report_unclaim(created.id),
        routes::report_resolve(created.id),
        routes::report_dismiss(created.id),
    ] {
        let res = env.post(&path, &CloseReport::default()).await;
        assert_eq!(res.status(), StatusCode::CONFLICT, "{path}");
    }
    // Unknown ids.
    assert_eq!(
        env.get(&routes::report(999_999)).await.status(),
        StatusCode::NOT_FOUND
    );
    let res = env
        .post(&routes::report_claim(999_999), &ClaimReport::default())
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        env.get("/v1/reports/nope").await.status(),
        StatusCode::BAD_REQUEST
    );

    // Dismissing, by the console.
    let dismissed: Report = env
        .post_json(
            &routes::report_dismiss(second.id),
            &CloseReport {
                note: Some("not spam".into()),
                ..CloseReport::default()
            },
        )
        .await;
    assert_eq!(dismissed.status, ReportStatus::Dismissed);
    assert_eq!(dismissed.closed_by.unwrap().kind, IssuerKind::Console);

    let page: ReportPage = env.get_json("/v1/reports?status=resolved").await;
    assert_eq!(page.total, 1);
    let page: ReportPage = env.get_json("/v1/reports?status=dismissed").await;
    assert_eq!(page.total, 1);
    let page: ReportPage = env.get_json("/v1/reports?status=open").await;
    assert_eq!(page.total, 4);
    let page: ReportPage = env.get_json("/v1/reports?status=claimed").await;
    assert_eq!(page.total, 0);
    assert!(env.metrics().await.contains("hallow_reports_open 4"));
    env.drop().await;
}

/// Without a database, reports answer 503 and the gauge is left out.
#[tokio::test]
async fn reports_need_the_database() {
    let Some(env) = Env::start_without_db().await else {
        return;
    };
    let (status, err) = Env::refused(env.report(Uuid::new_v4(), "bob", "x").await).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(err.code, ErrorCode::Unavailable);
    assert_eq!(
        env.get(routes::REPORTS).await.status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(!env.metrics().await.contains("hallow_reports_open"));
    env.drop().await;
}

fn skywars(enabled: bool) -> KindSpec {
    KindSpec {
        image: "hallow-skywars:dev".into(),
        role: KindRole::Game,
        lobby: Some("skywars-lobby".into()),
        players: KindPlayers { min: 1, max: 8 },
        scaling: KindScaling {
            warm: 1,
            max_instances: Some(4),
        },
        env: [("MODE".to_owned(), "solo".to_owned())].into(),
        memory_mb: Some(2048),
        updated_by: Some("panel:Admin".into()),
        enabled,
    }
}

fn metric(metrics: &str, line: &str) -> Option<f64> {
    metrics
        .lines()
        .find_map(|l| l.strip_prefix(line)?.trim().parse().ok())
}

/// A disabled game takes no queue entries and isn't on the hub's menu;
/// enabled again, games start and the matchmaker's metrics count them.
#[tokio::test]
async fn disabled_games_and_game_metrics() {
    let Some(env) = Env::start().await else {
        return;
    };
    let a = env.register_proxy("proxy-a").await;
    env.register_server("hub-1", "hub", ServerRole::Lobby, 100)
        .await;
    env.register_server("sw-lobby-1", "skywars-lobby", ServerRole::Lobby, 50)
        .await;
    env.register_server("skywars-1", "skywars", ServerRole::Game, 8)
        .await;
    let alice = env.login("alice", "proxy-a").await;
    let bob = env.login("bob", "proxy-a").await;

    // GET → PUT round-trips every field.
    let info: KindInfo = Env::ok(
        env.put(&routes::kind("skywars"), &skywars(false)).await,
        "put",
    )
    .await;
    assert_eq!(info.spec, skywars(false));
    let raw: serde_json::Value = env.get_json(&routes::kind("skywars")).await;
    assert_eq!(raw["enabled"], false);
    let again: KindInfo = Env::ok(env.put(&routes::kind("skywars"), &raw).await, "put back").await;
    assert_eq!(again.spec, skywars(false));
    // Old clients that leave it out get an enabled kind.
    let mut old = serde_json::to_value(skywars(true)).unwrap();
    old.as_object_mut().unwrap().remove("enabled");
    old.as_object_mut().unwrap().remove("lobby");
    let info: KindInfo = Env::ok(env.put(&routes::kind("old-game"), &old).await, "old").await;
    assert!(info.spec.enabled);
    // A hub can't be turned off.
    let mut hub = skywars(false);
    hub.role = KindRole::Hub;
    hub.lobby = None;
    let (status, err) = Env::refused(env.put(&routes::kind("hub"), &hub).await).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(err.fields.contains_key("enabled"), "{err:?}");

    // Disabled: no queue, no menu entry, no lobby.
    let (status, err) = Env::refused(
        env.post(
            routes::QUEUES,
            &JoinQueue {
                kind: "skywars".into(),
                player: alice,
            },
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(err.message, "skywars is disabled right now.");
    let overview: LobbyOverview = env.get_json(routes::LOBBIES).await;
    assert!(
        overview.modes.iter().all(|m| m.name != "skywars"),
        "{overview:?}"
    );
    let (status, err) = Env::refused(
        env.post(
            routes::LOBBIES_SEND,
            &SendToLobby {
                player: alice,
                lobby: Some("skywars".into()),
                fallback_hub: false,
            },
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(err.message.contains("disabled"), "{}", err.message);

    // Enabled again: on the menu, queues place, and the metrics count.
    env.put(&routes::kind("skywars"), &skywars(true)).await;
    let overview: LobbyOverview = env.get_json(routes::LOBBIES).await;
    assert!(overview.modes.iter().any(|m| m.name == "skywars"));
    let metrics = env.metrics().await;
    assert_eq!(
        metric(&metrics, "hallow_games_started_total{kind=\"skywars\"}"),
        None
    );
    let mut sent = Vec::new();
    for player in [alice, bob] {
        let _: QueueStatus = env
            .post_json(
                routes::QUEUES,
                &JoinQueue {
                    kind: "skywars".into(),
                    player,
                },
            )
            .await;
        // Placed within a pass or two.
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let poll: Poll<Vec<ProxyCommand>> = env
                .get_json(&routes::since_wait(
                    &routes::proxy_commands("proxy-a"),
                    a.commands_revision,
                    0,
                ))
                .await;
            sent = match poll {
                Poll::Changed { data, .. } => data
                    .into_iter()
                    .filter_map(|c| match c {
                        ProxyCommand::Send { player, server } => Some((player, server)),
                        _ => None,
                    })
                    .collect(),
                Poll::Unchanged { .. } => Vec::new(),
            };
            if sent.iter().any(|(p, _)| *p == player) {
                break;
            }
        }
    }
    assert_eq!(
        sent,
        [
            (alice, "skywars-1".to_owned()),
            (bob, "skywars-1".to_owned())
        ]
    );
    let metrics = env.metrics().await;
    // One game: the second player joined it.
    assert_eq!(
        metric(&metrics, "hallow_games_started_total{kind=\"skywars\"}"),
        Some(1.0),
        "{metrics}"
    );
    assert_eq!(
        metric(
            &metrics,
            "hallow_queue_wait_seconds_count{kind=\"skywars\"}"
        ),
        Some(2.0),
        "{metrics}"
    );
    assert_eq!(
        metric(
            &metrics,
            "hallow_queue_wait_seconds_bucket{kind=\"skywars\",le=\"+Inf\"}"
        ),
        Some(2.0)
    );
    assert_eq!(
        metric(
            &metrics,
            "hallow_queue_wait_seconds_bucket{kind=\"skywars\",le=\"5\"}"
        ),
        Some(2.0)
    );
    assert!(metric(&metrics, "hallow_queue_wait_seconds_sum{kind=\"skywars\"}").is_some());
    assert!(metrics.contains("# TYPE hallow_queue_wait_seconds histogram"));
    env.drop().await;
}
