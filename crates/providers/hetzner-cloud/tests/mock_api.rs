//! The provider against a mock Hetzner Cloud API (axum, in process) that
//! checks the token and records every request. Never calls the real API.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use hallow_fleet::{
    Cost, Money, NodeId, NodeKind, NodeSpec, Provider, ProviderError, ProviderState,
};
use hallow_provider_hetzner_cloud::{ApiToken, Config, HetznerCloud, PriceBasis};
use serde_json::{Value, json};

const TOKEN: &str = "test-token";
/// The mock pages two servers at a time, whatever the client asks for.
const PAGE: usize = 2;

#[derive(Debug, Clone, PartialEq)]
struct Recorded {
    method: String,
    path: String,
    query: Option<String>,
    body: Option<Value>,
}

#[derive(Default)]
struct Mock {
    servers: BTreeMap<u64, Value>,
    next_id: u64,
    requests: Vec<Recorded>,
    rate_limited: bool,
    delay: Option<Duration>,
}

type Shared = Arc<Mutex<Mock>>;

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "error": { "code": code, "message": message } })),
    )
        .into_response()
}

fn prices(server_type: &str) -> Option<Value> {
    let (hourly_net, hourly_gross, monthly_net, monthly_gross) = match server_type {
        "cpx41" => (
            "0.0303000000",
            "0.0360570000",
            "18.9000000000",
            "22.4910000000",
        ),
        "cx22" => (
            "0.0060000000",
            "0.0071400000",
            "3.7900000000",
            "4.5101000000",
        ),
        _ => return None,
    };
    let at = |location: &str| {
        json!({
            "location": location,
            "price_hourly": { "net": hourly_net, "gross": hourly_gross },
            "price_monthly": { "net": monthly_net, "gross": monthly_gross },
            "included_traffic": 21990232555520u64,
            "price_per_tb_traffic": { "net": "1.0000000000", "gross": "1.1900000000" }
        })
    };
    Some(json!([at("fsn1"), at("nbg1"), at("hel1")]))
}

fn server_json(id: u64, name: &str, server_type: &str, location: &str, labels: &Value) -> Value {
    json!({
        "id": id,
        "name": name,
        "status": "initializing",
        "created": "2026-11-15T18:00:00+00:00",
        "server_type": { "id": 1, "name": server_type, "cores": 8, "memory": 16.0, "prices": prices(server_type) },
        "datacenter": { "id": 1, "name": format!("{location}-dc14"), "location": { "id": 1, "name": location } },
        "public_net": { "ipv4": { "ip": "203.0.113.10" } },
        "labels": labels,
    })
}

async fn guard(State(mock): State<Shared>, request: Request, next: Next) -> Response {
    let (rate_limited, delay) = {
        let mut m = mock.lock().unwrap();
        m.requests.push(Recorded {
            method: request.method().to_string(),
            path: request.uri().path().to_owned(),
            query: request.uri().query().map(str::to_owned),
            body: None,
        });
        (m.rate_limited, m.delay)
    };
    if let Some(delay) = delay {
        tokio::time::sleep(delay).await;
    }
    let auth = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());
    if auth != Some(&format!("Bearer {TOKEN}")) {
        return error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "unable to authenticate",
        );
    }
    if rate_limited {
        let mut response = error(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_exceeded",
            "limit of 3600 exceeded",
        );
        let reset = chrono::Utc::now().timestamp() + 30;
        response.headers_mut().insert(
            "ratelimit-reset",
            HeaderValue::from_str(&reset.to_string()).unwrap(),
        );
        response
            .headers_mut()
            .insert("ratelimit-limit", HeaderValue::from_static("3600"));
        response
            .headers_mut()
            .insert("ratelimit-remaining", HeaderValue::from_static("0"));
        return response;
    }
    next.run(request).await
}

async fn create(State(mock): State<Shared>, Json(body): Json<Value>) -> Response {
    let mut m = mock.lock().unwrap();
    m.requests.last_mut().unwrap().body = Some(body.clone());
    let name = body["name"].as_str().unwrap_or_default().to_owned();
    let server_type = body["server_type"].as_str().unwrap_or_default();
    let location = body["location"].as_str().unwrap_or_default();
    if prices(server_type).is_none() {
        return error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_input",
            "invalid input in field 'server_type'",
        );
    }
    if m.servers.values().any(|s| s["name"] == name.as_str()) {
        return error(
            StatusCode::CONFLICT,
            "uniqueness_error",
            "server name is already used",
        );
    }
    m.next_id += 1;
    let id = 1000 + m.next_id;
    let server = server_json(id, &name, server_type, location, &body["labels"]);
    m.servers.insert(id, server.clone());
    (
        StatusCode::CREATED,
        Json(json!({
            "server": server,
            "action": { "id": 1, "command": "create_server", "status": "running" },
            "next_actions": [],
            "root_password": null
        })),
    )
        .into_response()
}

async fn show(State(mock): State<Shared>, Path(id): Path<u64>) -> Response {
    match mock.lock().unwrap().servers.get(&id) {
        Some(server) => Json(json!({ "server": server })).into_response(),
        None => error(StatusCode::NOT_FOUND, "not_found", "server not found"),
    }
}

async fn delete(State(mock): State<Shared>, Path(id): Path<u64>) -> Response {
    match mock.lock().unwrap().servers.remove(&id) {
        Some(_) => {
            Json(json!({ "action": { "id": 2, "command": "delete_server", "status": "running" } }))
                .into_response()
        }
        None => error(StatusCode::NOT_FOUND, "not_found", "server not found"),
    }
}

async fn list(State(mock): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let m = mock.lock().unwrap();
    let selected: Vec<&Value> = m
        .servers
        .values()
        .filter(
            |s| match q.get("label_selector").and_then(|sel| sel.split_once('=')) {
                Some((k, v)) => s["labels"][k] == v,
                None => true,
            },
        )
        .collect();
    let page: usize = q.get("page").and_then(|p| p.parse().ok()).unwrap_or(1);
    let last_page = selected.len().div_ceil(PAGE).max(1);
    let servers: Vec<&Value> = selected
        .iter()
        .skip((page - 1) * PAGE)
        .take(PAGE)
        .copied()
        .collect();
    Json(json!({
        "servers": servers,
        "meta": { "pagination": {
            "page": page, "per_page": PAGE, "last_page": last_page, "total_entries": selected.len(),
            "previous_page": if page > 1 { json!(page - 1) } else { Value::Null },
            "next_page": if page < last_page { json!(page + 1) } else { Value::Null },
        }}
    }))
    .into_response()
}

async fn pricing() -> Response {
    let types: Vec<Value> = ["cx22", "cpx41"]
        .iter()
        .map(|t| json!({ "id": 1, "name": t, "prices": prices(t) }))
        .collect();
    Json(json!({
        "pricing": {
            "currency": "EUR",
            "vat_rate": "19.00",
            "primary_ips": [],
            "server_types": types,
        }
    }))
    .into_response()
}

struct Harness {
    mock: Shared,
    base_url: String,
}

impl Harness {
    async fn start() -> Harness {
        let mock: Shared = Arc::default();
        let app = Router::new()
            .route("/v1/servers", get(list).post(create))
            .route("/v1/servers/{id}", get(show).delete(delete))
            .route("/v1/pricing", get(pricing))
            .layer(middleware::from_fn_with_state(mock.clone(), guard))
            .with_state(mock.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Harness {
            mock,
            base_url: format!("http://{addr}/v1"),
        }
    }

    fn provider(&self) -> HetznerCloud {
        self.provider_with(TOKEN, Config::default())
    }

    fn provider_with(&self, token: &str, config: Config) -> HetznerCloud {
        HetznerCloud::new(
            ApiToken::new(token),
            Config {
                base_url: self.base_url.clone(),
                ..config
            },
        )
        .unwrap()
    }

    fn requests(&self) -> Vec<Recorded> {
        self.mock.lock().unwrap().requests.clone()
    }

    fn deletes(&self) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.method == "DELETE")
            .count()
    }

    /// A server someone else made in the same Hetzner project.
    fn seed_foreign(&self) -> NodeId {
        let mut m = self.mock.lock().unwrap();
        let server = server_json(7, "website", "cx22", "nbg1", &json!({ "team": "web" }));
        m.servers.insert(7, server);
        NodeId::from("7")
    }
}

fn spec(name: &str) -> NodeSpec {
    NodeSpec {
        name: name.into(),
        region: "fsn1".into(),
        machine_type: "cpx41".into(),
        image: "debian-13".into(),
        user_data: "#cloud-config\npackages: [docker.io]\n".into(),
        labels: BTreeMap::from([("role".into(), "game".into())]),
    }
}

fn cpx41_net() -> Cost {
    Cost::hourly(
        Money::parse("0.0303").unwrap(),
        Some(Money::parse("18.90").unwrap()),
    )
}

#[tokio::test]
async fn create_sends_the_spec_with_hallows_labels() {
    let h = Harness::start().await;
    let node = h.provider().create(&spec("hallow-burst-1")).await.unwrap();

    assert_eq!(node.id, NodeId::from("1001"));
    assert_eq!(node.provider, "hetzner-cloud");
    assert_eq!(node.name, "hallow-burst-1");
    assert_eq!(node.region, "fsn1");
    assert_eq!(node.machine_type, "cpx41");
    assert_eq!(node.kind, NodeKind::Burst);
    assert_eq!(node.state, ProviderState::Provisioning);
    assert_eq!(node.cost, cpx41_net());
    assert_eq!(node.created_at.to_rfc3339(), "2026-11-15T18:00:00+00:00");

    let requests = h.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        (requests[0].method.as_str(), requests[0].path.as_str()),
        ("POST", "/v1/servers")
    );
    assert_eq!(
        requests[0].body,
        Some(json!({
            "name": "hallow-burst-1",
            "server_type": "cpx41",
            "image": "debian-13",
            "location": "fsn1",
            "user_data": "#cloud-config\npackages: [docker.io]\n",
            "labels": { "hallow": "node", "hallow-kind": "burst", "role": "game" },
            "start_after_create": true
        }))
    );
}

/// The node bootstrap's cloud-init goes to Hetzner as the server's user data,
/// unchanged and under Hetzner's 32 KiB limit.
#[tokio::test]
async fn create_sends_the_rendered_node_bootstrap() {
    let h = Harness::start().await;
    let user_data = hallow_fleet::render_cloud_init(&hallow_fleet::NodeBootstrap {
        control_plane_url: "https://cp.example.net".into(),
        join_token: "hjt_0123456789abcdef".into(),
        role: NodeKind::Burst,
        region: "fsn1".into(),
        labels: BTreeMap::new(),
        wireguard: Some(hallow_fleet::WireGuard {
            address: "10.42.1.7/16".into(),
            listen_port: None,
            peers: vec![hallow_fleet::WireGuardPeer {
                public_key: "xTIBA5rboUvnH4htodjb6e697QjLERt1NAB4mZqp8Dg=".into(),
                endpoint: Some("hub.example.net:51820".into()),
                allowed_ips: vec!["10.42.0.0/16".into()],
                persistent_keepalive: Some(25),
            }],
        }),
        private_interfaces: vec![],
        ssh_allow: vec![],
        wings_allow: vec![],
        game_ports: None,
        wings: None,
        ssh_authorized_keys: vec![],
    })
    .unwrap();
    assert!(user_data.len() <= hallow_fleet::bootstrap::USER_DATA_LIMIT);
    let mut spec = spec("hallow-burst-1");
    spec.user_data = user_data.clone();
    h.provider().create(&spec).await.unwrap();
    let requests = h.requests();
    assert_eq!(
        requests[0].body.as_ref().unwrap()["user_data"].as_str(),
        Some(user_data.as_str())
    );
}

#[tokio::test]
async fn list_sees_only_hallows_servers_across_pages() {
    let h = Harness::start().await;
    h.seed_foreign();
    let provider = h.provider();
    for i in 1..=3 {
        provider
            .create(&spec(&format!("hallow-burst-{i}")))
            .await
            .unwrap();
    }

    let nodes = provider.list().await.unwrap();
    let names: Vec<_> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(
        names,
        ["hallow-burst-1", "hallow-burst-2", "hallow-burst-3"]
    );

    let queries: Vec<_> = h
        .requests()
        .into_iter()
        .filter(|r| r.method == "GET" && r.path == "/v1/servers")
        .map(|r| r.query.unwrap())
        .collect();
    assert_eq!(
        queries,
        [
            "label_selector=hallow%3Dnode&page=1&per_page=50",
            "label_selector=hallow%3Dnode&page=2&per_page=50"
        ]
    );
}

#[tokio::test]
async fn status_then_cancel() {
    let h = Harness::start().await;
    let provider = h.provider();
    let node = provider.create(&spec("hallow-burst-1")).await.unwrap();

    assert_eq!(provider.status(&node.id).await.unwrap(), node);
    provider.cancel(&node.id).await.unwrap();
    assert_eq!(h.deletes(), 1);
    assert!(matches!(
        provider.status(&node.id).await,
        Err(ProviderError::NotFound(_))
    ));

    // Cancelling again succeeds without another DELETE.
    provider.cancel(&node.id).await.unwrap();
    assert_eq!(h.deletes(), 1);

    let paths: Vec<_> = h
        .requests()
        .into_iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    assert_eq!(
        paths,
        [
            "POST /v1/servers",
            "GET /v1/servers/1001",
            "GET /v1/servers/1001",
            "DELETE /v1/servers/1001",
            "GET /v1/servers/1001",
            "GET /v1/servers/1001",
        ]
    );
}

#[tokio::test]
async fn never_touches_servers_without_hallows_label() {
    let h = Harness::start().await;
    let foreign = h.seed_foreign();
    let provider = h.provider();

    assert_eq!(
        provider.status(&foreign).await,
        Err(ProviderError::NotManaged(foreign.clone()))
    );
    assert_eq!(
        provider.cancel(&foreign).await,
        Err(ProviderError::NotManaged(foreign.clone()))
    );
    assert_eq!(h.deletes(), 0);
    assert!(h.mock.lock().unwrap().servers.contains_key(&7));
}

#[tokio::test]
async fn prices_per_type_and_location() {
    let h = Harness::start().await;
    let prices = h.provider().prices().await.unwrap();
    assert_eq!(prices.currency, "EUR");
    assert_eq!(prices.entries.len(), 6);
    assert_eq!(prices.find("cpx41", "fsn1").unwrap().cost, cpx41_net());
    assert_eq!(
        prices.find("cx22", "hel1").unwrap().cost,
        Cost::hourly(
            Money::from_micros(6_000),
            Some(Money::from_micros(3_790_000))
        )
    );

    let gross = h
        .provider_with(
            TOKEN,
            Config {
                price_basis: PriceBasis::Gross,
                ..Config::default()
            },
        )
        .prices()
        .await
        .unwrap();
    assert_eq!(
        gross.find("cpx41", "fsn1").unwrap().cost,
        Cost::hourly(
            Money::from_micros(36_057),
            Some(Money::from_micros(22_491_000))
        )
    );
}

#[tokio::test]
async fn maps_errors() {
    let h = Harness::start().await;
    let provider = h.provider();

    let wrong_token = h.provider_with("wrong", Config::default());
    assert_eq!(wrong_token.list().await, Err(ProviderError::Unauthorized));

    let mut bad_type = spec("hallow-burst-1");
    bad_type.machine_type = "cx999".into();
    assert!(matches!(
        provider.create(&bad_type).await,
        Err(ProviderError::InvalidRequest(_))
    ));

    provider.create(&spec("hallow-burst-1")).await.unwrap();
    assert!(matches!(
        provider.create(&spec("hallow-burst-1")).await,
        Err(ProviderError::Conflict(_))
    ));

    let before = h.requests().len();
    assert!(matches!(
        provider.status(&NodeId::from("not-a-number")).await,
        Err(ProviderError::InvalidRequest(_))
    ));
    assert_eq!(h.requests().len(), before, "a bad id never reaches the API");

    h.mock.lock().unwrap().rate_limited = true;
    let err = provider.list().await.unwrap_err();
    let ProviderError::RateLimited {
        retry_after: Some(wait),
    } = err
    else {
        panic!("{err:?}");
    };
    assert!(
        wait <= Duration::from_secs(30) && wait >= Duration::from_secs(28),
        "{wait:?}"
    );
    assert!(err.is_retryable());
}

#[tokio::test]
async fn times_out() {
    let h = Harness::start().await;
    h.mock.lock().unwrap().delay = Some(Duration::from_secs(5));
    let provider = h.provider_with(
        TOKEN,
        Config {
            timeout: Duration::from_millis(200),
            ..Config::default()
        },
    );
    assert_eq!(provider.list().await, Err(ProviderError::Timeout));
}
