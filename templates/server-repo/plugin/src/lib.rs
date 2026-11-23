//! This server kind's game, on `hallow-sdk`. The SDK runs the match
//! (waiting, countdown, in game, ending, back to the lobby), teams, kits,
//! the sidebar, spectating and stats; this file holds the rules that make
//! the game this game. See hallow-sdk's README for every hook.

use hallow_sdk::{
    Cause, Kit, Match, Minigame, Settings, Uuid,
    pumpkin::{Context, Plugin, PluginMetadata, register_plugin},
    stats,
    teams::{NamedColor, Team},
};

/// The plugin's name in Pumpkin. Its data folder, and where the base image
/// copies maps/, is plugins/data/<NAME>.
const NAME: &str = "example";

struct Example;

impl Minigame for Example {
    fn kit(&self, _game: &Match, _player: Uuid) -> Option<Kit> {
        Some(Kit::new().item(0, "minecraft:stone_sword", 1))
    }

    fn eliminated(&mut self, game: &mut Match, player: Uuid, cause: &Cause) {
        if let Cause::Died { killer } = cause {
            game.add_stat(player, stats::DEATHS, 1);
            if let Some(killer) = killer {
                game.add_stat(*killer, stats::KILLS, 1);
            }
        }
    }
}

struct ExamplePlugin;

impl Plugin for ExamplePlugin {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec![],
            description: "An example game on hallow-sdk.".into(),
            dependencies: vec![],
            // Also listed in hallow.toml, where the base image pre-approves them.
            permissions: hallow_sdk::permissions(),
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        let mut settings = Settings::new("§e§lEXAMPLE");
        settings.length_secs = Some(10 * 60);
        settings.teams = vec![
            Team::new("red", "Red", NamedColor::Red),
            Team::new("blue", "Blue", NamedColor::Blue),
        ];
        hallow_sdk::start(&context, NAME, settings, Example)
    }
}

register_plugin!(ExamplePlugin);
