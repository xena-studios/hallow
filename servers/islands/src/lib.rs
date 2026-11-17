//! Sky-island maps and the match plumbing Hallow's island games share
//! (EggWars and SkyWars, in `servers/eggwars` and `servers/skywars`).
//!
//! - [`map`]: the map format. Maps are TOML files in a game's `maps/`
//!   folder, drawn for one team and copied by symmetry.
//! - [`build`]: turns a map into blocks, chunk by chunk (pure, tested).
//! - [`generator`]: the Pumpkin chunk generator that builds a map into the
//!   match's fresh `arena` world.
//! - [`play`]: server state and stats over IPC, picking maps, team colours,
//!   the sidebar and the dev network's test hooks.
//! - [`roster`]: teams, lives, kill credit and the match phases (pure).
//! - [`combat`]: the seam for a future 1.8-style combat module.
//!
//! [`play`] and parts of [`map`] are what `hallow-sdk` (#27) is meant to
//! provide; the games move onto it when it lands.

pub mod build;
pub mod combat;
pub mod generator;
pub mod map;
pub mod play;
pub mod roster;
