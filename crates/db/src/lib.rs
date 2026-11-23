//! Hallow's PostgreSQL schema and the typed queries the control plane runs
//! against it: accounts, ranks, sessions, punishments and their templates,
//! anticheat signals, fleet settings, the fleet manager's nodes and audit log, node join tokens, player stats and
//! server kinds.
//!
//! Every table lives in the `hallow` schema, so Hallow and the Calagopus
//! panel can share one database. Queries are checked at runtime
//! (`sqlx::query_as`, not the compile-time macros), so building needs no
//! database. See the crate README for the schema.
//!
//! ```no_run
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let pool = hallow_db::connect("postgres://hallow@localhost/hallow").await?;
//! hallow_db::migrate(&pool).await?;
//! # Ok(()) }
//! ```

mod accounts;
mod fleet;
mod fleet_nodes;
mod friends;
mod kinds;
mod nodes;
mod punishments;
mod ranks;
mod sessions;
mod signals;
mod stats;

pub use accounts::{
    Account, Edition, Login, PastUsername, find_accounts_by_username, get_account, upsert_account,
    username_history,
};
pub use fleet::{
    FleetSettingsChange, StoredFleetSettings, fleet_settings, fleet_settings_history,
    save_fleet_settings,
};
pub use fleet_nodes::{
    FleetEvent, FleetNode, NewFleetEvent, NewFleetNode, add_fleet_events, add_fleet_node,
    approve_fleet_cancel, await_fleet_approval, fleet_events, fleet_node, fleet_nodes,
    put_floor_node, set_fleet_node_phase,
};
pub use friends::{
    Friend, MAX_FRIENDS, RequestOutcome, accept_friend, are_friends, deny_friend, friends_of,
    pending_requests, remove_friend, request_friend,
};
pub use hallow_proto::permission::{has_permission, matches_node};
pub use kinds::{ServerKind, delete_kind, list_kinds, put_kind};
pub use nodes::{
    FinishJoinError, JoinToken, NewJoinToken, NewNode, Node, add_join_token, claim_join_token,
    delete_join_tokens_before, finish_join, list_nodes, node_exists, release_join_token,
};
pub use punishments::{
    Actor, NewPunishment, NewTemplate, Punishment, PunishmentFilter, PunishmentKind,
    PunishmentTemplate, PunishmentWithTarget, Revocation, TemplateError, active_ban, active_mute,
    add_punishment, add_template, delete_template, list_templates, punishment_history,
    revoke_punishment, search_punishments, update_template,
};
pub use ranks::{
    AccountPermissions, Rank, RankGrant, RankWithMembers, account_permissions, active_ranks,
    delete_rank, grant_rank, list_ranks, rank_grants, revoke_rank, save_rank, upsert_rank,
};
pub use sessions::{
    NewSession, Session, close_proxy_sessions, close_session, open_session, set_session_server,
};

pub use signals::{NewSignal, Signal, add_signals, delete_signals_before, recent_signals};
pub use sqlx::{self, PgPool, migrate::MigrateError};
pub use stats::{StatIncrease, StatTotal, add_stats, stats_of};

/// The schema every Hallow table lives in.
pub const SCHEMA: &str = "hallow";

/// Connects a pool to `url` with sqlx's defaults.
pub async fn connect(url: &str) -> Result<PgPool, sqlx::Error> {
    PgPool::connect(url).await
}

/// The migrations in `crates/db/migrations`. sqlx's bookkeeping table is
/// `hallow._sqlx_migrations`, so nothing of Hallow's lands in `public`.
pub fn migrator() -> sqlx::migrate::Migrator {
    let mut migrator = sqlx::migrate!();
    migrator.create_schema(SCHEMA);
    migrator.dangerous_set_table_name("hallow._sqlx_migrations");
    migrator
}

/// Applies every pending migration.
pub async fn migrate(pool: &PgPool) -> Result<(), MigrateError> {
    migrator().run(pool).await
}

/// Error for a text column holding a value the enum doesn't know.
fn decode_error(column: &str, value: &str) -> sqlx::Error {
    sqlx::Error::Decode(format!("unknown {column} {value:?}").into())
}
