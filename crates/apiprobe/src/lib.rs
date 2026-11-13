//! Checks the Pumpkin plugin API calls every Hallow minigame relies on.
//!
//! When the first player joins, the probe runs in stages, a second apart, the
//! way a game would: create a world and send the player there, then build in
//! it once its chunks are loaded, then show a scoreboard and give an item,
//! then unload the world, load it again from disk and check the build
//! survived. Every check logs a line starting with `apiprobe:` and `ok` or
//! `FAIL`, and the run ends with a summary line.
//!
//! Things the probe works around, so they're easy to find:
//! - Pumpkin kicks a client that confirms a teleport other than the latest
//!   one, so the probe waits a second after the join before teleporting.
//! - Pumpkin only writes blocks into loaded chunks; a write elsewhere is
//!   silently dropped. The probe builds after the player has arrived.
//! - The v0.1 scoreboard has no `get-score`; adding 0 returns the value.

use std::sync::atomic::{AtomicBool, Ordering};

use pumpkin_plugin_api::{
    Block, BlockTypeExt, Context, Dimension, ItemStack, Plugin, PluginMetadata, Server, World,
    common::BlockPos,
    events::{EventHandler, EventPriority, PlayerJoinEvent},
    events_wit::PlayerJoinEventData,
    register_plugin,
    scheduler::schedule_delayed_task,
    scoreboard::{DisplaySlot, RenderType},
    text::TextComponent,
    world::BlockFlags,
};

const ARENA: &str = "apiprobe_arena";
const PLATFORM_Y: i32 = 100;
const OBJECTIVE: &str = "apiprobe";
const STAGE_TICKS: u64 = 20;

static RAN: AtomicBool = AtomicBool::new(false);
static FAILED: AtomicBool = AtomicBool::new(false);

fn report(step: &str, result: Result<String, String>) {
    match result {
        Ok(detail) => tracing::info!("apiprobe: ok   {step}: {detail}"),
        Err(detail) => {
            FAILED.store(true, Ordering::Relaxed);
            tracing::error!("apiprobe: FAIL {step}: {detail}");
        }
    }
}

fn check(step: &str, passed: bool, ok: impl Into<String>, fail: impl Into<String>) {
    report(
        step,
        if passed {
            Ok(ok.into())
        } else {
            Err(fail.into())
        },
    );
}

const fn platform(x: i32, z: i32) -> BlockPos {
    BlockPos {
        x,
        y: PLATFORM_Y,
        z,
    }
}

fn platform_is_stone(world: &World) -> (bool, String) {
    let block = world.get_block_state(platform(0, 0)).get_block();
    (block.name.ends_with("stone"), block.name)
}

/// Runs `stage` for the player `name` one stage later, if they're still online.
fn later(name: String, stage: fn(&Server, &str)) {
    schedule_delayed_task(STAGE_TICKS, move |server: Server| {
        if server.get_player_by_name(&name).is_some() {
            stage(&server, &name);
        } else {
            report(
                "player online",
                Err(format!("{name} left before the probe finished")),
            );
        }
    });
}

fn teleport_into(server: &Server, name: &str, world: &str, y: f64) -> bool {
    let (Some(player), Some(target)) = (
        server.get_player_by_name(name),
        server.get_world_by_name(world),
    ) else {
        return false;
    };
    player.teleport_world(target, (0.5, y, 0.5), None, None);
    player.get_world().get_name() == world
}

struct Probe;

impl Plugin for Probe {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "apiprobe".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Xena Studios".into()],
            description: "Checks the plugin API calls Hallow minigames need.".into(),
            dependencies: vec![],
            permissions: vec![],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        context.register_event_handler(OnJoin, EventPriority::Normal, true)?;
        tracing::info!("apiprobe: loaded, waiting for a player to join");
        Ok(())
    }
}

struct OnJoin;

impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, event: PlayerJoinEventData) -> PlayerJoinEventData {
        if !RAN.swap(true, Ordering::Relaxed) {
            later(event.player.get_name(), create_and_enter);
        }
        event
    }
}

/// Stage 1: create a world for the match and send the player into it.
fn create_and_enter(server: &Server, name: &str) {
    tracing::info!("apiprobe: running for {name}");
    let arena = server.create_world(ARENA, Dimension::Overworld);
    check(
        "create world",
        server.has_world(ARENA) && arena.get_name() == ARENA,
        format!("{ARENA} created"),
        format!("{ARENA} missing after create-world"),
    );
    let entered = teleport_into(server, name, ARENA, f64::from(PLATFORM_Y) + 1.0);
    check(
        "teleport into world",
        entered,
        format!("{name} is in {ARENA}"),
        format!("{name} is not in {ARENA}"),
    );
    later(name.to_string(), build);
}

/// Stage 2: with the player there, the chunks are loaded: build and dress up.
fn build(server: &Server, name: &str) {
    let Some(arena) = server.get_world_by_name(ARENA) else {
        report("set blocks", Err(format!("{ARENA} is gone")));
        return;
    };
    match Block::of("minecraft:stone") {
        Some(stone) => {
            for x in -2..=2 {
                for z in -2..=2 {
                    arena.set_block(platform(x, z), &stone, BlockFlags::NOTIFY_LISTENERS);
                }
            }
            let (placed, read) = platform_is_stone(&arena);
            check(
                "set blocks",
                placed,
                "5x5 stone platform",
                format!("platform reads back as {read}"),
            );
        }
        None => report("set blocks", Err("minecraft:stone not found".into())),
    }

    let Some(player) = server.get_player_by_name(name) else {
        return;
    };
    match player.as_java() {
        Some(java) => {
            let scoreboard = java.get_scoreboard();
            scoreboard.add_objective(
                OBJECTIVE,
                TextComponent::text("Hallow probe"),
                RenderType::Integer,
                None,
            );
            scoreboard.set_display_slot(DisplaySlot::Sidebar, OBJECTIVE);
            scoreboard.update_score(name, OBJECTIVE, 42, None);
            let score = scoreboard.add_score(name, OBJECTIVE, 0);
            check(
                "scoreboard",
                score == 42,
                "sidebar objective with a score of 42",
                format!("score reads back as {score}"),
            );
        }
        None => report("scoreboard", Err("not a Java player".into())),
    }

    player
        .get_inventory()
        .as_inventory()
        .set_item(0, Some(ItemStack::new("minecraft:diamond_sword", 1)));
    report(
        "inventory",
        match player.get_inventory_item(0) {
            Some(stack)
                if stack.get_registry_key().trim_start_matches("minecraft:") == "diamond_sword" =>
            {
                Ok(format!(
                    "diamond sword in slot 0 (key reads back as {:?})",
                    stack.get_registry_key()
                ))
            }
            Some(stack) => Err(format!("slot 0 holds {}", stack.get_registry_key())),
            None => Err("slot 0 is empty".into()),
        },
    );
    later(name.to_string(), unload);
}

/// Stage 3: send the player home and unload the match world.
fn unload(server: &Server, name: &str) {
    let home = server
        .get_all_worlds()
        .into_iter()
        .map(|world| world.get_name())
        .find(|world| world != ARENA);
    if let Some(home) = home {
        teleport_into(server, name, &home, 120.0);
    }
    report(
        "unload world",
        server
            .unload_world(ARENA)
            .map(|()| format!("{ARENA} saved and unloaded")),
    );
    check(
        "world gone",
        !server.has_world(ARENA),
        format!("{ARENA} no longer loaded"),
        format!("{ARENA} still loaded"),
    );
    later(name.to_string(), reload);
}

/// Stage 4: load the world again from disk and go back in.
fn reload(server: &Server, name: &str) {
    server.create_world(ARENA, Dimension::Overworld);
    let entered = teleport_into(server, name, ARENA, f64::from(PLATFORM_Y) + 1.0);
    check(
        "load world from disk",
        entered,
        format!("{ARENA} loaded and entered"),
        format!("could not enter {ARENA}"),
    );
    later(name.to_string(), verify);
}

/// Stage 5: the platform built before the unload must still be there.
fn verify(server: &Server, _name: &str) {
    match server.get_world_by_name(ARENA) {
        Some(arena) => {
            let (kept, read) = platform_is_stone(&arena);
            check(
                "build survived reload",
                kept,
                "platform still there",
                format!("platform reads back as {read}"),
            );
        }
        None => report("build survived reload", Err(format!("{ARENA} is gone"))),
    }
    if FAILED.load(Ordering::Relaxed) {
        tracing::error!("apiprobe: done, some checks FAILED");
    } else {
        tracing::info!("apiprobe: done, all checks passed");
    }
}

register_plugin!(Probe);
