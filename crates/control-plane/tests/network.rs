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
    fleet::{FleetSettings, FleetSettingsChange, FleetSettingsRecord, ProviderSettings},
    friend::{FriendAction, FriendReply},
    party::{PartyAction, PartyInfo, PartyReply},
    presence::{
        LoginDecision, LoginRequest, NetworkCount, PlayerLeft, PlayerLocation, PlayerSwitched,
    },
    proxy::{ProxyHeartbeat, ProxyInfo, ProxyRegistered, RegisterProxy, SetDraining},
    punishment::{
        IssuePunishment, PunishmentKind, PunishmentRecord, PunishmentReply, RevokePunishment,
    },
    queue::{JoinQueue, QueueStatus},
    routes,
    server::{
        RegisterServer, ServerHeartbeat, ServerInfo, ServerPlayer, ServerPlayersChanged,
        ServerRole, ServerState,
    },
    signal::{PlayerSignals, SignalBatch, SignalRecord},
    stats::{PlayerStats, PlayerStatsDelta, StatsBatch},
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

    async fn put<B: Serialize>(&self, path: &str, body: &B) -> reqwest::Response {
        self.send(self.http.put(format!("{}{path}", self.base)).json(body))
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
                ip: None,
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
        permissions: vec![],
        mute: None,
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
    let beat = |players, state| ServerHeartbeat {
        players,
        state,
        tick_ms: None,
        roster: None,
    };
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
                ip: None,
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

fn username(uuid: Uuid) -> String {
    format!("p{}", &uuid.simple().to_string()[..6])
}

/// Every message queued for `proxy` after `since`, and the new revision.
async fn messages(one: &Instance, proxy: &str, since: u64) -> (u64, Vec<String>) {
    match one.commands(proxy, since).await {
        Poll::Changed { revision, data } => (
            revision,
            data.into_iter()
                .filter_map(|c| match c {
                    ProxyCommand::Message { text, .. } => Some(text),
                    _ => None,
                })
                .collect(),
        ),
        Poll::Unchanged { revision } => (revision, Vec::new()),
    }
}

/// Invite, accept, chat, warp, hand-over and the party ending, with members
/// on different proxies.
#[tokio::test]
async fn parties() {
    let Some(redis) = redis_url() else { return };
    let mut config = config(&redis, &format!("test-{}", Uuid::new_v4()));
    config.long_poll = Duration::from_millis(200);
    let one = Instance::start(config).await;
    let a = one.register_proxy("proxy-a").await;
    let b = one.register_proxy("proxy-b").await;
    one.register_hub("hub-1").await;
    let [p1, p2, p3, p4] = [(); 4].map(|()| Uuid::new_v4());
    for (player, proxy) in [
        (p1, "proxy-a"),
        (p2, "proxy-b"),
        (p3, "proxy-b"),
        (p4, "proxy-a"),
    ] {
        assert!(matches!(
            one.login(player, proxy).await,
            LoginDecision::Allow { .. }
        ));
    }
    let act = |action: PartyAction| {
        let one = &one;
        async move {
            one.post_json::<_, PartyReply>(routes::PARTIES, &action)
                .await
        }
    };

    // Accepting needs an invite.
    let reply = act(PartyAction::Accept {
        player: p2,
        leader: username(p1),
    })
    .await;
    assert!(!reply.ok, "{reply:?}");

    for member in [p2, p3] {
        let reply = act(PartyAction::Invite {
            player: p1,
            target: username(member).to_uppercase(),
        })
        .await;
        assert!(reply.ok, "{reply:?}");
        let reply = act(PartyAction::Accept {
            player: member,
            leader: username(p1),
        })
        .await;
        assert!(reply.ok, "{reply:?}");
    }
    let party: PartyInfo = one.get_json(&routes::party(&p3)).await;
    assert_eq!(party.leader, p1);
    assert_eq!(
        party.members.iter().map(|m| m.uuid).collect::<Vec<_>>(),
        [p1, p2, p3]
    );

    // Only the leader invites; p4 isn't in a party.
    let reply = act(PartyAction::Invite {
        player: p2,
        target: username(p4),
    })
    .await;
    assert!(!reply.ok);
    let reply = act(PartyAction::Chat {
        player: p4,
        text: "hi".into(),
    })
    .await;
    assert!(!reply.ok);

    // Chat reaches members on both proxies, without colour codes from players.
    let (rev_b, _) = messages(&one, "proxy-b", b.commands_revision).await;
    let reply = act(PartyAction::Chat {
        player: p2,
        text: "§kgo team".into(),
    })
    .await;
    assert!(reply.ok);
    let (rev_b, seen) = messages(&one, "proxy-b", rev_b).await;
    assert_eq!(seen.len(), 2, "p2 and p3 are on proxy-b: {seen:?}");
    assert!(seen[0].ends_with("§7: kgo team"), "{seen:?}");
    let (rev_a, seen) = messages(&one, "proxy-a", a.commands_revision).await;
    assert!(seen.iter().any(|m| m.contains("go team")));

    // Warp sends the others to the leader's server.
    one.post(
        routes::PRESENCE_SWITCH,
        &PlayerSwitched {
            uuid: p1,
            server: "hub-1".into(),
        },
    )
    .await;
    let reply = act(PartyAction::Warp { player: p1 }).await;
    assert!(reply.ok, "{reply:?}");
    let Poll::Changed { data, .. } = one.commands("proxy-b", rev_b).await else {
        panic!()
    };
    let sends: Vec<Uuid> = data
        .iter()
        .filter_map(|c| match c {
            ProxyCommand::Send { player, server } if server == "hub-1" => Some(*player),
            _ => None,
        })
        .collect();
    assert_eq!(sends, [p2, p3]);
    let _ = rev_a;

    // The leader leaving the network hands over to the longest member.
    one.post(
        routes::PRESENCE_LEAVE,
        &PlayerLeft {
            uuid: p1,
            proxy: "proxy-a".into(),
        },
    )
    .await;
    let party: PartyInfo = one.get_json(&routes::party(&p2)).await;
    assert_eq!(party.leader, p2);
    assert_eq!(party.members.len(), 2);

    // Down to one member, the party ends.
    let reply = act(PartyAction::Leave { player: p3 }).await;
    assert!(reply.ok);
    assert_eq!(
        one.get(&routes::party(&p2)).await.status(),
        StatusCode::NOT_FOUND
    );
}

impl Instance {
    async fn register_game(&self, name: &str, kind: &str, max_players: u32, players: u32) {
        let res = self
            .post(
                routes::SERVERS,
                &RegisterServer {
                    name: name.into(),
                    kind: kind.into(),
                    version: "v1.0.0".into(),
                    role: ServerRole::Game,
                    address: format!("{name}:25565"),
                    max_players,
                },
            )
            .await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        let beat = ServerHeartbeat {
            players,
            state: ServerState::Ready,
            tick_ms: Some(7.5),
            roster: None,
        };
        let res = self.post(&routes::server_heartbeat(name), &beat).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }
}

/// Where the matchmaker sent each player, from the proxies' command queues.
async fn sends(one: &Instance, proxies: &[(&str, u64)], wait: Duration) -> Vec<(Uuid, String)> {
    tokio::time::sleep(wait).await;
    let mut out = Vec::new();
    for (proxy, since) in proxies {
        if let Poll::Changed { data, .. } = one.commands(proxy, *since).await {
            for command in data {
                if let ProxyCommand::Send { player, server } = command {
                    out.push((player, server));
                }
            }
        }
    }
    out
}

/// The #21 gate: a party of four on two proxies queues together and lands
/// in the same game, on the one server with room for all of them.
#[tokio::test]
async fn party_of_four_queues_together() {
    let Some(redis) = redis_url() else { return };
    let mut config = config(&redis, &format!("test-{}", Uuid::new_v4()));
    config.long_poll = Duration::from_millis(200);
    let one = Instance::start(config).await;
    let a = one.register_proxy("proxy-a").await;
    let b = one.register_proxy("proxy-b").await;
    one.register_hub("hub-1").await;
    // sw-busy has room for 2, sw-free for 12.
    one.register_game("sw-busy", "skywars", 12, 10).await;
    one.register_game("sw-free", "skywars", 12, 0).await;
    let players = [(); 5].map(|()| Uuid::new_v4());
    let [leader, m1, m2, m3, solo] = players;
    for (i, player) in players.iter().enumerate() {
        let proxy = if i % 2 == 0 { "proxy-a" } else { "proxy-b" };
        assert!(matches!(
            one.login(*player, proxy).await,
            LoginDecision::Allow { .. }
        ));
    }
    for member in [m1, m2, m3] {
        let invite = PartyAction::Invite {
            player: leader,
            target: username(member),
        };
        assert!(
            one.post_json::<_, PartyReply>(routes::PARTIES, &invite)
                .await
                .ok
        );
        let accept = PartyAction::Accept {
            player: member,
            leader: username(leader),
        };
        assert!(
            one.post_json::<_, PartyReply>(routes::PARTIES, &accept)
                .await
                .ok
        );
    }

    // Members can't queue the party; the leader can.
    let res = one
        .post(
            routes::QUEUES,
            &JoinQueue {
                kind: "skywars".into(),
                player: m2,
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let res = one
        .post(
            routes::QUEUES,
            &JoinQueue {
                kind: "nothing".into(),
                player: solo,
            },
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::CONFLICT,
        "no servers of that kind"
    );
    let status: QueueStatus = one
        .post_json(
            routes::QUEUES,
            &JoinQueue {
                kind: "skywars".into(),
                player: leader,
            },
        )
        .await;
    assert_eq!(status.queued, 4);
    let _: QueueStatus = one
        .post_json(
            routes::QUEUES,
            &JoinQueue {
                kind: "skywars".into(),
                player: solo,
            },
        )
        .await;

    let placed = sends(
        &one,
        &[
            ("proxy-a", a.commands_revision),
            ("proxy-b", b.commands_revision),
        ],
        Duration::from_millis(1500),
    )
    .await;
    let party_servers: Vec<&str> = placed
        .iter()
        .filter(|(p, _)| [leader, m1, m2, m3].contains(p))
        .map(|(_, s)| s.as_str())
        .collect();
    assert_eq!(party_servers, ["sw-free"; 4], "{placed:?}");
    // The solo player fills the fullest server with room.
    assert!(placed.contains(&(solo, "sw-busy".into())), "{placed:?}");
}

/// A member disconnecting takes the party out of the queue.
#[tokio::test]
async fn disconnect_leaves_the_queue() {
    let Some(redis) = redis_url() else { return };
    let one = Instance::start(config(&redis, &format!("test-{}", Uuid::new_v4()))).await;
    one.register_proxy("proxy-a").await;
    one.register_hub("hub-1").await;
    // Full: nothing gets placed while we look.
    one.register_game("sw-full", "skywars", 2, 2).await;
    let [leader, member] = [(); 2].map(|()| Uuid::new_v4());
    for player in [leader, member] {
        one.login(player, "proxy-a").await;
    }
    let invite = PartyAction::Invite {
        player: leader,
        target: username(member),
    };
    one.post_json::<_, PartyReply>(routes::PARTIES, &invite)
        .await;
    let accept = PartyAction::Accept {
        player: member,
        leader: username(leader),
    };
    one.post_json::<_, PartyReply>(routes::PARTIES, &accept)
        .await;
    one.post_json::<_, QueueStatus>(
        routes::QUEUES,
        &JoinQueue {
            kind: "skywars".into(),
            player: leader,
        },
    )
    .await;

    one.post(
        routes::PRESENCE_LEAVE,
        &PlayerLeft {
            uuid: member,
            proxy: "proxy-a".into(),
        },
    )
    .await;
    let res = one
        .send(
            one.http
                .delete(format!("{}{}", one.base, routes::queue_entry(&leader))),
        )
        .await;
    assert_eq!(
        res.status(),
        StatusCode::NOT_FOUND,
        "the entry went with the member"
    );
}

/// A fresh database for one test, or `None` (skip) without
/// `HALLOW_TEST_DATABASE_URL`. Returns its URL and an admin pool to drop it.
async fn test_database() -> Option<(String, String, hallow_db::PgPool)> {
    use hallow_db::sqlx::{self, Executor, postgres::PgPoolOptions};
    let url = std::env::var("HALLOW_TEST_DATABASE_URL").ok();
    let Some(url) = url else {
        assert!(
            std::env::var_os("CI").is_none(),
            "HALLOW_TEST_DATABASE_URL isn't set in CI"
        );
        eprintln!("HALLOW_TEST_DATABASE_URL isn't set; skipping");
        return None;
    };
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let name = format!("hallow_cp_test_{}", Uuid::new_v4().simple());
    admin
        .execute(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
        .await
        .unwrap();
    let (base, _) = url.rsplit_once('/').unwrap();
    Some((format!("{base}/{name}"), name, admin))
}

async fn login_as(one: &Instance, uuid: Uuid, username: &str, proxy: &str) -> LoginDecision {
    one.post_json(
        routes::PRESENCE_LOGIN,
        &LoginRequest {
            uuid,
            username: username.into(),
            proxy: proxy.into(),
            protocol: Some(777),
            ip: Some("203.0.113.7".into()),
        },
    )
    .await
}

/// With a database: admins get `*`, bans refuse logins, sessions are kept.
#[tokio::test]
async fn database_logins() {
    use hallow_db::sqlx::{self, Executor};
    let Some(redis) = redis_url() else { return };
    let Some((db_url, db_name, admin)) = test_database().await else {
        return;
    };
    let mut config = config(&redis, &format!("test-{}", Uuid::new_v4()));
    config.database_url = Some(db_url.clone());
    config.admins = vec!["Boss".into()];
    let one = Instance::start(config).await;
    let pool = hallow_db::connect(&db_url).await.unwrap();
    one.register_proxy("proxy-a").await;
    one.register_hub("hub-1").await;

    let (boss, player) = (Uuid::new_v4(), Uuid::new_v4());
    let LoginDecision::Allow { permissions, .. } = login_as(&one, boss, "boss", "proxy-a").await
    else {
        panic!("boss refused")
    };
    assert_eq!(permissions, ["*"]);
    let LoginDecision::Allow { permissions, .. } = login_as(&one, player, "pleb", "proxy-a").await
    else {
        panic!("player refused")
    };
    assert!(permissions.is_empty());

    // Sessions follow the player and close at logout.
    one.post(
        routes::PRESENCE_SWITCH,
        &PlayerSwitched {
            uuid: player,
            server: "hub-1".into(),
        },
    )
    .await;
    one.post(
        routes::PRESENCE_LEAVE,
        &PlayerLeft {
            uuid: player,
            proxy: "proxy-a".into(),
        },
    )
    .await;
    let (server, closed): (Option<String>, bool) = sqlx::query_as(
        "SELECT last_server, logout_at IS NOT NULL FROM hallow.sessions WHERE account_uuid = $1",
    )
    .bind(player)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(server.as_deref(), Some("hub-1"));
    assert!(closed);

    // A ban refuses the next login, before it reaches presence.
    hallow_db::add_punishment(
        &pool,
        &hallow_db::NewPunishment {
            kind: hallow_db::PunishmentKind::Ban,
            target: player,
            issuer: hallow_db::Actor::Account(boss),
            reason: "testing bans",
            expires_at: Some(chrono::Utc::now() + chrono::TimeDelta::days(3)),
        },
    )
    .await
    .unwrap();
    let LoginDecision::Deny { reason } = login_as(&one, player, "pleb", "proxy-a").await else {
        panic!("banned player let in")
    };
    assert!(
        reason.contains("testing bans") && reason.contains("2d 23h"),
        "{reason}"
    );
    assert_eq!(one.count().await, 1);

    // A lost leave: logging in again closes the stale session.
    let LoginDecision::Allow { .. } = login_as(&one, boss, "boss", "proxy-a").await else {
        panic!()
    };
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM hallow.sessions WHERE account_uuid = $1 AND logout_at IS NULL",
    )
    .bind(boss)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(open, 1);

    pool.close().await;
    admin
        .execute(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE {db_name} WITH (FORCE)"
        )))
        .await
        .unwrap();
}

/// Staff punishments: mutes reach the player's proxy, bans kick and refuse
/// logins, offline players resolve by name, history reads back.
#[tokio::test]
async fn punishments_in_game() {
    use hallow_db::sqlx::{self, Executor};
    let Some(redis) = redis_url() else { return };
    let Some((db_url, db_name, admin)) = test_database().await else {
        return;
    };
    let mut config = config(&redis, &format!("test-{}", Uuid::new_v4()));
    config.database_url = Some(db_url);
    config.long_poll = Duration::from_millis(200);
    let one = Instance::start(config).await;
    one.register_proxy("proxy-a").await;
    let b = one.register_proxy("proxy-b").await;
    one.register_hub("hub-1").await;
    let (boss, target) = (Uuid::new_v4(), Uuid::new_v4());
    login_as(&one, boss, "boss", "proxy-a").await;
    login_as(&one, target, "Griefer", "proxy-b").await;

    let issue = |kind, duration_seconds| IssuePunishment {
        kind,
        target: "griefer".into(),
        issuer: Some(boss),
        reason: "being rude".into(),
        duration_seconds,
    };
    let reply: PunishmentReply = one
        .post_json(routes::PUNISHMENTS, &issue(PunishmentKind::Mute, Some(600)))
        .await;
    assert!(reply.ok, "{reply:?}");
    let reply: PunishmentReply = one
        .post_json(routes::PUNISHMENTS, &issue(PunishmentKind::Mute, None))
        .await;
    assert!(
        !reply.ok && reply.message.contains("already muted"),
        "{reply:?}"
    );
    let Poll::Changed { revision, data } = one.commands("proxy-b", b.commands_revision).await
    else {
        panic!()
    };
    assert!(
        data.iter().any(|c| matches!(c, ProxyCommand::Mute { player, mute: Some(m) } if *player == target && m.until_ms.is_some())),
        "{data:?}"
    );

    // A ban kicks the player where they are, and refuses the next login.
    let reply: PunishmentReply = one
        .post_json(routes::PUNISHMENTS, &issue(PunishmentKind::Ban, None))
        .await;
    assert!(
        reply.ok && reply.message.contains("permanently"),
        "{reply:?}"
    );
    let Poll::Changed { data, .. } = one.commands("proxy-b", revision).await else {
        panic!()
    };
    assert!(
        data.iter().any(|c| matches!(c, ProxyCommand::Kick { player, reason } if *player == target && reason.contains("being rude"))),
        "{data:?}"
    );
    one.post(
        routes::PRESENCE_LEAVE,
        &PlayerLeft {
            uuid: target,
            proxy: "proxy-b".into(),
        },
    )
    .await;
    assert!(matches!(
        login_as(&one, target, "Griefer", "proxy-b").await,
        LoginDecision::Deny { .. }
    ));

    // Offline players are found by name; the console can act.
    let warn = IssuePunishment {
        kind: PunishmentKind::Warn,
        issuer: None,
        ..issue(PunishmentKind::Warn, None)
    };
    assert!(
        one.post_json::<_, PunishmentReply>(routes::PUNISHMENTS, &warn)
            .await
            .ok
    );

    let unban = RevokePunishment {
        kind: PunishmentKind::Ban,
        target: "GRIEFER".into(),
        issuer: Some(boss),
        reason: None,
    };
    assert!(
        one.post_json::<_, PunishmentReply>(routes::PUNISHMENTS_REVOKE, &unban)
            .await
            .ok
    );
    let LoginDecision::Allow { mute, .. } = login_as(&one, target, "Griefer", "proxy-b").await
    else {
        panic!("unbanned player refused")
    };
    assert_eq!(
        mute.map(|m| m.reason).as_deref(),
        Some("being rude"),
        "the mute still holds"
    );

    let history: Vec<PunishmentRecord> = one.get_json(&routes::punishment_history("griefer")).await;
    let kinds: Vec<(PunishmentKind, bool, &str)> = history
        .iter()
        .map(|r| (r.kind, r.active, r.issuer.as_str()))
        .collect();
    assert_eq!(
        kinds,
        [
            (PunishmentKind::Warn, false, "console"),
            (PunishmentKind::Ban, false, "boss"),
            (PunishmentKind::Mute, true, "boss"),
        ]
    );

    admin
        .execute(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE {db_name} WITH (FORCE)"
        )))
        .await
        .unwrap();
}

/// Friends across proxies: requests and accepts reach the other player,
/// the list shows who's online, and friends hear about joins and leaves.
#[tokio::test]
async fn friends_across_proxies() {
    use hallow_db::sqlx::{self, Executor};
    let Some(redis) = redis_url() else { return };
    let Some((db_url, db_name, admin)) = test_database().await else {
        return;
    };
    let mut config = config(&redis, &format!("test-{}", Uuid::new_v4()));
    config.database_url = Some(db_url);
    config.long_poll = Duration::from_millis(200);
    let one = Instance::start(config).await;
    let a = one.register_proxy("proxy-a").await;
    let b = one.register_proxy("proxy-b").await;
    one.register_hub("hub-1").await;
    let (alice, bob) = (Uuid::new_v4(), Uuid::new_v4());
    login_as(&one, alice, "alice", "proxy-a").await;
    login_as(&one, bob, "bob", "proxy-b").await;
    let act = |action: FriendAction| {
        let one = &one;
        async move {
            one.post_json::<_, FriendReply>(routes::FRIENDS, &action)
                .await
        }
    };

    let reply = act(FriendAction::Add {
        player: alice,
        target: "BOB".into(),
    })
    .await;
    assert!(reply.ok, "{reply:?}");
    let (rev_b, seen) = messages(&one, "proxy-b", b.commands_revision).await;
    assert!(
        seen.iter()
            .any(|m| m.contains("alice sent you a friend request")),
        "{seen:?}"
    );
    let reply = act(FriendAction::Accept {
        player: bob,
        target: "alice".into(),
    })
    .await;
    assert!(reply.ok, "{reply:?}");
    let (rev_a, seen) = messages(&one, "proxy-a", a.commands_revision).await;
    assert!(seen.iter().any(|m| m.contains("bob accepted")), "{seen:?}");

    let list = act(FriendAction::List { player: alice }).await;
    assert!(
        list.message.contains("1 online of 1") && list.message.contains("bob"),
        "{list:?}"
    );

    // Friends hear about leaves and joins.
    one.post(
        routes::PRESENCE_LEAVE,
        &PlayerLeft {
            uuid: bob,
            proxy: "proxy-b".into(),
        },
    )
    .await;
    let (rev_a, seen) = messages(&one, "proxy-a", rev_a).await;
    assert!(
        seen.iter().any(|m| m.contains("bob") && m.contains("left")),
        "{seen:?}"
    );
    let list = act(FriendAction::List { player: alice }).await;
    assert!(list.message.contains("0 online of 1"), "{list:?}");
    login_as(&one, bob, "bob", "proxy-b").await;
    let (_, seen) = messages(&one, "proxy-a", rev_a).await;
    assert!(
        seen.iter()
            .any(|m| m.contains("bob") && m.contains("joined")),
        "{seen:?}"
    );
    let _ = rev_b;

    let reply = act(FriendAction::Remove {
        player: bob,
        target: "alice".into(),
    })
    .await;
    assert!(reply.ok);
    let reply = act(FriendAction::Accept {
        player: bob,
        target: "alice".into(),
    })
    .await;
    assert!(!reply.ok, "nothing to accept");

    admin
        .execute(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE {db_name} WITH (FORCE)"
        )))
        .await
        .unwrap();
}

/// `/metrics` needs no token and reports players, servers, tick times and
/// queues.
#[tokio::test]
async fn metrics_endpoint() {
    let Some(redis) = redis_url() else { return };
    let one = Instance::start(config(&redis, &format!("test-{}", Uuid::new_v4()))).await;
    one.register_proxy("proxy-a").await;
    one.register_hub("hub-1").await;
    one.register_game("sw-1", "skywars", 12, 12).await;
    let player = Uuid::new_v4();
    one.login(player, "proxy-a").await;
    one.post_json::<_, QueueStatus>(
        routes::QUEUES,
        &JoinQueue {
            kind: "skywars".into(),
            player,
        },
    )
    .await;

    let res = one
        .http
        .get(format!("{}/metrics", one.base))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.text().await.unwrap();
    for line in [
        "hallow_players_online 1",
        "hallow_players_max 1000",
        "hallow_proxy_players{proxy=\"proxy-a\",draining=\"false\"} 0",
        "hallow_server_players{server=\"sw-1\",kind=\"skywars\",role=\"game\",state=\"ready\"} 12",
        "hallow_server_tick_ms{server=\"sw-1\",kind=\"skywars\"} 7.5",
        "hallow_queue_players{kind=\"skywars\"} 1",
    ] {
        assert!(
            body.lines().any(|l| l == line),
            "missing {line:?} in:\n{body}"
        );
    }
}

/// Signals go in from a proxy and come back out by player name.
#[tokio::test]
async fn signals_roundtrip() {
    use hallow_db::sqlx::{self, Executor};
    let Some(redis) = redis_url() else { return };
    let Some((db_url, db_name, admin)) = test_database().await else {
        return;
    };
    let mut config = config(&redis, &format!("test-{}", Uuid::new_v4()));
    config.database_url = Some(db_url);
    let one = Instance::start(config).await;
    one.register_proxy("proxy-a").await;
    one.register_hub("hub-1").await;
    let player = Uuid::new_v4();
    login_as(&one, player, "Clicker", "proxy-a").await;

    let batch = SignalBatch {
        source: "proxy-a".into(),
        samples: vec![PlayerSignals {
            player,
            at_ms: 1_800_000_000_000,
            window_ms: 30_000,
            protocol: Some(777),
            metrics: [("attacks".to_owned(), 42.0), ("cps_max".to_owned(), 15.5)].into(),
        }],
    };
    let res = one.post(routes::SIGNALS, &batch).await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    let records: Vec<SignalRecord> = one.get_json(&routes::signals_of("clicker")).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].source, "proxy-a");
    assert_eq!(records[0].at_ms, 1_800_000_000_000);
    assert_eq!(records[0].metrics["cps_max"], 15.5);

    admin
        .execute(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE {db_name} WITH (FORCE)"
        )))
        .await
        .unwrap();
}

/// Fleet settings: defaults until changed, field errors for bad values,
/// and a history of every change.
#[tokio::test]
async fn fleet_settings_roundtrip() {
    use hallow_db::sqlx::{self, Executor};
    let Some(redis) = redis_url() else { return };
    let Some((db_url, db_name, admin)) = test_database().await else {
        return;
    };
    let mut config = config(&redis, &format!("test-{}", Uuid::new_v4()));
    config.database_url = Some(db_url);
    let one = Instance::start(config).await;

    // Needs the token like everything else.
    let res = one
        .http
        .get(format!("{}{}", one.base, routes::FLEET_SETTINGS))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let record: FleetSettingsRecord = one.get_json(routes::FLEET_SETTINGS).await;
    assert_eq!(record.settings, FleetSettings::default());
    assert!(record.settings.dry_run);
    assert_eq!(record.updated_at, None);
    assert_eq!(record.updated_by, None);
    let history: Vec<FleetSettingsChange> = one.get_json(&routes::fleet_settings_history(10)).await;
    assert!(history.is_empty());

    // Invalid values, and a wrong type, each come back against their field.
    let res = one
        .put(
            routes::FLEET_SETTINGS,
            &serde_json::json!({
                "settings": {
                    "timezone": "Nowhere/Special",
                    "monthly_budget": "-3.00",
                    "tighten_from": 2.0,
                    "max_nodes": -1,
                    "scale_down_cooldown_secs": -60,
                },
                "updated_by": "alice",
            }),
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let err: ApiError = res.json().await.unwrap();
    assert_eq!(err.code, ErrorCode::BadRequest);
    let fields: Vec<&str> = err.fields.keys().map(String::as_str).collect();
    assert_eq!(
        fields,
        [
            "max_nodes",
            "monthly_budget",
            "scale_down_cooldown_secs",
            "tighten_from",
            "timezone"
        ]
    );
    let res = one
        .put(
            routes::FLEET_SETTINGS,
            &serde_json::json!({"settings": {}, "updated_by": ""}),
        )
        .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let err: ApiError = res.json().await.unwrap();
    assert!(err.fields.contains_key("updated_by"), "{err:?}");
    // Nothing was saved.
    let record: FleetSettingsRecord = one.get_json(routes::FLEET_SETTINGS).await;
    assert_eq!(record.updated_by, None);

    let mut settings = FleetSettings {
        dry_run: false,
        timezone: "Europe/Berlin".into(),
        monthly_budget: Some("120.00".into()),
        daily_cap: None,
        max_nodes: 6,
        providers: vec![ProviderSettings {
            name: "hetzner-cloud".into(),
            enabled: true,
            regions: vec!["fsn1".into(), "nbg1".into()],
            machine_types: vec!["cpx41".into()],
        }],
        ..FleetSettings::default()
    };
    let res = one
        .put(
            routes::FLEET_SETTINGS,
            &serde_json::json!({"settings": settings, "updated_by": "alice"}),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);
    let saved: FleetSettingsRecord = res.json().await.unwrap();
    assert_eq!(saved.settings, settings);
    assert_eq!(saved.updated_by.as_deref(), Some("alice"));
    assert!(saved.updated_at.is_some());
    let record: FleetSettingsRecord = one.get_json(routes::FLEET_SETTINGS).await;
    assert_eq!(record, saved);

    settings.paused = true;
    let res = one
        .put(
            routes::FLEET_SETTINGS,
            &serde_json::json!({"settings": settings, "updated_by": "bob"}),
        )
        .await;
    assert_eq!(res.status(), StatusCode::OK);

    let history: Vec<FleetSettingsChange> = one.get_json(&routes::fleet_settings_history(10)).await;
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].updated_by, "bob");
    assert!(history[0].after.paused);
    assert!(!history[0].before.as_ref().unwrap().paused);
    assert_eq!(history[1].updated_by, "alice");
    assert_eq!(history[1].before, None);
    assert_eq!(history[1].after.timezone, "Europe/Berlin");
    let latest: Vec<FleetSettingsChange> = one.get_json(&routes::fleet_settings_history(1)).await;
    assert_eq!(latest.len(), 1);
    assert_eq!(latest[0].id, history[0].id);

    admin
        .execute(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE {db_name} WITH (FORCE)"
        )))
        .await
        .unwrap();
}

fn on_server(uuid: Uuid, username: &str) -> ServerPlayer {
    ServerPlayer {
        uuid,
        username: username.into(),
    }
}

async fn players_on(instance: &Instance, server: &str) -> Vec<String> {
    instance
        .get_json::<Vec<ServerPlayer>>(&routes::server_players(server))
        .await
        .into_iter()
        .map(|p| p.username)
        .collect()
}

/// Servers report who is on them, through either instance: joins and leaves
/// as they happen, and the whole list with heartbeats to correct drift.
#[tokio::test]
async fn servers_report_their_players() {
    let Some(redis) = redis_url() else { return };
    let prefix = format!("test-{}", Uuid::new_v4());
    let one = Instance::start(config(&redis, &prefix)).await;
    let two = Instance::start(config(&redis, &prefix)).await;
    one.register_hub("hub-1").await;
    one.register_hub("hub-22").await;
    let (alex, steve, notch) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

    let res = one
        .post(
            &routes::server_players("hub-1"),
            &ServerPlayersChanged {
                joined: vec![on_server(steve, "Steve"), on_server(alex, "alex")],
                left: vec![],
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(players_on(&two, "hub-1").await, ["alex", "Steve"]);

    // Steve turns up on hub-22 before hub-1 reports him leaving.
    let res = two
        .post(
            &routes::server_players("hub-22"),
            &ServerPlayersChanged {
                joined: vec![on_server(steve, "Steve")],
                left: vec![],
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(players_on(&one, "hub-1").await, ["alex"]);
    assert_eq!(players_on(&one, "hub-22").await, ["Steve"]);
    // hub-1's late leave doesn't take him off hub-22.
    let res = one
        .post(
            &routes::server_players("hub-1"),
            &ServerPlayersChanged {
                joined: vec![],
                left: vec![steve],
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(players_on(&one, "hub-22").await, ["Steve"]);

    // A heartbeat's roster replaces the list: alex's leave was lost, Notch's
    // join too.
    let res = two
        .post(
            &routes::server_heartbeat("hub-1"),
            &ServerHeartbeat {
                players: 1,
                state: ServerState::Ready,
                tick_ms: None,
                roster: Some(vec![on_server(notch, "Notch")]),
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(players_on(&one, "hub-1").await, ["Notch"]);
    // A heartbeat without one leaves it alone.
    let res = one
        .post(
            &routes::server_heartbeat("hub-1"),
            &ServerHeartbeat {
                players: 1,
                state: ServerState::Ready,
                tick_ms: None,
                roster: None,
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    assert_eq!(players_on(&one, "hub-1").await, ["Notch"]);

    // Unknown servers are refused, so the plugin registers again.
    let res = one
        .post(
            &routes::server_players("nope"),
            &ServerPlayersChanged {
                joined: vec![on_server(alex, "alex")],
                left: vec![],
            },
        )
        .await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        one.get(&routes::server_players("nope")).await.status(),
        StatusCode::NOT_FOUND
    );

    // A server that restarts, or goes, takes its players with it.
    one.register_hub("hub-1").await;
    assert!(players_on(&one, "hub-1").await.is_empty());
    let res = one
        .send(
            one.http
                .delete(format!("{}{}", one.base, routes::server("hub-22"))),
        )
        .await;
    assert_eq!(res.status(), StatusCode::NO_CONTENT);
    one.register_hub("hub-22").await;
    assert!(players_on(&two, "hub-22").await.is_empty());
}

/// Stats go in from a server as increases and come back out as totals, by
/// name or UUID.
#[tokio::test]
async fn stats_add_up() {
    use hallow_db::sqlx::{self, Executor};
    let Some(redis) = redis_url() else { return };
    let Some((db_url, db_name, admin)) = test_database().await else {
        return;
    };
    let mut config = config(&redis, &format!("test-{}", Uuid::new_v4()));
    config.database_url = Some(db_url);
    let one = Instance::start(config).await;
    one.register_proxy("proxy-a").await;
    one.register_hub("hub-1").await;
    let (player, stranger) = (Uuid::new_v4(), Uuid::new_v4());
    login_as(&one, player, "Fighter", "proxy-a").await;

    let batch = |kind: &str, stats: &[(&str, i64)]| StatsBatch {
        source: "game-1".into(),
        kind: kind.into(),
        players: [player, stranger]
            .into_iter()
            .map(|player| PlayerStatsDelta {
                player,
                stats: stats.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect(),
            })
            .collect(),
    };
    for body in [
        batch(
            "skywars",
            &[("kills", 2), ("play_time_s", 30), ("joins", 1)],
        ),
        batch("skywars", &[("kills", 1), ("wins", 1)]),
        batch("hub", &[("play_time_s", 12)]),
    ] {
        let res = one.post(routes::STATS, &body).await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }
    let stats: PlayerStats = one.get_json(&routes::stats_of("fighter")).await;
    assert_eq!(stats.uuid, player);
    assert_eq!(stats.username, "Fighter");
    assert_eq!(stats.kinds["skywars"]["kills"], 3);
    assert_eq!(stats.kinds["skywars"]["wins"], 1);
    assert_eq!(stats.kinds["skywars"]["play_time_s"], 30);
    assert_eq!(stats.kinds["hub"]["play_time_s"], 12);
    let by_uuid: PlayerStats = one.get_json(&routes::stats_of(&player.to_string())).await;
    assert_eq!(by_uuid, stats);

    // The stranger never joined: nothing was kept.
    assert_eq!(
        one.get(&routes::stats_of(&stranger.to_string()))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
    // Bad names and negative increases are refused whole.
    for bad in [
        batch("skywars", &[("Kills", 1)]),
        batch("skywars", &[("kills", -5)]),
    ] {
        let res = one.post(routes::STATS, &bad).await;
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
    let stats: PlayerStats = one.get_json(&routes::stats_of("Fighter")).await;
    assert_eq!(stats.kinds["skywars"]["kills"], 3);

    admin
        .execute(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE {db_name} WITH (FORCE)"
        )))
        .await
        .unwrap();
}
