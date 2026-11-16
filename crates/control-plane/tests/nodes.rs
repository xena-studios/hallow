//! Node join tokens end to end: a control plane on the test Redis and a
//! fresh PostgreSQL database, talking to a mock Calagopus panel (axum, in
//! process) that checks the API key and records every call. The mock speaks
//! the routes and shapes of the panel's admin API (`panel/calagopus`,
//! `backend/src/routes/api/admin/nodes`).
//!
//! Needs `HALLOW_TEST_REDIS_URL` and `HALLOW_TEST_DATABASE_URL`, like
//! `tests/network.rs`; skips without them (except in CI).

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, post},
};
use hallow_control_plane::{Config, panel::PanelConfig, serve};
use hallow_db::sqlx::{self, Executor, postgres::PgPoolOptions};
use hallow_proto::{
    ApiError, ErrorCode,
    node::{IssueJoinToken, JoinReply, JoinRequest, JoinToken, NodeInfo, NodeRole},
    routes,
};
use serde_json::{Value, json};
use uuid::Uuid;

const TOKEN: &str = "test-token-0123456789";
const PANEL_KEY: &str = "c7sp_0123456789012345678901234567890123456789abc";
const WG_KEY: &str = "xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=";

// The mock panel

#[derive(Default)]
struct Mock {
    /// Method, path and body of every call that passed the key check.
    calls: Vec<(String, String, Value)>,
    nodes: BTreeMap<Uuid, Value>,
    /// Answer the next enrollment with a 500.
    fail_enrollment: bool,
}

type Shared = Arc<Mutex<Mock>>;

fn errors(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "errors": [message] }))).into_response()
}

async fn check_key(State(mock): State<Shared>, request: Request, next: Next) -> Response {
    let auth = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    if auth != format!("Bearer {PANEL_KEY}") {
        return errors(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let (method, path) = (
        request.method().to_string(),
        request.uri().path().to_owned(),
    );
    let (parts, body) = request.into_parts();
    let bytes = axum::body::to_bytes(body, 1 << 20).await.unwrap();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    mock.lock().unwrap().calls.push((method, path, value));
    next.run(Request::from_parts(parts, axum::body::Body::from(bytes)))
        .await
}

async fn create_node(State(mock): State<Shared>, Json(body): Json<Value>) -> Response {
    let mut m = mock.lock().unwrap();
    if m.nodes.values().any(|n| n["name"] == body["name"]) {
        return errors(StatusCode::CONFLICT, "node with name already exists");
    }
    let uuid = Uuid::new_v4();
    let mut node = body.clone();
    node["uuid"] = json!(uuid);
    m.nodes.insert(uuid, node.clone());
    Json(json!({ "node": node })).into_response()
}

async fn allocations(
    State(mock): State<Shared>,
    Path(node): Path<Uuid>,
    Json(body): Json<Value>,
) -> Response {
    if !mock.lock().unwrap().nodes.contains_key(&node) {
        return errors(StatusCode::NOT_FOUND, "node not found");
    }
    let created = body["ports"].as_array().map_or(0, Vec::len);
    Json(json!({ "created": created })).into_response()
}

async fn enrollment(State(mock): State<Shared>, Path(node): Path<Uuid>) -> Response {
    let mut m = mock.lock().unwrap();
    if std::mem::take(&mut m.fail_enrollment) {
        return errors(StatusCode::INTERNAL_SERVER_ERROR, "cache unavailable");
    }
    if !m.nodes.contains_key(&node) {
        return errors(StatusCode::NOT_FOUND, "node not found");
    }
    Json(json!({ "code": format!("enroll-{node}"), "expires": "2026-11-15T20:30:00Z" }))
        .into_response()
}

async fn delete_node(State(mock): State<Shared>, Path(node): Path<Uuid>) -> Response {
    match mock.lock().unwrap().nodes.remove(&node) {
        Some(_) => Json(json!({})).into_response(),
        None => errors(StatusCode::NOT_FOUND, "node not found"),
    }
}

async fn start_panel() -> (String, Shared) {
    let mock = Shared::default();
    let app = Router::new()
        .route("/api/admin/nodes", post(create_node))
        .route("/api/admin/nodes/{node}", delete(delete_node))
        .route("/api/admin/nodes/{node}/allocations", post(allocations))
        .route("/api/admin/nodes/{node}/enrollment", post(enrollment))
        .layer(middleware::from_fn_with_state(mock.clone(), check_key))
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, mock)
}

// The control plane

struct Env {
    base: String,
    http: reqwest::Client,
    mock: Shared,
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
        let db_name = format!("hallow_cp_test_{}", Uuid::new_v4().simple());
        admin
            .execute(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db_name}")))
            .await
            .unwrap();
        let (base_url, _) = db_url.rsplit_once('/').unwrap();

        let (panel_url, mock) = start_panel().await;
        let mut panel = PanelConfig::new(panel_url, PANEL_KEY);
        panel.public_url = "https://panel.example.net".into();
        panel.locations.insert(
            "fsn1".into(),
            "11111111-1111-1111-1111-111111111111".parse().unwrap(),
        );
        panel.game_ports = (30000, 30009);
        let mut config = Config::new(redis, TOKEN);
        config.key_prefix = format!("test-{}", Uuid::new_v4());
        config.database_url = Some(format!("{base_url}/{db_name}"));
        config.panel = Some(panel);

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
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        Some(Self {
            base,
            http,
            mock,
            admin,
            db_name,
        })
    }

    async fn issue(&self, req: &IssueJoinToken) -> reqwest::Response {
        self.http
            .post(format!("{}{}", self.base, routes::NODE_JOIN_TOKENS))
            .bearer_auth(TOKEN)
            .json(req)
            .send()
            .await
            .unwrap()
    }

    async fn join(&self, token: &str, req: &JoinRequest) -> reqwest::Response {
        self.http
            .post(format!("{}{}", self.base, routes::NODE_JOIN))
            .bearer_auth(token)
            .json(req)
            .send()
            .await
            .unwrap()
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
}

fn env_or_skip(var: &str) -> Option<String> {
    let value = std::env::var(var).ok();
    if value.is_none() {
        assert!(std::env::var_os("CI").is_none(), "{var} isn't set in CI");
        eprintln!("{var} isn't set; skipping");
    }
    value
}

fn burst(name: Option<&str>) -> IssueJoinToken {
    IssueJoinToken {
        role: NodeRole::Burst,
        region: "fsn1".into(),
        name: name.map(str::to_owned),
        labels: [("pool".to_owned(), "games".to_owned())].into(),
        ttl_seconds: None,
    }
}

fn machine(ip: &str) -> JoinRequest {
    JoinRequest {
        hostname: "hallow-burst-1.fsn1".into(),
        private_ip: ip.into(),
        public_ip: Some("203.0.113.9".into()),
        wireguard_public_key: Some(WG_KEY.into()),
        arch: "x86_64".into(),
        memory_mb: 15_990,
        disk_mb: 301_000,
    }
}

async fn error_code(res: reqwest::Response) -> ErrorCode {
    res.json::<ApiError>().await.unwrap().code
}

/// Issue, join once, refuse the second join; the panel got the node, its
/// allocations on the private IP and an enrollment for the public URL.
#[tokio::test]
async fn a_token_joins_one_node() {
    let Some(env) = Env::start().await else {
        return;
    };
    let res = env.issue(&burst(None)).await;
    assert_eq!(res.status(), StatusCode::OK);
    let token: JoinToken = res.json().await.unwrap();
    assert!(token.token.starts_with("hjt_"));
    assert_eq!(token.token.len(), 68);
    assert_eq!(token.role, NodeRole::Burst);
    let now_ms = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap();
    let day_ms = 24 * 3600 * 1000;
    assert!(
        token.expires_at_ms > now_ms + day_ms - 60_000 && token.expires_at_ms <= now_ms + day_ms
    );

    let res = env.join(&token.token, &machine("10.42.1.7")).await;
    assert_eq!(res.status(), StatusCode::OK);
    let reply: JoinReply = res.json().await.unwrap();
    assert_eq!(reply.name, "hallow-burst-1", "named after the hostname");
    assert_eq!(reply.panel_url, "https://panel.example.net");
    assert_eq!(reply.enroll_code, format!("enroll-{}", reply.node));
    assert_eq!(reply.role, NodeRole::Burst);

    let calls = env.mock.lock().unwrap().calls.clone();
    let paths: Vec<(&str, String)> = calls
        .iter()
        .map(|(m, p, _)| (m.as_str(), p.replace(&reply.node.to_string(), "{node}")))
        .collect();
    assert_eq!(
        paths,
        [
            ("POST", "/api/admin/nodes".to_owned()),
            ("POST", "/api/admin/nodes/{node}/allocations".to_owned()),
            ("POST", "/api/admin/nodes/{node}/enrollment".to_owned()),
        ]
    );
    let created = &calls[0].2;
    assert_eq!(created["name"], "hallow-burst-1");
    assert_eq!(
        created["location_uuid"],
        "11111111-1111-1111-1111-111111111111"
    );
    assert_eq!(created["url"], "http://10.42.1.7:8080");
    assert_eq!(created["sftp_host"], "10.42.1.7");
    assert_eq!(created["sftp_port"], 2022);
    assert_eq!(created["memory"], 15_990);
    assert_eq!(created["deployment_enabled"], true);
    assert!(
        created["description"]
            .as_str()
            .unwrap()
            .contains("burst node in fsn1")
    );
    assert_eq!(calls[1].2["ip"], "10.42.1.7");
    assert_eq!(
        calls[1].2["ports"],
        json!([
            30000, 30001, 30002, 30003, 30004, 30005, 30006, 30007, 30008, 30009
        ])
    );
    assert_eq!(calls[2].2["remote"], "https://panel.example.net");

    // Spent.
    let res = env.join(&token.token, &machine("10.42.1.8")).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(res).await, ErrorCode::Unauthorized);

    let res = env
        .http
        .get(format!("{}{}", env.base, routes::NODES))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    let nodes: Vec<NodeInfo> = res.json().await.unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].panel_uuid, reply.node);
    assert_eq!(nodes[0].private_ip, "10.42.1.7");
    assert_eq!(nodes[0].wireguard_public_key.as_deref(), Some(WG_KEY));
    assert_eq!(nodes[0].labels["pool"], "games");
    env.drop().await;
}

/// A panel failure half way deletes the panel node and gives the token back,
/// so the machine's retry works.
#[tokio::test]
async fn a_failed_join_can_be_retried() {
    let Some(env) = Env::start().await else {
        return;
    };
    let token: JoinToken = env
        .issue(&burst(Some("burst-7")))
        .await
        .json()
        .await
        .unwrap();
    env.mock.lock().unwrap().fail_enrollment = true;
    let res = env.join(&token.token, &machine("10.42.1.9")).await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    {
        let m = env.mock.lock().unwrap();
        assert!(m.nodes.is_empty(), "the half-made node was deleted");
        assert_eq!(m.calls.last().unwrap().0, "DELETE");
    }
    let res = env.join(&token.token, &machine("10.42.1.9")).await;
    assert_eq!(res.status(), StatusCode::OK);
    let reply: JoinReply = res.json().await.unwrap();
    assert_eq!(reply.name, "burst-7", "named by the token");
    assert_eq!(env.mock.lock().unwrap().nodes.len(), 1);
    env.drop().await;
}

/// Two machines racing one token: exactly one joins.
#[tokio::test]
async fn racing_joins_get_one_node() {
    let Some(env) = Env::start().await else {
        return;
    };
    let token: JoinToken = env
        .issue(&burst(Some("burst-race")))
        .await
        .json()
        .await
        .unwrap();
    let (one, two) = (machine("10.42.2.1"), machine("10.42.2.2"));
    let (a, b) = tokio::join!(env.join(&token.token, &one), env.join(&token.token, &two));
    let mut statuses = [a.status(), b.status()];
    statuses.sort();
    assert_eq!(statuses, [StatusCode::OK, StatusCode::UNAUTHORIZED]);
    assert_eq!(env.mock.lock().unwrap().nodes.len(), 1);
    env.drop().await;
}

/// Bad tokens, bad requests and the shared token on the wrong route.
#[tokio::test]
async fn joins_and_issues_are_checked() {
    let Some(env) = Env::start().await else {
        return;
    };
    // The join route doesn't take the shared token, and the issue route
    // doesn't take a join token.
    let res = env.join(TOKEN, &machine("10.42.1.7")).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let fake = format!("hjt_{}", "0".repeat(64));
    let res = env.join(&fake, &machine("10.42.1.7")).await;
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let token: JoinToken = env.issue(&burst(None)).await.json().await.unwrap();
    let res = env
        .http
        .post(format!("{}{}", env.base, routes::NODE_JOIN_TOKENS))
        .bearer_auth(&token.token)
        .json(&burst(None))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    // A bad request doesn't spend the token.
    let mut bad = machine("10.42.1.7");
    bad.private_ip = "nope".into();
    assert_eq!(
        env.join(&token.token, &bad).await.status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        env.join(&token.token, &machine("10.42.1.7")).await.status(),
        StatusCode::OK
    );

    // Issue checks.
    let mut unknown_region = burst(None);
    unknown_region.region = "ash".into();
    assert_eq!(
        env.issue(&unknown_region).await.status(),
        StatusCode::BAD_REQUEST
    );
    let mut long = burst(None);
    long.ttl_seconds = Some(8 * 24 * 3600);
    assert_eq!(env.issue(&long).await.status(), StatusCode::BAD_REQUEST);
    let mut label = burst(None);
    label.labels.insert("bad key".into(), "x".into());
    assert_eq!(env.issue(&label).await.status(), StatusCode::BAD_REQUEST);
    // The name is taken by the node that just joined.
    let res = env.issue(&burst(Some("hallow-burst-1"))).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    env.drop().await;
}
