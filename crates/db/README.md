# hallow-db

Hallow's PostgreSQL schema and the typed queries the control plane runs against it: accounts, ranks, sessions, punishments, anticheat signals and player stats. Redis holds live state; PostgreSQL holds everything kept ([Decisions](../../docs/DECISIONS.md) 13).

- Migrations are in [`migrations/`](migrations), embedded with `sqlx::migrate!` and applied by `hallow_db::migrate`.
- Queries are checked at runtime (`sqlx::query_as`), not by sqlx's compile-time macros, so building needs no database.

```rust
let pool = hallow_db::connect(&database_url).await?;
hallow_db::migrate(&pool).await?;

// At login:
let account = hallow_db::upsert_account(&pool, &login).await?;
if let Some(ban) = hallow_db::active_ban(&pool, account.uuid).await? { /* refuse */ }
let perms = hallow_db::account_permissions(&pool, account.uuid).await?;
let session = hallow_db::open_session(&pool, &new_session).await?;
```

## The `hallow` schema

Every table, and sqlx's bookkeeping table (`hallow._sqlx_migrations`), lives in the `hallow` schema. The Hallow Panel is a Calagopus fork whose own tables and `migrations` table are in `public` ([Panel](../../docs/PANEL.md)), so both can share one database without colliding. The panel reads and writes Hallow's tables by their qualified names (`hallow.accounts`).

Enumerations (`edition`, punishment `kind`, actor kinds) are `text` columns with CHECK constraints, not Postgres enum types. Adding a value is then a one-line migration, and the panel's TypeScript side sees plain strings.

## Tables

| Table | Holds | Key |
|---|---|---|
| `accounts` | Every player who has joined: UUID, current username, edition (`java` or `bedrock`), first and last seen, last IP | `uuid` |
| `account_usernames` | Every username an account has used, the current one included, with when it was first and last used | `(account_uuid, username)` |
| `ranks` | Name, display prefix (`&` colour codes), weight (higher wins), permission nodes, and whether it is the default rank every account holds | `name` |
| `account_ranks` | Ranks an account holds, who granted them, and an optional expiry | `(account_uuid, rank_name)` |
| `sessions` | One row per connection: account, proxy, last server, protocol version (`NULL` for Bedrock), IP, login and logout times (`NULL` while open) | `id` |
| `friend_requests` | A request from one account to another, until it's accepted or denied; asking back accepts | `(from_uuid, to_uuid)` |
| `friendships` | One row per friendship, the smaller UUID first; at most 200 friends each | `(a_uuid, b_uuid)` |
| `signal_samples` | Anticheat signals: one row per player per measured window (30 s), with the source, protocol and free-form JSON metrics; deleted after the retention period | `id` |
| `fleet_settings` | The fleet manager's settings in force (`hallow_proto::fleet::FleetSettings` as JSON), who changed them last and when; one row, absent until the defaults are first changed | `id` (always 1) |
| `fleet_settings_history` | Every change to `fleet_settings`, append-only, with the settings before (`NULL` for the first change) and after | `id` |
| `node_join_tokens` | One-time node join tokens: SHA-256 of the token, role, region, optional node name, labels, expiry, a short claim lease while a join runs, and when it was used; deleted a week after expiry | `id` (hash unique) |
| `nodes` | Every node that joined: name, panel UUID, role, region, labels, private and public IP, WireGuard public key, the token it used | `name` |
| `player_stats` | Running totals per account, server kind and stat name (`play_time_s`, `joins`, `kills`, ...). `add_stats` adds a batch of increases in one upsert (`value = value + n`); increases for accounts that never joined are skipped | `(account_uuid, kind, stat)` |
| `server_kinds` | Server kinds the control plane starts instances of: name, the kind's spec (`hallow_proto::instance::KindSpec` as JSON: live image, role, players, scaling, env), who changed it last and when. Instances are live state in Redis, never rows | `name` |
| `punishments` | Bans, mutes, kicks and warnings: target, issuer, reason, created, expiry (`NULL` is permanent), and revocation (when, by whom, why) | `id` |

A punishment's issuer and revoker are an actor: a staff `account` (with its UUID), the `console`, `system` (automation) or `anticheat` (the verdict service, see [Anticheat](../../docs/ANTICHEAT.md)). Kicks never expire.

### Indexes for the control plane's queries

| Query | Index |
|---|---|
| Is this account banned now? Its active mute? | `punishments_active_idx`: `(target_uuid, kind, expires_at)` over unrevoked bans and mutes. Expiry is checked at query time, since `now()` can't go in an index predicate |
| A player's punishment history | `punishments_target_history_idx`: `(target_uuid, created_at DESC)` |
| An account by username, ignoring case | `accounts_username_lower_idx` on `lower(username)`; `account_usernames_username_lower_idx` for past names |
| An account's ranks | the `account_ranks` primary key, which starts with `account_uuid` |
| A player's sessions; accounts seen on an IP; open sessions on a proxy | `sessions_account_idx`, `sessions_ip_idx`, `sessions_open_by_proxy_idx` |
| A player's stats | the `player_stats` primary key, which starts with `account_uuid` |
| Leaderboards (top values of one stat in one kind) | `player_stats_kind_stat_value_idx`: `(kind, stat, value DESC)` |

A test checks that the planner uses the first group of indexes for those queries.

Usernames aren't unique. If a player renames and someone else takes the old name before the first player returns, both accounts carry it until the first player logs in again; `find_accounts_by_username` returns the most recently seen first.

## Permission nodes

Rank nodes use Vine's format and matching rules (`upstream/Vine/src/permissions.rs`), so a node means the same to Vine and to the control plane:

- dot-separated segments: `hallow.staff.find`;
- `*` grants everything; a trailing `.*` grants everything below a prefix (`hallow.staff.*` grants `hallow.staff.find`, not `hallow.staff`);
- a leading `-` denies (`-hallow.staff.ban`), and a denial in any held rank beats a grant in any other.

Vine compares nodes ignoring case. The database stores them lowercase and rejects anything else (`hallow.permission_nodes_valid`). Ranks don't inherit from each other: an account's nodes are the union of its active ranks plus the default rank. `AccountPermissions::has` and `hallow_db::has_permission` evaluate them.

## Privacy

`accounts.last_ip` and `sessions.ip` are personal data. They are kept for ban evasion and alt detection (the panel's player search and alts view), and should only be shown to staff with a permission for it. Delete or anonymise them on a player's erasure request and after a retention period the network sets. A retention job isn't built yet. An account that received, issued or revoked punishments can't be deleted (`ON DELETE RESTRICT`) until those rows are dealt with, so erasure is a deliberate step.

## Tests

The tests in `tests/postgres.rs` run against a real PostgreSQL:

```sh
docker run -d --name hallow-pg -e POSTGRES_PASSWORD=postgres -p 127.0.0.1:55440:5432 postgres:18-alpine
HALLOW_TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55440/postgres cargo test -p hallow-db
```

Each test creates its own database (the role needs `CREATEDB`), migrates it and drops it, so tests run in parallel. Without `HALLOW_TEST_DATABASE_URL` they skip, except in CI, where they fail.
