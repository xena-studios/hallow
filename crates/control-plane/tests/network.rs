//! End-to-end tests against a real Redis (or Valkey): two control plane
//! instances share it, and simulated proxies talk to different instances.
//!
//! Set `HALLOW_TEST_REDIS_URL` (such as `redis://127.0.0.1:6379`) to run
//! them; without it they pass without doing anything (except in CI, where
//! they fail). Each test uses its own
//! key prefix, so they can share one Redis.

use std::time::Duration;

use hallow_control_plane::{Config, DuplicateLogin, serve, serve_all};
use hallow_proto::{
    ApiError, ErrorCode, Poll,
    command::{NetworkCommand, ProxyCommand},
    presence::{
        LoginDecision, LoginRequest, NetworkCount, PlayerLeft, PlayerLocation, PlayerSwitched,
    },
    proxy::{ProxyHeartbeat, ProxyInfo, ProxyRegistered, RegisterProxy, SetDraining},
    routes,
    server::{RegisterServer, ServerHeartbeat, ServerInfo, ServerRole, ServerState},
};
use reqwest::StatusCode;
use serde::{Serialize, de::DeserializeOwned};
use uuid::Uuid;

const TOKEN: &str = "test-token-0123456789";

fn redis_url() -> Option<String> {
    let url = std::env::var("HALLOW_TEST_REDIS_URL").ok();
    if url.is_none() {
        // CI provides Valkey; a skip there would hide a broken setup.
        assert!(
            std::env::var_os("CI").is_none(),
            "HALLOW_TEST_REDIS_URL isn't set in CI"
        );
        eprintln!("HALLOW_TEST_REDIS_URL isn't set; skipping");
    }
    url
}

fn config(redis: &str, prefix: &str) -> Config {
    let mut config = Config::new(redis, TOKEN);
    config.key_prefix = prefix.to_owned();
    config
}

/// One control plane instance on a free local port.
struct Instance {
    base: String,
    http: reqwest::Client,
}

impl Instance {
    async fn start(config: Config) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { serve(listener, config).await.unwrap() });
        Self::connect(base).await
    }

    /// Waits for an instance at `base` to answer its health check.
    async fn connect(base: String) -> Self {
        let instance = Self {
            base,
            http: reqwest::Client::new(),
        };
        for _ in 0..50 {
            if let Ok(res) = instance
                .http
                .get(format!("{}/healthz", instance.base))
                .send()
                .await
                && res.status() == StatusCode::NO_CONTENT
            {
                return instance;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("control plane didn't come up");
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> reqwest::Response {
        req.bearer_auth(TOKEN).send().await.unwrap()
    }

    async fn post<B: Serialize>(&self, path: &str, body: &B) -> reqwest::Response {
        self.send(self.http.post(format!("{}{path}", self.base)).json(body))
            .await
    }

    async fn get(&self, path: &str) -> reqwest::Response {
        self.send(self.http.get(format!("{}{path}", self.base)))
            .await
    }

    async fn post_json<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> T {
        let res = self.post(path, body).await;
        assert!(res.status().is_success(), "POST {path}: {}", res.status());
        res.json().await.unwrap()
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> T {
        let res = self.get(path).await;
        assert!(res.status().is_success(), "GET {path}: {}", res.status());
        res.json().await.unwrap()
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

    async fn register_hub(&self, name: &str) {
        let res = self
            .post(
                routes::SERVERS,
                &RegisterServer {
                    name: name.into(),
                    kind: "hub".into(),
                    version: "v1.0.0".into(),
                    role: ServerRole::Lobby,
                    address: format!("10.0.0.1:{}", 25565 + name.len()),
                    max_players: 100,
                },
            )
            .await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    async fn login(&self, uuid: Uuid, proxy: &str) -> LoginDecision {
        self.post_json(
            routes::PRESENCE_LOGIN,
            &LoginRequest {
                uuid,
                username: format!("p{}", &uuid.simple().to_string()[..6]),
                proxy: proxy.into(),
                protocol: Some(777),
            },
        )
        .await
    }

    async fn count(&self) -> u32 {
        self.get_json::<NetworkCount>(routes::PRESENCE_COUNT)
            .await
            .online
    }

    async fn servers(&self, since: u64) -> Poll<Vec<ServerInfo>> {
        self.get_json(&routes::since(routes::SERVERS, since)).await
    }

    async fn commands(&self, proxy: &str, since: u64) -> Poll<Vec<ProxyCommand>> {
        self.get_json(&routes::since(&routes::proxy_commands(proxy), since))
            .await
    }
}

fn names(poll: &Poll<Vec<ServerInfo>>) -> Vec<String> {
    match poll {
        Poll::Changed { data, .. } => data.iter().map(|s| s.name.clone()).collect(),
        Poll::Unchanged { .. } => panic!("expected a change, got {poll:?}"),
    }
}

/// The #15 gate: two proxies on two control plane instances see the same
/// server list and player count, and changes reach a waiting long poll on the
/// other instance straight away.
#[tokio::test]
async fn two_proxies_share_state() {
    let Some(redis) = redis_url() else { return };
    let prefix = format!("test-{}", Uuid::new_v4());
    let one = Instance::start(config(&redis, &prefix)).await;
    let two = Instance::start(config(&redis, &prefix)).await;

    let a = one.register_proxy("proxy-a").await;
    let b = two.register_proxy("proxy-b").await;
    assert_eq!(a.servers_revision, b.servers_revision);

    // Both proxies wait on the server list, each on its own instance.
    let wait_a = tokio::spawn({
        let one = Instance {
            base: one.base.clone(),
            http: one.http.clone(),
        };
        async move { one.servers(a.servers_revision).await }
    });
    let wait_b = tokio::spawn({
        let two = Instance {
            base: two.base.clone(),
            http: two.http.clone(),
        };
        async move { two.servers(b.servers_revision).await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    one.register_hub("hub-1").await;

    // Well inside the 25 s long poll: the change was pushed, not timed out.
    let in_time = Duration::from_secs(3);
    let seen_a = tokio::time::timeout(in_time, wait_a)
        .await
        .unwrap()
        .unwrap();
    let seen_b = tokio::time::timeout(in_time, wait_b)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(names(&seen_a), ["hub-1"]);
    assert_eq!(seen_a, seen_b);

    // Players log in through different proxies and instances.
    let p1 = Uuid::new_v4();
    let p2 = Uuid::new_v4();
    let hub = LoginDecision::Allow {
        server: "hub-1".into(),
    };
    assert_eq!(one.login(p1, "proxy-a").await, hub);
    assert_eq!(two.login(p2, "proxy-b").await, hub);
    assert_eq!(one.count().await, 2);
    assert_eq!(two.count().await, 2);

    // The same account can't be online twice.
    assert!(matches!(
        two.login(p1, "proxy-b").await,
        LoginDecision::Deny { .. }
    ));

    // A command posted on instance two reaches proxy-a's poll on instance one.
    let message = NetworkCommand::Message {
        player: p1,
        text: "§aHello".into(),
        from: Some("someone".into()),
    };
    let res = two.post(routes::COMMANDS, &message).await;
    assert_eq!(res.status(), StatusCode::ACCEPTED);
    let Poll::Changed { data, .. } = one.commands("proxy-a", a.commands_revision).await else {
        panic!("expected proxy-a's command");
    };
    assert_eq!(data, [ProxyCommand::from(message)]);

    // Switches show up in lookups from either instance.
    let res = one
        .post(
            routes::PRESENCE_SWITCH,
            &PlayerSwitched {
                uuid: p1,
                server: "hub-1".into(),
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let location: PlayerLocation = two.get_json(&routes::player(&p1)).await;
    assert_eq!(location.proxy, "proxy-a");
    assert_eq!(location.server.as_deref(), Some("hub-1"));

    // Lookups by name ignore case, from either instance.
    let username = format!("p{}", &p1.simple().to_string()[..6]);
    let by_name: PlayerLocation = one
        .get_json(&routes::player_by_name(&username.to_uppercase()))
        .await;
    assert_eq!(by_name, location);

    // A leave from the wrong proxy is ignored; the right one counts.
    for proxy in ["proxy-b", "proxy-a"] {
        let res = two
            .post(
                routes::PRESENCE_LEAVE,
                &PlayerLeft {
                    uuid: p1,
                    proxy: proxy.into(),
                },
            )
            .await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }
    assert_eq!(one.count().await, 1);
    assert_eq!(
        two.get(&routes::player(&p1)).await.status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        two.get(&routes::player_by_name(&username)).await.status(),
        StatusCode::NOT_FOUND
    );

    // A state change bumps the list for everyone; a player count alone doesn't.
    let Poll::Changed { revision, .. } = one.servers(0).await else {
        panic!()
    };
    let beat = |players, state| ServerHeartbeat { players, state };
    let res = two
        .post(
            &routes::server_heartbeat("hub-1"),
            &beat(5, ServerState::Ready),
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    // wait=0 answers at once.
    let quick: Poll<Vec<ServerInfo>> = tokio::time::timeout(
        Duration::from_secs(1),
        one.get_json(&routes::since_wait(routes::SERVERS, revision, 0)),
    )
    .await
    .expect("wait=0 should answer at once");
    assert_eq!(quick, Poll::Unchanged { revision });

    // Still waiting after a second means nothing changed.
    assert!(
        tokio::time::timeout(Duration::from_secs(1), one.servers(revision))
            .await
            .is_err()
    );
    two.post(
        &routes::server_heartbeat("hub-1"),
        &beat(5, ServerState::Draining),
    )
    .await;
    let Poll::Changed { data, .. } = one.servers(revision).await else {
        panic!("expected the state change")
    };
    assert_eq!(data[0].state, ServerState::Draining);
    assert_eq!(data[0].players, 5);
}

/// Proxies and servers that stop heartbeating are removed, with their players.
#[tokio::test]
async fn expiry_removes_dead_proxies_and_servers() {
    let Some(redis) = redis_url() else { return };
    let prefix = format!("test-{}", Uuid::new_v4());
    let mut config = config(&redis, &prefix);
    config.expiry = Duration::from_millis(600);
    config.long_poll = Duration::from_millis(300);
    let one = Instance::start(config).await;

    let registered = one.register_proxy("proxy-x").await;
    one.register_hub("hub-1").await;
    assert!(matches!(
        one.login(Uuid::new_v4(), "proxy-x").await,
        LoginDecision::Allow { .. }
    ));
    let Poll::Changed { revision, .. } = one.servers(registered.servers_revision).await else {
        panic!()
    };

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(names(&one.servers(revision).await), Vec::<String>::new());
    assert_eq!(one.count().await, 0);
    let res = one
        .post(
            &routes::proxy_heartbeat("proxy-x"),
            &ProxyHeartbeat {
                players: 0,
                draining: false,
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let err: ApiError = res.json().await.unwrap();
    assert_eq!(err.code, ErrorCode::NotFound);
}

/// With `kick_old`, a second login kicks the first session on its proxy.
#[tokio::test]
async fn kick_old_session() {
    let Some(redis) = redis_url() else { return };
    let prefix = format!("test-{}", Uuid::new_v4());
    let mut config = config(&redis, &prefix);
    config.duplicate_login = DuplicateLogin::KickOld;
    let one = Instance::start(config).await;

    let a = one.register_proxy("proxy-a").await;
    one.register_proxy("proxy-b").await;
    one.register_hub("hub-1").await;
    let player = Uuid::new_v4();
    assert!(matches!(
        one.login(player, "proxy-a").await,
        LoginDecision::Allow { .. }
    ));
    assert!(matches!(
        one.login(player, "proxy-b").await,
        LoginDecision::Allow { .. }
    ));

    let Poll::Changed { data, .. } = one.commands("proxy-a", a.commands_revision).await else {
        panic!("expected a kick")
    };
    assert!(matches!(&data[..], [ProxyCommand::Kick { player: p, .. }] if *p == player));
    let location: PlayerLocation = one.get_json(&routes::player(&player)).await;
    assert_eq!(location.proxy, "proxy-b");
    assert_eq!(one.count().await, 1);
}

/// Requests need the token; bad input gets an `ApiError`.
#[tokio::test]
async fn auth_and_errors() {
    let Some(redis) = redis_url() else { return };
    let one = Instance::start(config(&redis, &format!("test-{}", Uuid::new_v4()))).await;

    let res = one
        .http
        .get(format!("{}{}", one.base, routes::PRESENCE_COUNT))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let err: ApiError = res.json().await.unwrap();
    assert_eq!(err.code, ErrorCode::Unauthorized);

    let res = one
        .post(
            routes::PROXIES,
            &RegisterProxy {
                id: "bad id/with slash".into(),
                address: "x:1".into(),
                version: "test".into(),
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let res = one
        .send(
            one.http
                .post(format!("{}{}", one.base, routes::SERVERS))
                .body("{"),
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let err: ApiError = res.json().await.unwrap();
    assert_eq!(err.code, ErrorCode::BadRequest);

    // Removing twice: the second time there is nothing to remove.
    one.register_hub("hub-gone").await;
    one.register_proxy("proxy-gone").await;
    for path in [routes::server("hub-gone"), routes::proxy("proxy-gone")] {
        let url = format!("{}{path}", one.base);
        let res = one.send(one.http.delete(&url)).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT, "DELETE {path}");
        let res = one.send(one.http.delete(&url)).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "DELETE {path} again");
    }

    // Logging in through an unregistered proxy tells it to register.
    let res = one
        .post(
            routes::PRESENCE_LOGIN,
            &LoginRequest {
                uuid: Uuid::new_v4(),
                username: "x".into(),
                proxy: "nobody".into(),
                protocol: None,
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

/// Asks the agent port about `id`, like HAProxy's agent check.
async fn agent_check(agent: std::net::SocketAddr, id: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(agent).await.unwrap();
    stream
        .write_all(format!("{id}\n").as_bytes())
        .await
        .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();
    reply.trim().to_owned()
}

/// Draining a proxy turns its agent check to `drain` at once, tells the
/// proxy, and survives the proxy restarting.
#[tokio::test]
async fn drain_and_agent_checks() {
    let Some(redis) = redis_url() else { return };
    let config = config(&redis, &format!("test-{}", Uuid::new_v4()));
    let http = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let agent_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let agent = agent_listener.local_addr().unwrap();
    let base = format!("http://{}", http.local_addr().unwrap());
    tokio::spawn(async move { serve_all(http, Some(agent_listener), config).await.unwrap() });
    let one = Instance::connect(base).await;

    let registered = one.register_proxy("proxy-a").await;
    assert!(!registered.draining);
    assert_eq!(agent_check(agent, "proxy-a").await, "up ready");
    assert_eq!(agent_check(agent, "nobody").await, "down");

    let drain = |draining| SetDraining { draining };
    let res = one
        .post(&routes::proxy_drain("proxy-a"), &drain(true))
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(agent_check(agent, "proxy-a").await, "drain");
    let Poll::Changed { data, .. } = one.commands("proxy-a", registered.commands_revision).await
    else {
        panic!("expected a drain command")
    };
    assert_eq!(data, [ProxyCommand::Drain { draining: true }]);
    let proxies: Vec<ProxyInfo> = one.get_json(routes::PROXIES).await;
    assert!(proxies[0].draining);

    // A restart keeps the drain.
    assert!(one.register_proxy("proxy-a").await.draining);
    assert_eq!(agent_check(agent, "proxy-a").await, "drain");

    one.post(&routes::proxy_drain("proxy-a"), &drain(false))
        .await;
    assert_eq!(agent_check(agent, "proxy-a").await, "up ready");
    let res = one.post(&routes::proxy_drain("nobody"), &drain(true)).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
