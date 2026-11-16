//! Tests against a real PostgreSQL.
//!
//! Set `HALLOW_TEST_DATABASE_URL` (such as
//! `postgres://postgres:postgres@127.0.0.1:5432/postgres`) to run them; the
//! role needs `CREATEDB`. Without it they pass without doing anything
//! (except in CI, where they fail). Each test creates its own database and
//! drops it at the end, so tests run in parallel; a failing test leaves its
//! `hallow_test_*` database behind.

use std::{net::IpAddr, str::FromStr};

use chrono::{Duration, Utc};
use hallow_db::{
    Actor, Edition, Login, NewPunishment, NewSession, PgPool, PunishmentKind, Rank,
    sqlx::{
        self, Executor,
        postgres::{PgConnectOptions, PgPoolOptions},
    },
};
use uuid::Uuid;

fn database_url() -> Option<String> {
    let url = std::env::var("HALLOW_TEST_DATABASE_URL").ok();
    if url.is_none() {
        // CI provides PostgreSQL; a skip there would hide a broken setup.
        assert!(
            std::env::var_os("CI").is_none(),
            "HALLOW_TEST_DATABASE_URL isn't set in CI"
        );
        eprintln!("HALLOW_TEST_DATABASE_URL isn't set; skipping");
    }
    url
}

/// A freshly migrated database of the test's own.
struct TestDb {
    admin: PgPool,
    name: String,
    pool: PgPool,
}

impl TestDb {
    async fn new() -> Option<Self> {
        let url = database_url()?;
        let options = PgConnectOptions::from_str(&url).unwrap();
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
            .unwrap();
        let name = format!("hallow_test_{}", Uuid::new_v4().simple());
        admin
            .execute(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
            .await
            .unwrap();
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(options.database(&name))
            .await
            .unwrap();
        hallow_db::migrate(&pool).await.unwrap();
        Some(Self { admin, name, pool })
    }

    async fn drop(self) {
        self.pool.close().await;
        self.admin
            .execute(sqlx::AssertSqlSafe(format!(
                "DROP DATABASE {} WITH (FORCE)",
                self.name
            )))
            .await
            .unwrap();
    }
}

fn ip(s: &str) -> Option<IpAddr> {
    Some(s.parse().unwrap())
}

async fn join(pool: &PgPool, uuid: Uuid, username: &str) {
    let login = Login {
        uuid,
        username,
        edition: Edition::Java,
        ip: None,
    };
    hallow_db::upsert_account(pool, &login).await.unwrap();
}

fn rank(name: &str, weight: i32, permissions: &[&str]) -> Rank {
    Rank {
        name: name.to_owned(),
        display_prefix: format!("&7[{name}] "),
        weight,
        permissions: permissions.iter().map(|&p| p.to_owned()).collect(),
        is_default: false,
    }
}

#[tokio::test]
async fn migrations_stay_in_the_hallow_schema() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    // Running again is a no-op.
    hallow_db::migrate(&db.pool).await.unwrap();

    let public: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'public'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(public, Vec::<String>::new());

    let mut tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name::text FROM information_schema.tables WHERE table_schema = 'hallow'",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    tables.sort();
    assert_eq!(
        tables,
        [
            "_sqlx_migrations",
            "account_ranks",
            "account_usernames",
            "accounts",
            "fleet_settings",
            "fleet_settings_history",
            "friend_requests",
            "friendships",
            "node_join_tokens",
            "nodes",
            "player_stats",
            "punishment_templates",
            "punishments",
            "ranks",
            "server_kinds",
            "sessions",
            "signal_samples"
        ]
    );
    db.drop().await;
}

#[tokio::test]
async fn fleet_settings_keep_a_history() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    assert_eq!(hallow_db::fleet_settings(&db.pool).await.unwrap(), None);

    let mut settings = hallow_proto::fleet::FleetSettings {
        max_nodes: 4,
        ..Default::default()
    };
    let first = hallow_db::save_fleet_settings(&db.pool, &settings, "alice")
        .await
        .unwrap();
    assert_eq!(first.settings.0, settings);
    assert_eq!(first.updated_by, "alice");
    settings.dry_run = false;
    hallow_db::save_fleet_settings(&db.pool, &settings, "bob")
        .await
        .unwrap();
    let stored = hallow_db::fleet_settings(&db.pool).await.unwrap().unwrap();
    assert!(!stored.settings.0.dry_run);
    assert_eq!(stored.updated_by, "bob");

    let history = hallow_db::fleet_settings_history(&db.pool, 10)
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].updated_by, "bob");
    assert!(history[0].before.as_ref().unwrap().0.dry_run);
    assert!(!history[0].after.0.dry_run);
    assert_eq!(history[1].updated_by, "alice");
    assert_eq!(history[1].before, None);
    db.drop().await;
}

#[tokio::test]
async fn login_creates_then_updates_the_account() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = &db.pool;
    let uuid = Uuid::new_v4();

    let first = hallow_db::upsert_account(
        pool,
        &Login {
            uuid,
            username: "Steve",
            edition: Edition::Java,
            ip: ip("203.0.113.7"),
        },
    )
    .await
    .unwrap();
    assert_eq!(first.username, "Steve");
    assert_eq!(first.edition, Edition::Java);
    assert_eq!(first.last_ip, ip("203.0.113.7"));

    // A rename; an unknown IP keeps the last one.
    let second = hallow_db::upsert_account(
        pool,
        &Login {
            uuid,
            username: "Steve2",
            edition: Edition::Java,
            ip: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(second.username, "Steve2");
    assert_eq!(second.first_seen, first.first_seen);
    assert!(second.last_seen >= first.last_seen);
    assert_eq!(second.last_ip, ip("203.0.113.7"));

    let names: Vec<String> = hallow_db::username_history(pool, uuid)
        .await
        .unwrap()
        .into_iter()
        .map(|n| n.username)
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"Steve".to_owned()) && names.contains(&"Steve2".to_owned()));

    // Lookup ignores case and only matches the current name.
    let found = hallow_db::find_accounts_by_username(pool, "sTEVE2")
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].uuid, uuid);
    assert!(
        hallow_db::find_accounts_by_username(pool, "steve")
            .await
            .unwrap()
            .is_empty()
    );

    assert_eq!(
        hallow_db::get_account(pool, uuid).await.unwrap(),
        Some(second)
    );
    assert_eq!(
        hallow_db::get_account(pool, Uuid::new_v4()).await.unwrap(),
        None
    );
    db.drop().await;
}

#[tokio::test]
async fn ranks_combine_their_nodes() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = &db.pool;
    let player = Uuid::new_v4();
    let admin = Uuid::new_v4();
    join(pool, player, "Mod").await;
    join(pool, admin, "Admin").await;

    // The seeded default rank, given nodes.
    let mut default = rank("default", 0, &["hallow.lobby.*"]);
    default.display_prefix = String::new();
    default.is_default = true;
    hallow_db::upsert_rank(pool, &default).await.unwrap();
    hallow_db::upsert_rank(
        pool,
        &rank("helper", 10, &["hallow.staff.find", "hallow.staff.msg"]),
    )
    .await
    .unwrap();
    hallow_db::upsert_rank(
        pool,
        &rank(
            "moderator",
            20,
            &["hallow.staff.*", "-hallow.staff.ban", "hallow.staff.find"],
        ),
    )
    .await
    .unwrap();
    hallow_db::upsert_rank(pool, &rank("vip", 5, &["hallow.cosmetics.*"]))
        .await
        .unwrap();

    // Without grants: just the default rank.
    let none = hallow_db::account_permissions(pool, player).await.unwrap();
    assert_eq!(none.primary().unwrap().name, "default");
    assert!(none.has("hallow.lobby.fly"));
    assert!(!none.has("hallow.staff.find"));

    hallow_db::grant_rank(pool, player, "helper", Some(admin), None)
        .await
        .unwrap();
    hallow_db::grant_rank(
        pool,
        player,
        "moderator",
        Some(admin),
        Some(Utc::now() + Duration::days(7)),
    )
    .await
    .unwrap();
    hallow_db::grant_rank(pool, player, "vip", None, None)
        .await
        .unwrap();
    // An expired grant: backdate it.
    sqlx::query(
        "UPDATE hallow.account_ranks SET granted_at = now() - interval '2 days',
         expires_at = now() - interval '1 day' WHERE rank_name = 'vip'",
    )
    .execute(pool)
    .await
    .unwrap();

    let perms = hallow_db::account_permissions(pool, player).await.unwrap();
    let names: Vec<&str> = perms.ranks.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["moderator", "helper", "default"]);
    assert_eq!(perms.primary().unwrap().display_prefix, "&7[moderator] ");
    assert_eq!(
        perms.nodes,
        [
            "hallow.staff.*",
            "-hallow.staff.ban",
            "hallow.staff.find",
            "hallow.staff.msg",
            "hallow.lobby.*"
        ]
    );
    assert!(perms.has("hallow.staff.kick"));
    assert!(!perms.has("hallow.staff.ban"));
    assert!(!perms.has("hallow.cosmetics.hat"));

    assert!(
        hallow_db::revoke_rank(pool, player, "moderator")
            .await
            .unwrap()
    );
    assert!(
        !hallow_db::revoke_rank(pool, player, "moderator")
            .await
            .unwrap()
    );
    let perms = hallow_db::account_permissions(pool, player).await.unwrap();
    assert!(perms.has("hallow.staff.find"));
    assert!(!perms.has("hallow.staff.kick"));

    // The database rejects malformed nodes and names, and a second default.
    for bad in [
        rank("bad", 0, &["Hallow.Staff"]),
        rank("bad", 0, &["hallow..staff"]),
        rank("bad", 0, &["hallow.*.find"]),
        rank("bad", 0, &["hallow staff"]),
        rank("Bad", 0, &[]),
        Rank {
            is_default: true,
            ..rank("other-default", 0, &[])
        },
    ] {
        assert!(
            hallow_db::upsert_rank(pool, &bad).await.is_err(),
            "{bad:?} was accepted"
        );
    }
    hallow_db::upsert_rank(pool, &rank("ok", 0, &["*", "-hallow.x.*", "a_b-c.d"]))
        .await
        .unwrap();
    db.drop().await;
}

#[tokio::test]
async fn bans_and_mutes() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = &db.pool;
    let target = Uuid::new_v4();
    let staff = Uuid::new_v4();
    join(pool, target, "Cheater").await;
    join(pool, staff, "Staff").await;

    assert_eq!(hallow_db::active_ban(pool, target).await.unwrap(), None);

    let week = Utc::now() + Duration::days(7);
    let temp = hallow_db::add_punishment(
        pool,
        &NewPunishment {
            kind: PunishmentKind::Ban,
            target,
            issuer: Actor::Anticheat,
            reason: "speed",
            expires_at: Some(week),
        },
    )
    .await
    .unwrap();
    assert_eq!(temp.issuer, Actor::Anticheat);
    assert!(temp.is_active_at(Utc::now()));
    assert_eq!(
        hallow_db::active_ban(pool, target)
            .await
            .unwrap()
            .unwrap()
            .id,
        temp.id
    );

    // A permanent ban wins over the temporary one.
    let perm = hallow_db::add_punishment(
        pool,
        &NewPunishment {
            kind: PunishmentKind::Ban,
            target,
            issuer: Actor::Account(staff),
            reason: "alt of a banned account",
            expires_at: None,
        },
    )
    .await
    .unwrap();
    let active = hallow_db::active_ban(pool, target).await.unwrap().unwrap();
    assert_eq!(active.id, perm.id);
    assert_eq!(active.issuer, Actor::Account(staff));

    // Revoking it leaves the temporary one.
    let revoked = hallow_db::revoke_punishment(pool, perm.id, &Actor::Console, Some("appeal"))
        .await
        .unwrap()
        .unwrap();
    let revocation = revoked.revoked.clone().unwrap();
    assert_eq!(revocation.by, Actor::Console);
    assert_eq!(revocation.reason.as_deref(), Some("appeal"));
    assert!(!revoked.is_active_at(Utc::now()));
    assert_eq!(
        hallow_db::revoke_punishment(pool, perm.id, &Actor::Console, None)
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        hallow_db::active_ban(pool, target)
            .await
            .unwrap()
            .unwrap()
            .id,
        temp.id
    );

    hallow_db::revoke_punishment(pool, temp.id, &Actor::Account(staff), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hallow_db::active_ban(pool, target).await.unwrap(), None);

    // An expired ban isn't active.
    sqlx::query(
        "INSERT INTO hallow.punishments (kind, target_uuid, issuer_kind, reason, created_at, expires_at)
         VALUES ('ban', $1, 'system', 'old', now() - interval '2 days', now() - interval '1 day')",
    )
    .bind(target)
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(hallow_db::active_ban(pool, target).await.unwrap(), None);

    // Mutes are separate from bans.
    assert_eq!(hallow_db::active_mute(pool, target).await.unwrap(), None);
    let mute = hallow_db::add_punishment(
        pool,
        &NewPunishment {
            kind: PunishmentKind::Mute,
            target,
            issuer: Actor::Account(staff),
            reason: "spam",
            expires_at: Some(Utc::now() + Duration::hours(1)),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        hallow_db::active_mute(pool, target)
            .await
            .unwrap()
            .unwrap()
            .id,
        mute.id
    );
    assert_eq!(hallow_db::active_ban(pool, target).await.unwrap(), None);

    let kick = hallow_db::add_punishment(
        pool,
        &NewPunishment {
            kind: PunishmentKind::Kick,
            target,
            issuer: Actor::System,
            reason: "restart",
            expires_at: None,
        },
    )
    .await
    .unwrap();

    // History: everything, newest first.
    let history = hallow_db::punishment_history(pool, target, 50)
        .await
        .unwrap();
    assert_eq!(history.len(), 5);
    assert_eq!(history[0].id, kick.id);
    assert_eq!(history[1].id, mute.id);
    assert_eq!(history[4].reason, "old");
    assert_eq!(
        hallow_db::punishment_history(pool, target, 2)
            .await
            .unwrap()
            .len(),
        2
    );

    // Rejected: a kick with an expiry, an expiry in the past, an unknown
    // target or issuer.
    for bad in [
        NewPunishment {
            kind: PunishmentKind::Kick,
            target,
            issuer: Actor::System,
            reason: "x",
            expires_at: Some(week),
        },
        NewPunishment {
            kind: PunishmentKind::Ban,
            target,
            issuer: Actor::System,
            reason: "x",
            expires_at: Some(Utc::now() - Duration::hours(1)),
        },
        NewPunishment {
            kind: PunishmentKind::Ban,
            target: Uuid::new_v4(),
            issuer: Actor::System,
            reason: "x",
            expires_at: None,
        },
        NewPunishment {
            kind: PunishmentKind::Warn,
            target,
            issuer: Actor::Account(Uuid::new_v4()),
            reason: "x",
            expires_at: None,
        },
    ] {
        assert!(
            hallow_db::add_punishment(pool, &bad).await.is_err(),
            "{bad:?} was accepted"
        );
    }

    // History is kept: a punished account can't be deleted.
    assert!(
        sqlx::query("DELETE FROM hallow.accounts WHERE uuid = $1")
            .bind(target)
            .execute(pool)
            .await
            .is_err()
    );
    db.drop().await;
}

#[tokio::test]
async fn sessions_open_and_close() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let pool = &db.pool;
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    join(pool, a, "A").await;
    join(pool, b, "B").await;

    let session = hallow_db::open_session(
        pool,
        &NewSession {
            account: a,
            proxy: "vine-1",
            protocol_version: Some(775),
            ip: ip("2001:db8::1"),
        },
    )
    .await
    .unwrap();
    assert_eq!(session.logout_at, None);
    assert_eq!(session.ip, ip("2001:db8::1"));

    assert!(
        hallow_db::set_session_server(pool, session.id, "hub-1")
            .await
            .unwrap()
    );
    let closed = hallow_db::close_session(pool, session.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(closed.last_server.as_deref(), Some("hub-1"));
    assert!(closed.logout_at.unwrap() >= closed.login_at);
    assert_eq!(
        hallow_db::close_session(pool, session.id).await.unwrap(),
        None
    );
    assert!(
        !hallow_db::set_session_server(pool, session.id, "hub-2")
            .await
            .unwrap()
    );

    // A proxy going away closes its open sessions only.
    for (account, proxy) in [(a, "vine-1"), (b, "vine-1"), (b, "vine-2")] {
        hallow_db::open_session(
            pool,
            &NewSession {
                account,
                proxy,
                protocol_version: None,
                ip: None,
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(
        hallow_db::close_proxy_sessions(pool, "vine-1")
            .await
            .unwrap(),
        2
    );
    let open: i64 =
        sqlx::query_scalar("SELECT count(*) FROM hallow.sessions WHERE logout_at IS NULL")
            .fetch_one(pool)
            .await
            .unwrap();
    assert_eq!(open, 1);
    db.drop().await;
}

/// The login-path lookups use their indexes (with sequential scans off, as
/// the test tables are too small for the planner to prefer an index).
#[tokio::test]
async fn lookups_use_their_indexes() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut conn = db.pool.acquire().await.unwrap();
    conn.execute("SET enable_seqscan = off").await.unwrap();
    for (query, index) in [
        (
            "SELECT * FROM hallow.punishments WHERE target_uuid = '00000000-0000-0000-0000-000000000001' AND kind = 'ban'
             AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now())",
            "punishments_active_idx",
        ),
        (
            "SELECT * FROM hallow.punishments WHERE target_uuid = '00000000-0000-0000-0000-000000000001' AND kind = 'mute'
             AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now())",
            "punishments_active_idx",
        ),
        (
            "SELECT * FROM hallow.punishments WHERE target_uuid = '00000000-0000-0000-0000-000000000001'
             ORDER BY created_at DESC LIMIT 50",
            "punishments_target_history_idx",
        ),
        (
            "SELECT * FROM hallow.accounts WHERE lower(username) = lower('Steve')",
            "accounts_username_lower_idx",
        ),
        (
            "SELECT * FROM hallow.account_usernames WHERE lower(username) = lower('Steve')",
            "account_usernames_username_lower_idx",
        ),
    ] {
        let plan: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("EXPLAIN {query}")))
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        let plan = plan.join("\n");
        assert!(plan.contains(index), "{query}\n{plan}");
    }
    drop(conn);
    db.drop().await;
}

#[tokio::test]
async fn friends() {
    use hallow_db::RequestOutcome;
    let Some(db) = TestDb::new().await else {
        return;
    };
    let [a, b, c] = [(); 3].map(|()| Uuid::new_v4());
    join(&db.pool, a, "alice").await;
    join(&db.pool, b, "bob").await;
    join(&db.pool, c, "carol").await;

    assert_eq!(
        hallow_db::request_friend(&db.pool, a, b).await.unwrap(),
        RequestOutcome::Sent
    );
    assert_eq!(
        hallow_db::request_friend(&db.pool, a, b).await.unwrap(),
        RequestOutcome::AlreadySent
    );
    // Asking back accepts.
    assert_eq!(
        hallow_db::request_friend(&db.pool, b, a).await.unwrap(),
        RequestOutcome::Accepted
    );
    assert_eq!(
        hallow_db::request_friend(&db.pool, a, b).await.unwrap(),
        RequestOutcome::AlreadyFriends
    );
    let friends = hallow_db::friends_of(&db.pool, a).await.unwrap();
    assert_eq!(
        friends
            .iter()
            .map(|f| f.username.as_str())
            .collect::<Vec<_>>(),
        ["bob"]
    );
    assert_eq!(hallow_db::friends_of(&db.pool, b).await.unwrap()[0].uuid, a);

    hallow_db::request_friend(&db.pool, c, a).await.unwrap();
    let pending = hallow_db::pending_requests(&db.pool, a).await.unwrap();
    assert_eq!(pending.iter().map(|f| f.uuid).collect::<Vec<_>>(), [c]);
    assert!(hallow_db::deny_friend(&db.pool, a, c).await.unwrap());
    assert!(
        !hallow_db::accept_friend(&db.pool, a, c).await.unwrap(),
        "denied requests are gone"
    );

    assert!(hallow_db::remove_friend(&db.pool, b, a).await.unwrap());
    assert!(!hallow_db::are_friends(&db.pool, a, b).await.unwrap());
    db.drop().await;
}

#[tokio::test]
async fn signals() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (player, stranger) = (Uuid::new_v4(), Uuid::new_v4());
    join(&db.pool, player, "clicker").await;
    let metrics = serde_json::json!({"attacks": 42.0, "cps_max": 15.0});
    let now = Utc::now();
    let sample = |account, at| hallow_db::NewSignal {
        account,
        source: "proxy-1",
        at,
        window_ms: 30_000,
        protocol: Some(777),
        metrics: &metrics,
    };
    // A sample for an account that never joined is skipped.
    let stored = hallow_db::add_signals(
        &db.pool,
        &[
            sample(player, now - Duration::days(20)),
            sample(player, now),
            sample(stranger, now),
        ],
    )
    .await
    .unwrap();
    assert_eq!(stored, 2);
    let recent = hallow_db::recent_signals(&db.pool, player, 10)
        .await
        .unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].metrics.0["cps_max"], 15.0);
    assert!(recent[0].at > recent[1].at, "newest first");

    let deleted = hallow_db::delete_signals_before(&db.pool, now - Duration::days(14))
        .await
        .unwrap();
    assert_eq!(deleted, 1);
    db.drop().await;
}

fn new_node<'a>(
    name: &'a str,
    labels: &'a std::collections::BTreeMap<String, String>,
    key: Option<&'a str>,
) -> hallow_db::NewNode<'a> {
    hallow_db::NewNode {
        name,
        panel_uuid: Uuid::new_v4(),
        role: "burst",
        region: "fsn1",
        labels,
        private_ip: "10.42.1.7".parse().unwrap(),
        public_ip: ip("203.0.113.9"),
        wireguard_public_key: key,
    }
}

/// A join token is claimed by one join at a time, used once, and can be
/// retried after a failed join or a lapsed claim.
#[tokio::test]
async fn join_tokens_are_single_use() {
    use hallow_db::{FinishJoinError, NewJoinToken};
    let Some(db) = TestDb::new().await else {
        return;
    };
    let now = Utc::now();
    let lease = Duration::seconds(60);
    let labels = [("pool".to_owned(), "games".to_owned())].into();
    let hash = [7u8; 32];
    let token = hallow_db::add_join_token(
        &db.pool,
        &NewJoinToken {
            id: Uuid::new_v4(),
            token_hash: &hash,
            role: "burst",
            region: "fsn1",
            name: Some("burst-1"),
            labels: &labels,
            expires_at: now + Duration::hours(1),
        },
    )
    .await
    .unwrap();
    assert_eq!(token.labels.0, labels);

    // Unknown hashes claim nothing.
    assert!(
        hallow_db::claim_join_token(&db.pool, &[8u8; 32], now, lease)
            .await
            .unwrap()
            .is_none()
    );
    // Two joins race: one wins.
    let (a, b) = tokio::join!(
        hallow_db::claim_join_token(&db.pool, &hash, now, lease),
        hallow_db::claim_join_token(&db.pool, &hash, now, lease),
    );
    assert_eq!(
        [a.unwrap().is_some(), b.unwrap().is_some()]
            .iter()
            .filter(|&&won| won)
            .count(),
        1
    );
    // A failed join gives it back.
    hallow_db::release_join_token(&db.pool, token.id)
        .await
        .unwrap();
    let claimed = hallow_db::claim_join_token(&db.pool, &hash, now, lease)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.id, token.id);
    // A crashed join's claim lapses.
    let later = now + Duration::seconds(61);
    assert!(
        hallow_db::claim_join_token(&db.pool, &hash, now + Duration::seconds(30), lease)
            .await
            .unwrap()
            .is_none()
    );
    hallow_db::claim_join_token(&db.pool, &hash, later, lease)
        .await
        .unwrap()
        .unwrap();
    // A join that finishes with a lapsed claim records nothing.
    let node = new_node("burst-1", &labels, Some("key-1"));
    assert!(matches!(
        hallow_db::finish_join(&db.pool, token.id, &node, later + Duration::seconds(120)).await,
        Err(FinishJoinError::LostClaim)
    ));
    assert!(hallow_db::list_nodes(&db.pool).await.unwrap().is_empty());
    // Claim again and finish: the token is used up.
    let at = later + Duration::seconds(121);
    hallow_db::claim_join_token(&db.pool, &hash, at, lease)
        .await
        .unwrap()
        .unwrap();
    let joined = hallow_db::finish_join(&db.pool, token.id, &node, at)
        .await
        .unwrap();
    assert_eq!(joined.name, "burst-1");
    assert_eq!(joined.join_token_id, Some(token.id));
    assert!(hallow_db::node_exists(&db.pool, "burst-1").await.unwrap());
    assert!(
        hallow_db::claim_join_token(&db.pool, &hash, at + Duration::hours(2), lease)
            .await
            .unwrap()
            .is_none(),
        "a used token can't be claimed again"
    );
    hallow_db::release_join_token(&db.pool, token.id)
        .await
        .unwrap();
    assert!(
        hallow_db::claim_join_token(&db.pool, &hash, at, lease)
            .await
            .unwrap()
            .is_none(),
        "releasing doesn't revive a used token"
    );
    let nodes = hallow_db::list_nodes(&db.pool).await.unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].private_ip.to_string(), "10.42.1.7");
    db.drop().await;
}

/// Expired tokens can't be claimed and are deleted by age; the nodes that
/// used them stay. A second node with the same key or name is refused.
#[tokio::test]
async fn join_tokens_expire() {
    use hallow_db::{FinishJoinError, NewJoinToken};
    let Some(db) = TestDb::new().await else {
        return;
    };
    let now = Utc::now();
    let lease = Duration::seconds(60);
    let labels = std::collections::BTreeMap::new();
    let mut ids = Vec::new();
    for (i, expires) in [Duration::hours(-1), Duration::hours(1), Duration::hours(1)]
        .into_iter()
        .enumerate()
    {
        let hash = [i as u8; 32];
        let token = hallow_db::add_join_token(
            &db.pool,
            &NewJoinToken {
                id: Uuid::new_v4(),
                token_hash: &hash,
                role: "base",
                region: "nbg1",
                name: None,
                labels: &labels,
                expires_at: now + expires,
            },
        )
        .await
        .unwrap();
        ids.push(token.id);
    }
    assert!(
        hallow_db::claim_join_token(&db.pool, &[0u8; 32], now, lease)
            .await
            .unwrap()
            .is_none(),
        "expired"
    );
    hallow_db::claim_join_token(&db.pool, &[1u8; 32], now, lease)
        .await
        .unwrap()
        .unwrap();
    hallow_db::finish_join(
        &db.pool,
        ids[1],
        &new_node("base-1", &labels, Some("k")),
        now,
    )
    .await
    .unwrap();
    hallow_db::claim_join_token(&db.pool, &[2u8; 32], now, lease)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        hallow_db::finish_join(
            &db.pool,
            ids[2],
            &new_node("base-2", &labels, Some("k")),
            now
        )
        .await,
        Err(FinishJoinError::Duplicate)
    ));
    // The duplicate rolled back: the token wasn't used.
    hallow_db::release_join_token(&db.pool, ids[2])
        .await
        .unwrap();
    assert!(
        hallow_db::claim_join_token(&db.pool, &[2u8; 32], now, lease)
            .await
            .unwrap()
            .is_some()
    );

    let deleted = hallow_db::delete_join_tokens_before(&db.pool, now + Duration::hours(2))
        .await
        .unwrap();
    assert_eq!(deleted, 3);
    let nodes = hallow_db::list_nodes(&db.pool).await.unwrap();
    assert_eq!(nodes.len(), 1);
    assert_eq!(
        nodes[0].join_token_id, None,
        "the token went, the node stays"
    );
    db.drop().await;
}

#[tokio::test]
async fn stats_add_up() {
    let Some(db) = TestDb::new().await else {
        return;
    };
    let (player, stranger) = (Uuid::new_v4(), Uuid::new_v4());
    join(&db.pool, player, "fighter").await;
    let inc = |account, kind, stat, by| hallow_db::StatIncrease {
        account,
        kind,
        stat,
        by,
    };
    // Two increases to one total in the same call are summed; a stranger's
    // are skipped.
    let changed = hallow_db::add_stats(
        &db.pool,
        &[
            inc(player, "skywars", "kills", 2),
            inc(player, "skywars", "kills", 3),
            inc(player, "hub", "play_time_s", 60),
            inc(stranger, "skywars", "kills", 9),
        ],
    )
    .await
    .unwrap();
    assert_eq!(changed, 2);
    hallow_db::add_stats(&db.pool, &[inc(player, "skywars", "kills", 1)])
        .await
        .unwrap();
    let totals = hallow_db::stats_of(&db.pool, player).await.unwrap();
    let totals: Vec<_> = totals
        .iter()
        .map(|t| (t.kind.as_str(), t.stat.as_str(), t.value))
        .collect();
    assert_eq!(
        totals,
        [("hub", "play_time_s", 60), ("skywars", "kills", 6)]
    );
    assert!(
        hallow_db::stats_of(&db.pool, stranger)
            .await
            .unwrap()
            .is_empty()
    );
    db.drop().await;
}

/// Kinds are upserted, listed by name and deleted.
#[tokio::test]
async fn server_kinds() {
    use hallow_proto::instance::{KindPlayers, KindRole, KindScaling, KindSpec};
    let Some(db) = TestDb::new().await else {
        return;
    };
    let mut spec = KindSpec {
        image: "hallow-test-game:dev".into(),
        role: KindRole::Game,
        lobby: Some("hub".into()),
        players: KindPlayers { min: 2, max: 16 },
        scaling: KindScaling {
            warm: 1,
            max_instances: Some(20),
        },
        env: [("MODE".to_owned(), "solo".to_owned())].into(),
        memory_mb: None,
        updated_by: Some("alice".into()),
    };
    let first = hallow_db::put_kind(&db.pool, "test-game", &spec)
        .await
        .unwrap();
    assert_eq!(first.spec.0, spec);
    assert_eq!(first.updated_by.as_deref(), Some("alice"));
    spec.image = "hallow-test-game:v2".into();
    spec.updated_by = None;
    hallow_db::put_kind(&db.pool, "test-game", &spec)
        .await
        .unwrap();
    hallow_db::put_kind(&db.pool, "a-game", &spec)
        .await
        .unwrap();
    // Names follow hallow.toml's rule.
    assert!(
        hallow_db::put_kind(&db.pool, "Bad_Name", &spec)
            .await
            .is_err()
    );
    let kinds = hallow_db::list_kinds(&db.pool).await.unwrap();
    let names: Vec<&str> = kinds.iter().map(|k| k.name.as_str()).collect();
    assert_eq!(names, ["a-game", "test-game"]);
    assert_eq!(kinds[1].spec.0.image, "hallow-test-game:v2");
    assert_eq!(kinds[1].updated_by, None);
    assert!(hallow_db::delete_kind(&db.pool, "a-game").await.unwrap());
    assert!(!hallow_db::delete_kind(&db.pool, "a-game").await.unwrap());
    db.drop().await;
}
