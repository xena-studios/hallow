//! Hallow's PostgreSQL schema and the typed queries the control plane runs
//! against it: accounts, ranks, sessions and punishments.
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
mod permissions;
mod punishments;
mod ranks;
mod sessions;

pub use accounts::{
    Account, Edition, Login, PastUsername, find_accounts_by_username, get_account, upsert_account,
    username_history,
};
pub use permissions::{has_permission, matches_node};
pub use punishments::{
    Actor, NewPunishment, Punishment, PunishmentKind, Revocation, active_ban, active_mute,
    add_punishment, punishment_history, revoke_punishment,
};
pub use ranks::{
    AccountPermissions, Rank, account_permissions, active_ranks, grant_rank, revoke_rank,
    upsert_rank,
};
pub use sessions::{
    NewSession, Session, close_proxy_sessions, close_session, open_session, set_session_server,
};

pub use sqlx::{self, PgPool, migrate::MigrateError};

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
