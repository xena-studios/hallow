//! `hallow-sdk`: Hallow's minigame library, for a game's Pumpkin plugin in
//! its server repo.
//!
//! One match runs per server instance. The SDK runs it from start to end
//! and keeps the control plane told, so a game only writes its rules:
//!
//! | Part | Module | What the SDK does |
//! |---|---|---|
//! | Arenas | [`arena`] | Picks a map from the repo's `maps/`, copies it and loads the copy as a world; spawn points per team; spectator point; out below `void_y` |
//! | Match lifecycle | [`lifecycle`] | Waiting, countdown, in game, ending, then `draining` so the server plugin sends everyone to the lobby |
//! | Teams | [`teams`] | Balanced assignment with preferences, no friendly fire, team chat, coloured name tags |
//! | Kits | [`kit`] | Gives each player the game's kit when the match starts |
//! | Scoreboards | [`sidebar`] | A sidebar for every player: state, time, teams, plus the game's lines |
//! | Spectating | [`Match::eliminate`] | Players who die, fall out or are put out spectate at the map's spectator point; late joiners watch |
//! | Stats | [`stats`] | `games_played` and `wins` counted; games add their own |
//! | Control plane | [`network`] | Server state, stats and moving players, through Hallow's server plugin |
//!
//! # Example
//!
//! The smallest game: two teams, a kit, a five minute limit, a kill counter.
//!
//! ```rust,ignore
//! use hallow_sdk::{Cause, Kit, Match, Minigame, Settings, stats, teams::{NamedColor, Team}};
//! use pumpkin_plugin_api::{Context, Plugin, PluginMetadata, register_plugin};
//!
//! const NAME: &str = "duels";
//!
//! struct Duels;
//!
//! impl Minigame for Duels {
//!     fn kit(&self, _game: &Match, _player: uuid::Uuid) -> Option<Kit> {
//!         Some(Kit::new().item(0, "minecraft:iron_sword", 1).chestplate("minecraft:iron_chestplate"))
//!     }
//!
//!     fn eliminated(&mut self, game: &mut Match, _player: uuid::Uuid, cause: &Cause) {
//!         if let Cause::Died { killer: Some(killer) } = cause {
//!             game.add_stat(*killer, stats::KILLS, 1);
//!         }
//!     }
//! }
//!
//! struct Plugin_;
//!
//! impl Plugin for Plugin_ {
//!     fn new() -> Self { Self }
//!     fn metadata(&self) -> PluginMetadata {
//!         PluginMetadata {
//!             name: NAME.into(),
//!             version: env!("CARGO_PKG_VERSION").into(),
//!             authors: vec!["Xena Studios".into()],
//!             description: "Duels".into(),
//!             dependencies: vec![],
//!             permissions: hallow_sdk::permissions(),
//!         }
//!     }
//!     fn on_load(&self, context: Context) -> Result<(), String> {
//!         let mut settings = Settings::new("§c§lDUELS");
//!         settings.length_secs = Some(300);
//!         settings.teams = vec![
//!             Team::new("red", "Red", NamedColor::Red),
//!             Team::new("blue", "Blue", NamedColor::Blue),
//!         ];
//!         hallow_sdk::start(&context, NAME, settings, Duels)
//!     }
//! }
//!
//! register_plugin!(Plugin_);
//! ```
//!
//! `hallow.toml` lists the plugin with [`permissions`] pre-approved, and
//! `[maps] dir = "maps"` for its maps. `servers/test-game` in the Hallow
//! repo uses every part.
//!
//! # How it runs
//!
//! [`start`] subscribes to Pumpkin's events and runs a task every quarter
//! second. Event handlers only note what happened (a join, a death, a fall);
//! the task does the work, and calls the game's [`Minigame`] methods with
//! the [`Match`]. Pumpkin can call back into a plugin from a call the
//! plugin makes (setting a game mode fires an event), so the SDK never holds
//! a lock across a call into Pumpkin: while the task runs, the state is out
//! of its lock, and events fired back meanwhile pass through untouched.
//! Commands and other code outside the callbacks reach the match with
//! [`later`].
//!
//! The SDK targets Pumpkin's plugin API v0.1, like the rest of Hallow.

pub mod arena;
mod game;
pub mod kit;
pub mod lifecycle;
pub mod network;
pub mod sidebar;
pub mod stats;
pub mod teams;

pub use arena::{Arena, Map};
pub use game::{Cause, Match, Minigame, Outcome, Settings, Status, later, start};
pub use kit::Kit;
pub use lifecycle::Phase;
pub use teams::Team;

/// Pumpkin's plugin API, the version the SDK builds against, so a game
/// needs no dependency of its own (`hallow_sdk::pumpkin::register_plugin!`).
pub use pumpkin_plugin_api as pumpkin;
pub use uuid::Uuid;

use pumpkin_plugin_api::permissions as perm;

/// The permissions the SDK needs: the data folder (to read maps and copy
/// them for the arena) and the variables `hallow.toml` sets. Put them in
/// the game plugin's `PluginMetadata::permissions` and in `hallow.toml`'s
/// `[[plugins]] permissions`, where the base image pre-approves them.
#[must_use]
pub fn permissions() -> Vec<String> {
    [perm::FS_READ_DATA, perm::FS_WRITE_DATA]
        .into_iter()
        .map(str::to_owned)
        .chain(
            game::ENV_VARS
                .iter()
                .map(|var| format!("{}{var}", perm::SYS_ENV_PREFIX)),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn permissions() {
        assert_eq!(
            super::permissions(),
            [
                "fs.read.data",
                "fs.write.data",
                "sys.env.HALLOW_MIN_PLAYERS",
                "sys.env.HALLOW_MAX_PLAYERS",
                "sys.env.HALLOW_MAPS_PICK"
            ]
        );
    }
}
