//! Stats end to end: a game server reports finished games with its scoped
//! token, the totals and recent games show in leaderboards and profiles,
//! display lines carry rank prefixes, and the public API answers without a
//! token but within its rate limit.
//!
//! Needs `HALLOW_TEST_REDIS_URL` and `HALLOW_TEST_DATABASE_URL`; without
//! them the test passes without doing anything (except in CI, where it
//! fails).

use std::{collections::BTreeMap, time::Duration};

use hallow_control_plane::{Config, public::PublicConfig, serve};
use hallow_db::{
    Edition, Login, Rank,
    sqlx::{self, Executor, postgres::PgPoolOptions},
};
use hallow_proto::{
    public::{self, BoardKind, NetworkSummary, PlayerMatch, PlayerProfile},
    routes,
    stats::{DisplayRequest, GamePlayer, GameResult, Leaderboard, PlayerDisplay, Window},
};
use reqwest::StatusCode;
use uuid::Uuid;

const TOKEN: &str = "test-token-0123456789";
const GAME_TOKEN: &str = "duels-scoped-token-0123456789";

fn env_or_skip(var: &str) -> Option<String> {
    let value = std::env::var(var).ok();
    if value.is_none() {
        assert!(std::env::var_os("CI").is_none(), "{var} isn't set in CI");
        eprintln!("{var} isn't set; skipping");
    }
    value
}

fn now_ms() -> u64 {
    u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap()
}

#[tokio::test]
async fn games_feed_leaderboards_and_profiles() {
    let (Some(redis), Some(db_url)) = (
        env_or_skip("HALLOW_TEST_REDIS_URL"),
        env_or_skip("HALLOW_TEST_DATABASE_URL"),
    ) else {
        return;
    };
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&db_url)
        .await
        .unwrap();
    let db_name = format!("hallow_stats_test_{}", Uuid::new_v4().simple());
    admin
        .execute(sqlx::AssertSqlSafe(format!("CREATE DATABASE {db_name}")))
        .await
        .unwrap();
    let (base_url, _) = db_url.rsplit_once('/').unwrap();
    let database_url = format!("{base_url}/{db_name}");

    let mut config = Config::new(redis, TOKEN);
    config.key_prefix = format!("test-{}", Uuid::new_v4());
    config.server_tokens =
        hallow_control_plane::auth::parse_static_tokens(&format!("duels-1:duels:{GAME_TOKEN}"))
            .unwrap();
    config.database_url = Some(database_url.clone());
    config.public = PublicConfig {
        per_minute: 6,
        burst: 30,
        ..PublicConfig::default()
    };
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

    // Two players who joined, one with a rank.
    let pool = hallow_db::connect(&database_url).await.unwrap();
    let (ana, bo) = (Uuid::new_v4(), Uuid::new_v4());
    for (uuid, username) in [(ana, "Ana"), (bo, "bo_rider")] {
        hallow_db::upsert_account(
            &pool,
            &Login {
                uuid,
                username,
                edition: Edition::Java,
                ip: None,
            },
        )
        .await
        .unwrap();
    }
    hallow_db::upsert_rank(
        &pool,
        &Rank {
            name: "vip".into(),
            display_prefix: "&a[VIP] ".into(),
            weight: 10,
            permissions: vec![],
            is_default: false,
        },
    )
    .await
    .unwrap();
    hallow_db::grant_rank(&pool, bo, "vip", None, None)
        .await
        .unwrap();

    let ended = now_ms();
    let game = GameResult {
        id: Uuid::new_v4(),
        source: "duels-1".into(),
        kind: "duels".into(),
        map: Some("arena".into()),
        started_ms: ended - 60_000,
        ended_ms: ended,
        players: vec![
            GamePlayer {
                player: ana,
                won: true,
                placement: Some(1),
                stats: BTreeMap::from([("kills".into(), 3)]),
                ..GamePlayer::default()
            },
            GamePlayer {
                player: bo,
                placement: Some(2),
                stats: BTreeMap::from([("kills".into(), 1), ("deaths".into(), 1)]),
                ..GamePlayer::default()
            },
        ],
    };
    let post = |token: &'static str, game: &GameResult| {
        http.post(format!("{base}{}", routes::GAMES))
            .bearer_auth(token)
            .json(game)
            .send()
    };
    // Twice: the second is the same game and counts nothing.
    for _ in 0..2 {
        let res = post(GAME_TOKEN, &game).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }
    // A server can't report for another.
    let mut other = game.clone();
    other.id = Uuid::new_v4();
    other.source = "duels-2".into();
    assert_eq!(
        post(GAME_TOKEN, &other).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    // A broken result is refused whole.
    let mut broken = game.clone();
    broken.id = Uuid::new_v4();
    broken.players[0].stats.insert("kills".into(), -1);
    assert_eq!(
        post(GAME_TOKEN, &broken).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );

    // The private leaderboard needs the shared token.
    let board_path = routes::leaderboard_query("duels", "wins", Window::Week, 10);
    let res = http
        .get(format!("{base}{board_path}"))
        .bearer_auth(GAME_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let board: Leaderboard = http
        .get(format!("{base}{board_path}"))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(board.window, Window::Week);
    assert!(board.since_ms.is_some());
    assert_eq!(board.entries.len(), 1);
    assert_eq!(board.entries[0].username, "Ana");
    assert_eq!(board.entries[0].value, 1);

    // Display lines for chat and the tab list, with the scoped token.
    let shown: Vec<PlayerDisplay> = http
        .post(format!("{base}{}", routes::DISPLAY))
        .bearer_auth(GAME_TOKEN)
        .json(&DisplayRequest {
            players: vec![ana, bo, Uuid::new_v4()],
        })
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(shown.len(), 2);
    let bo_shown = shown.iter().find(|d| d.uuid == bo).unwrap();
    assert_eq!(bo_shown.rank.as_deref(), Some("vip"));
    assert_eq!(bo_shown.prefix, "&a[VIP] ");
    assert_eq!(shown.iter().find(|d| d.uuid == ana).unwrap().rank, None);

    // The public API, no token.
    let get = |path: String| http.get(format!("{base}{path}")).send();
    let res = get(public::leaderboard_query("duels", "kills", Window::All, 5))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["access-control-allow-origin"], "*");
    let kills: Leaderboard = res.json().await.unwrap();
    let names: Vec<_> = kills
        .entries
        .iter()
        .map(|e| (e.position, e.username.as_str(), e.value, e.prefix.as_str()))
        .collect();
    assert_eq!(names, [(1, "Ana", 3, ""), (2, "bo_rider", 1, "&a[VIP] ")]);

    let profile: PlayerProfile = get(public::profile("BO_RIDER"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(profile.uuid, bo);
    assert_eq!(profile.rank.as_deref(), Some("vip"));
    assert!(!profile.online);
    assert_eq!(profile.kinds["duels"]["games"], 1);
    assert_eq!(profile.kinds["duels"]["deaths"], 1);
    assert!(!profile.kinds["duels"].contains_key("wins"));
    assert_eq!(profile.recent_games.len(), 1);
    assert_eq!(profile.recent_games[0].players, 2);
    assert_eq!(profile.recent_games[0].placement, Some(2));
    assert_eq!(profile.recent_games[0].map.as_deref(), Some("arena"));
    let by_uuid = get(public::profile(&ana.to_string())).await.unwrap();
    assert_eq!(by_uuid.status(), StatusCode::OK);
    let nobody = get(public::profile("nobody")).await.unwrap();
    assert_eq!(nobody.status(), StatusCode::NOT_FOUND);

    let found: Vec<PlayerMatch> = get(format!("{}?q=b", public::SEARCH))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].username, "bo_rider");
    let kinds: Vec<BoardKind> = get(public::LEADERBOARDS.into())
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(kinds.len(), 1);
    assert_eq!(kinds[0].stats, ["deaths", "games", "kills", "wins"]);
    let network: NetworkSummary = get(public::NETWORK.into())
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!((network.players, network.games), (2, 1));
    let bad = get(format!(
        "{}?window=year",
        public::leaderboard("duels", "kills")
    ))
    .await
    .unwrap();
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    // Over the burst, the public API asks the client to slow down; the
    // tokened API doesn't count against it.
    let mut limited = None;
    for _ in 0..40 {
        let res = get(public::NETWORK.into()).await.unwrap();
        if res.status() == StatusCode::TOO_MANY_REQUESTS {
            limited = Some(res);
            break;
        }
    }
    let limited = limited.expect("never rate limited");
    let wait: u64 = limited.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=10).contains(&wait), "{wait}");
    let res = http
        .get(format!("{base}{}", routes::stats_of("Ana")))
        .bearer_auth(TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    pool.close().await;
    admin
        .execute(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE {db_name} WITH (FORCE)"
        )))
        .await
        .unwrap();
}
