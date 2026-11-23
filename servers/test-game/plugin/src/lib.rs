//! The dev network's test game, on `hallow-sdk`: a short team match on a
//! platform in the void, using every part of the SDK so the dev network
//! tests it end to end.
//!
//! | SDK part | Here |
//! |---|---|
//! | Arena | `maps/platform` (its `map.toml`: team spawns, a spectator point, out below y 90), copied and loaded fresh; the platform itself comes from [`Platform`], a chunk generator, so the repo carries no region files |
//! | Lifecycle | `players.min` from `hallow.toml`, a 5 s countdown, `TEST_GAME_MATCH_SECS` long, 3 s of ending, then `draining` and back to `test-game-lobby` |
//! | Teams | Red and Blue, balanced, no friendly fire, team chat (`!` to shout) |
//! | Kits | A wooden sword and a stack of the team's wool |
//! | Sidebar | The SDK's lines, plus how to forfeit |
//! | Spectating | Falling off, dying, or `/forfeit` puts a player out to spectate |
//! | Stats | `games_played` and `wins` from the SDK; `kills`, `deaths` and `forfeits` here |
//! | Control plane | The SDK reports `in_game` and `draining`; this game asks for nothing else |
//!
//! With `TEST_GAME_MATCH_SECS` unset the plugin does nothing, as before: the
//! static `game-1` of the compose networks runs that way, and the dynamic
//! instances get the variable from the control plane (`HALLOW_INSTANCE_ENV`
//! in `compose.instances.yaml`).

use hallow_sdk::{
    Cause, Kit, Match, Minigame, Settings, later, stats,
    teams::{NamedColor, Team},
};
use pumpkin_plugin_api::{
    Block, Context, Plugin, PluginMetadata, Server,
    command::{Command, CommandError, CommandSender, ConsumedArgs},
    commands::CommandHandler,
    common::GameMode,
    permission::{Permission, PermissionDefault},
    permissions, register_plugin,
    worldgen::{ChunkBuffer, ChunkGenerator, GeneratorManager},
};
use uuid::Uuid;

const NAME: &str = "hallow_test_game";
const MATCH_SECS_VAR: &str = "TEST_GAME_MATCH_SECS";
/// The platform's top layer, and how far it reaches from 0, 0 each way.
const PLATFORM_Y: i32 = 100;
const PLATFORM_RADIUS: i32 = 7;

/// The platform the map stands on: a stone square at y 100, nothing else.
struct Platform {
    stone: u16,
}

impl ChunkGenerator for Platform {
    fn generate_noise(&self, chunk: &mut ChunkBuffer) {
        let (base_x, base_z) = (chunk.x() * 16, chunk.z() * 16);
        for x in 0..16u8 {
            for z in 0..16u8 {
                let (wx, wz) = (base_x + i32::from(x), base_z + i32::from(z));
                if wx.abs() <= PLATFORM_RADIUS && wz.abs() <= PLATFORM_RADIUS {
                    chunk.set_block(x, PLATFORM_Y, z, self.stone);
                }
            }
        }
    }
}

struct TestGame;

impl Minigame for TestGame {
    fn started(&mut self, game: &mut Match) {
        game.broadcast("§7Last team on the platform wins. §f/forfeit§7 to give up.");
    }

    fn eliminated(&mut self, game: &mut Match, player: Uuid, cause: &Cause) {
        match cause {
            Cause::Died { killer } | Cause::Void { killer } => {
                game.add_stat(player, stats::DEATHS, 1);
                if let Some(killer) = killer {
                    game.add_stat(*killer, stats::KILLS, 1);
                }
            }
            Cause::Game(_) => game.add_stat(player, "forfeits", 1),
            Cause::Left => {}
        }
    }

    fn kit(&self, game: &Match, player: Uuid) -> Option<Kit> {
        let wool = match game.team(player).map(|team| team.id.as_str()) {
            Some("blue") => "minecraft:blue_wool",
            _ => "minecraft:red_wool",
        };
        Some(
            Kit::new()
                .named(0, "minecraft:wooden_sword", "§fPractice Sword")
                .item(1, wool, 64),
        )
    }

    fn sidebar(&self, game: &Match, player: Uuid) -> Vec<String> {
        if game.is_alive(player) {
            vec![String::new(), "§7/forfeit to give up".to_owned()]
        } else {
            Vec::new()
        }
    }
}

/// `/forfeit`: give up and spectate the rest of the match.
struct Forfeit;

impl CommandHandler for Forfeit {
    fn handle(
        &self,
        sender: CommandSender,
        _server: Server,
        _args: ConsumedArgs,
    ) -> Result<i32, CommandError> {
        let Some(player) = sender.as_player() else {
            return Err(CommandError::CommandFailed(
                pumpkin_plugin_api::text::TextComponent::text("Only players can forfeit."),
            ));
        };
        let id = player.get_id();
        let id = Uuid::from_u64_pair(id.high, id.low);
        later(move |game| {
            if game.is_alive(id) {
                game.eliminate(id, Cause::Game("forfeited".into()));
            } else if let Some(player) = game.player(id) {
                player.send_system_message(
                    pumpkin_plugin_api::text::TextComponent::from_legacy_string(
                        "§cYou can only forfeit while you're playing.",
                    ),
                    false,
                );
            }
        });
        Ok(1)
    }
}

fn match_secs() -> Option<u64> {
    std::env::var(MATCH_SECS_VAR)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
}

struct TestGamePlugin;

impl Plugin for TestGamePlugin {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        let mut permissions_list = hallow_sdk::permissions();
        permissions_list.push(format!("{}{MATCH_SECS_VAR}", permissions::SYS_ENV_PREFIX));
        PluginMetadata {
            name: NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Xena Studios".into()],
            description: "A short team match on hallow-sdk that ends and sends its players back to their lobby."
                .into(),
            dependencies: vec![],
            permissions: permissions_list,
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        let Some(secs) = match_secs() else {
            tracing::info!("test-game: {MATCH_SECS_VAR} isn't set; no matches");
            return Ok(());
        };
        let stone = Block::from_name("minecraft:stone")
            .ok_or("test-game: minecraft:stone isn't a block")?
            .default_state_id;
        let mut settings = Settings::new("§e§lTEST GAME");
        settings.countdown_secs = 5;
        settings.full_countdown_secs = 3;
        settings.length_secs = Some(secs);
        settings.ending_secs = 3;
        settings.gamemode = GameMode::Adventure;
        settings.generator = Some(GeneratorManager::register(Platform { stone }));
        settings.teams = vec![
            Team::new("red", "Red", NamedColor::Red),
            Team::new("blue", "Blue", NamedColor::Blue),
        ];
        hallow_sdk::start(&context, NAME, settings, TestGame)?;

        context.register_permission(&Permission {
            node: format!("{NAME}:forfeit"),
            description: "Give up the match and spectate.".into(),
            default: PermissionDefault::Allow,
            children: vec![],
        })?;
        context.register_command(
            Command::new(&["forfeit".to_owned()], "Give up the match and spectate.")
                .execute(Forfeit),
            "forfeit",
        );
        tracing::info!("test-game: matches last {secs} s");
        Ok(())
    }
}

register_plugin!(TestGamePlugin);
