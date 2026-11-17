//! EggWars for Hallow: teams on sky islands, each guarding an egg.
//! Generators make iron, gold and diamonds; the shop keeper on every island
//! sells blocks, weapons, armour and tools for them. A player whose egg
//! stands respawns after a death; once it's broken, the next death is
//! final. The last team standing wins. Rules and tuning are in
//! `docs/games/EGGWARS.md`.
//!
//! One match per instance. The map is picked from `maps/` (copied into the
//! plugin's data folder) when the server loads, and built into the world as
//! its chunks generate, so every match starts on a fresh map.
//!
//! **Not on the tick.** Event handlers note what happened and decide
//! cancellations; one repeating task does the rest a tick or two later
//! (teleports, inventories, menus), never calling into Pumpkin in a way
//! that fires an event this plugin listens to.

mod gens;
mod menus;
mod rules;
mod shop;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard};

use hallow_islands::{
    build,
    combat::Combat,
    generator,
    map::{Layout, Pos},
    play::{self, COLOURS, Sidebars, TestHook},
    roster::{Life, Phase, Start},
};
use pumpkin_plugin_api::{
    Context, EntityType, Plugin, PluginMetadata, Server, World,
    common::{BlockPos, GameMode},
    events::{
        BlockBreakEvent, BlockPlaceEvent, EntityDamageByEntityEvent, EntityDamageEvent,
        EventHandler, EventPriority, InventoryClickEvent, InventoryCloseEvent, PlayerChatEvent,
        PlayerDeathEvent, PlayerInteractEntityEvent, PlayerInteractEvent, PlayerJoinEvent,
        PlayerLeaveEvent, PlayerMoveEvent,
    },
    events_wit::{
        BlockBreakEventData, BlockPlaceEventData, EntityDamageByEntityEventData,
        EntityDamageEventData, EntityInteractionAction, InteractAction, InventoryClickEventData,
        InventoryCloseEventData, PlayerChatEventData, PlayerDeathEventData,
        PlayerInteractEntityEventData, PlayerInteractEventData, PlayerJoinEventData,
        PlayerLeaveEventData, PlayerMoveEventData,
    },
    permissions, register_plugin, scheduler,
    text::TextComponent,
    wit::pumpkin::plugin::game_rules::{GameRule, GameRuleValue},
    world::BlockFlags,
};
use uuid::Uuid;

use crate::gens::Generator;
use crate::menus::Menu;
use crate::rules::Rules;
use crate::shop::{Armour, Pickaxe};

const NAME: &str = "hallow_eggwars";
/// The task runs every other tick.
const PERIOD: u64 = 2;
/// Ticks after a join before the player is moved (see docs/VALIDATION.md,
/// "Don't teleport a player during the join event").
const JOIN_DELAY: u64 = 20;
/// Players needed to start, unless `EGGWARS_MIN_PLAYERS` says otherwise.
const MIN_PLAYERS: usize = 2;
const TITLE: &str = "§6§lEGG WARS";

/// Something the task does.
#[derive(Clone, Debug, PartialEq)]
enum Action {
    Join(Uuid),
    Leave(Uuid),
    /// Pumpkin killed them.
    Died(Uuid),
    /// They fell below the map.
    Void(Uuid),
    /// Bring a dead player back from the death screen.
    Respawn(Uuid),
    /// Put a player at the spectator spot.
    Spectate(Uuid),
    OpenShop(Uuid),
    OpenUpgrade(Uuid, usize),
    Click(Uuid, u32),
    HitEgg(Uuid, usize),
    Hook(Uuid, TestHook),
}

/// What a player keeps through respawns.
#[derive(Clone, Copy, Debug, Default)]
struct Gear {
    armour: Armour,
    pickaxe: Pickaxe,
}

struct Game {
    layout: Layout,
    rules: Rules,
    phase: Phase,
    start: Start,
    ticks: u64,
    generators: Vec<(Pos, Generator)>,
    /// Blocks players placed: the only ones they can break.
    placed: BTreeSet<Pos>,
    /// Highest y a block can be placed at.
    build_limit: i32,
    /// Shop keepers' entity ids → team.
    shops: BTreeMap<i32, usize>,
    /// Players' entity ids.
    entities: BTreeMap<i32, Uuid>,
    menus: BTreeMap<Uuid, Menu>,
    gear: BTreeMap<Uuid, Gear>,
    actions: Vec<Action>,
    later: Vec<(u64, Action)>,
    sidebars: Sidebars,
    falling: BTreeSet<Uuid>,
    combat: Combat,
    drained: bool,
}

static GAME: Mutex<Option<Game>> = Mutex::new(None);

/// For event handlers: the game, unless the task holds it (a call of ours
/// fired the event; let it through untouched).
fn try_game() -> Option<MutexGuard<'static, Option<Game>>> {
    match GAME.try_lock() {
        Ok(guard) => Some(guard),
        Err(std::sync::TryLockError::Poisoned(err)) => Some(err.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

fn with<R>(f: impl FnOnce(&mut Game) -> R) -> Option<R> {
    let mut guard = try_game()?;
    guard.as_mut().map(f)
}

fn pos(p: &BlockPos) -> Pos {
    [p.x, p.y, p.z]
}

fn block_pos(p: Pos) -> BlockPos {
    BlockPos {
        x: p[0],
        y: p[1],
        z: p[2],
    }
}

impl Game {
    fn secs(&self) -> u64 {
        self.ticks / 20
    }

    fn push(&mut self, action: Action) {
        self.actions.push(action);
    }

    fn after(&mut self, ticks: u64, action: Action) {
        self.later.push((self.ticks + ticks, action));
    }

    fn playing(&self) -> bool {
        matches!(self.phase, Phase::Playing { .. })
    }

    fn egg_team(&self, at: Pos) -> Option<usize> {
        self.layout.teams.iter().position(|t| t.egg == Some(at))
    }

    fn generator_at(&self, at: Pos) -> Option<usize> {
        self.generators.iter().position(|(p, _)| *p == at)
    }

    fn alive(&self, id: Uuid) -> bool {
        self.rules
            .roster
            .get(id)
            .is_some_and(|m| m.online && m.life == Life::Alive)
    }

    fn colour(&self, id: Uuid) -> &'static str {
        self.rules
            .roster
            .team_of(id)
            .map_or("§7", |t| COLOURS[t % COLOURS.len()].code)
    }

    fn display(&self, id: Uuid) -> String {
        let name = self
            .rules
            .roster
            .get(id)
            .map_or_else(|| "?".to_owned(), |m| m.name.clone());
        format!("{}{name}", self.colour(id))
    }
}

fn broadcast(server: &Server, line: &str) {
    server.broadcast(line);
}

/// Runs every [`PERIOD`] ticks.
fn tick(server: &Server) {
    let mut guard = GAME.lock().unwrap_or_else(|err| err.into_inner());
    let Some(game) = guard.as_mut() else {
        return;
    };
    game.ticks += PERIOD;
    let now = game.ticks;
    let (due, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut game.later)
        .into_iter()
        .partition(|(at, _)| *at <= now);
    game.later = wait;
    let mut actions = std::mem::take(&mut game.actions);
    actions.extend(due.into_iter().map(|(_, a)| a));
    let Some(world) = generator::overworld(server) else {
        return;
    };
    for action in actions {
        handle(game, server, &world, action);
    }
    if now.is_multiple_of(10) && game.playing() {
        run_generators(game, server);
    }
    if now.is_multiple_of(20) {
        second(game, server, &world);
    }
}

fn handle(game: &mut Game, server: &Server, world: &World, action: Action) {
    let id = match &action {
        Action::Join(id)
        | Action::Leave(id)
        | Action::Died(id)
        | Action::Void(id)
        | Action::Respawn(id)
        | Action::Spectate(id)
        | Action::OpenShop(id)
        | Action::OpenUpgrade(id, _)
        | Action::Click(id, _)
        | Action::HitEgg(id, _)
        | Action::Hook(id, _) => *id,
    };
    if let Action::Leave(_) = action {
        leave(game, server, world, id);
        return;
    }
    let Some(player) = server.get_player_by_uuid(play::to_wit(id)) else {
        return;
    };
    match action {
        Action::Join(_) => join(game, &player, id),
        Action::Leave(_) => {}
        Action::Died(_) => {
            died(game, server, world, id, false);
            game.after(2, Action::Respawn(id));
        }
        Action::Void(_) => {
            if game.playing() && game.alive(id) {
                player.get_inventory().clear_all();
                died(game, server, world, id, true);
                to_spectate(game, &player);
            } else {
                // Before or after the match: back to where they belong.
                game.falling.remove(&id);
                place_waiting(game, &player);
            }
        }
        Action::Respawn(_) => {
            player.respawn();
            game.after(10, Action::Spectate(id));
        }
        Action::Spectate(_) => to_spectate(game, &player),
        Action::OpenShop(_) => menus::open_main(game, &player, id),
        Action::OpenUpgrade(_, g) => menus::open_upgrade(game, &player, id, g),
        Action::Click(_, slot) => menus::click(game, server, world, &player, id, slot),
        Action::HitEgg(_, team) => hit_egg(game, server, world, Some(id), team),
        Action::Hook(_, hook) => test_hook(game, server, world, &player, id, &hook),
    }
}

fn join(game: &mut Game, player: &pumpkin_plugin_api::Player, id: Uuid) {
    let waiting = matches!(game.phase, Phase::Waiting | Phase::Countdown { .. });
    let name = player.get_name();
    if waiting && let Some(team) = game.rules.roster.join(id, &name) {
        game.gear.insert(id, Gear::default());
        place_waiting(game, player);
        let colour = COLOURS[team % COLOURS.len()];
        play::title(
            player,
            TITLE,
            &format!("§7You're on {}{} §7team", colour.code, colour.name),
        );
        play::chat(
            player,
            &format!(
                "§6Egg Wars §7on §f{}§7. Guard your egg, break theirs, last team standing wins.",
                game.layout.name
            ),
        );
        play::chat(
            player,
            "§7Right-click your island's §eshop keeper §7to spend iron, gold and diamonds. Right-click a §fgenerator §7to upgrade it.",
        );
        return;
    }
    // A late arrival, or back after leaving mid-match: watch.
    play::chat(player, "§7The match is under way; you're spectating.");
    to_spectate(game, player);
}

fn place_waiting(game: &Game, player: &pumpkin_plugin_api::Player) {
    let spot = game.layout.waiting.unwrap_or(game.layout.spectate);
    player.set_gamemode(GameMode::Adventure);
    player.get_inventory().clear_all();
    player.teleport(
        (spot[0], spot[1], spot[2]),
        Some(0.0),
        Some(0.0),
        player.get_world(),
    );
}

fn to_spectate(game: &mut Game, player: &pumpkin_plugin_api::Player) {
    let spot = game.layout.spectate;
    player.set_gamemode(GameMode::Spectator);
    player.teleport((spot[0], spot[1], spot[2]), None, None, player.get_world());
    let id = play::to_uuid(&player.get_id());
    game.falling.remove(&id);
    if let Some(Life::Respawning { at }) = game.rules.roster.get(id).map(|m| m.life) {
        let left = at.saturating_sub(game.secs());
        play::title(
            player,
            "§c§lYOU DIED",
            &format!("§7Respawning in §f{left}§7 s"),
        );
    }
}

fn leave(game: &mut Game, server: &Server, world: &World, id: Uuid) {
    game.menus.remove(&id);
    game.sidebars.forget(id);
    game.falling.remove(&id);
    game.entities.retain(|_, p| *p != id);
    match game.phase {
        Phase::Waiting | Phase::Countdown { .. } => {
            game.rules.roster.remove(id);
            game.gear.remove(&id);
        }
        Phase::Playing { .. } => {
            if game.rules.roster.get(id).is_some() {
                let who = game.display(id);
                game.rules.roster.leave(id);
                broadcast(server, &format!("{who} §7left the match."));
                check_end(game, server, world);
            }
        }
        Phase::Over { .. } => {}
    }
}

/// Bookkeeping for a death; the caller moves the player.
fn died(game: &mut Game, server: &Server, world: &World, id: Uuid, void: bool) {
    if !game.playing() {
        return;
    }
    let now = game.secs();
    let Some(death) = game.rules.death(id, None, now) else {
        return;
    };
    let victim = game.display(id);
    let how = if void { "fell into the void" } else { "died" };
    let line = match death.killer {
        Some(killer) => {
            let by = game.display(killer);
            let how = if void {
                "was knocked into the void by"
            } else {
                "was slain by"
            };
            format!("{victim} §7{how} {by}§7.")
        }
        None => format!("{victim} §7{how}."),
    };
    let line = if death.final_death {
        format!("{line} §b§lFINAL KILL!")
    } else {
        line
    };
    broadcast(server, &line);
    play::stat(id, "deaths", 1);
    if let Some(killer) = death.killer {
        play::stat(killer, "kills", 1);
        if death.final_death {
            play::stat(killer, "final_kills", 1);
        }
    }
    if death.final_death {
        play::stat(id, "final_deaths", 1);
    }
    check_end(game, server, world);
}

fn hit_egg(game: &mut Game, server: &Server, world: &World, by: Option<Uuid>, team: usize) {
    if !game.playing() {
        return;
    }
    let player = by.and_then(|id| server.get_player_by_uuid(play::to_wit(id)));
    match game.rules.break_egg(team, by) {
        rules::EggBreak::No(why) => {
            if let Some(player) = &player {
                player.show_actionbar(TextComponent::from_legacy_string(&format!("§c{why}")));
            }
        }
        rules::EggBreak::Broken { team, out } => {
            if let Some(egg) = game.layout.teams[team].egg
                && let Some(air) = generator::block_state("air")
            {
                world.set_block_state(block_pos(egg), air, BlockFlags::NOTIFY_LISTENERS);
            }
            let colour = COLOURS[team % COLOURS.len()];
            let line = match by {
                Some(by) => format!(
                    "§f§lEGG BROKEN › {}{}'s §7egg was smashed by {}§7!",
                    colour.code,
                    colour.name,
                    game.display(by)
                ),
                None => format!(
                    "§f§lEGG BROKEN › {}{}'s §7egg cracked!",
                    colour.code, colour.name
                ),
            };
            broadcast(server, &line);
            if let Some(by) = by {
                play::stat(by, "eggs_broken", 1);
            }
            for member in game.rules.roster.team(team).to_vec() {
                if let Some(p) = server.get_player_by_uuid(play::to_wit(member)) {
                    play::title(&p, "§c§lEGG BROKEN", "§7You won't respawn any more!");
                }
            }
            for id in out {
                broadcast(server, &format!("{} §7is out.", game.display(id)));
                play::stat(id, "final_deaths", 1);
            }
            check_end(game, server, world);
        }
    }
}

fn check_end(game: &mut Game, server: &Server, world: &World) {
    if !game.playing() {
        return;
    }
    if let Some(winner) = game.rules.result() {
        end(game, server, world, winner);
    }
}

fn end(game: &mut Game, server: &Server, _world: &World, winner: Option<usize>) {
    game.phase = Phase::Over {
        drains: game.secs() + rules::OVER_SECS,
    };
    let headline = match winner {
        Some(team) => {
            let colour = COLOURS[team % COLOURS.len()];
            format!("{}{} team wins!", colour.code, colour.name)
        }
        None => "§7Nobody wins this time.".to_owned(),
    };
    broadcast(server, &format!("§6§l» §r§6Match over! {headline}"));
    let mut best: Vec<(u32, String)> = game
        .rules
        .roster
        .members()
        .map(|(id, m)| (m.kills, game.display(*id)))
        .filter(|(k, _)| *k > 0)
        .collect();
    best.sort_by(|a, b| b.0.cmp(&a.0));
    for (i, (kills, who)) in best.iter().take(3).enumerate() {
        broadcast(
            server,
            &format!("§7 #{} {who} §7· §f{kills} §7kills", i + 1),
        );
    }
    let members: Vec<(Uuid, usize)> = game
        .rules
        .roster
        .members()
        .map(|(id, m)| (*id, m.team))
        .collect();
    for (id, team) in members {
        play::stat(id, "games_played", 1);
        let won = winner == Some(team);
        play::stat(id, if won { "wins" } else { "losses" }, 1);
        if let Some(player) = server.get_player_by_uuid(play::to_wit(id)) {
            if won {
                play::title(&player, "§6§lVICTORY!", &headline);
            } else {
                play::title(&player, "§c§lGAME OVER", &headline);
            }
        }
    }
}

/// Starts the match.
fn begin(game: &mut Game, server: &Server, world: &World) {
    game.rules.start();
    game.phase = Phase::Playing { since: game.secs() };
    play::set_state("in_game");
    let air = generator::block_state("air").unwrap_or(0);
    // Teams nobody joined have no egg.
    for (team, standing) in game.rules.eggs.clone().into_iter().enumerate() {
        if !standing && let Some(egg) = game.layout.teams[team].egg {
            world.set_block_state(block_pos(egg), air, BlockFlags::NOTIFY_LISTENERS);
        }
    }
    if let Some(spot) = game.layout.waiting {
        for at in build::waiting_box(spot) {
            world.set_block_state(block_pos(at), air, BlockFlags::NOTIFY_LISTENERS);
        }
    }
    // Shop keepers on islands that have a team.
    for (team, site) in game.layout.teams.iter().enumerate() {
        let Some(at) = site.shop else { continue };
        if game.rules.roster.team(team).is_empty() {
            continue;
        }
        let keeper = world.spawn_entity(EntityType::Villager, (at[0], at[1], at[2]));
        keeper.set_invulnerable(true);
        keeper.set_silent(true);
        keeper.set_custom_name(TextComponent::from_legacy_string("§e§lSHOP"));
        keeper.set_custom_name_visible(true);
        keeper.set_rotation(site.yaw, 0.0);
        if let Some(mob) = keeper.as_mob() {
            mob.set_ai_disabled(true);
        }
        game.shops
            .insert(i32::try_from(keeper.get_id()).unwrap_or(-1), team);
    }
    let members: Vec<Uuid> = game.rules.roster.members().map(|(id, _)| *id).collect();
    for id in members {
        if let Some(player) = server.get_player_by_uuid(play::to_wit(id)) {
            spawn(game, &player, id);
            play::title(&player, "§a§lGO!", "§7Protect your egg!");
        }
    }
    broadcast(
        server,
        "§a§lThe match has started! §r§7Collect resources, buy gear and break the other eggs.",
    );
}

/// Puts a player on their island with their kit.
fn spawn(game: &mut Game, player: &pumpkin_plugin_api::Player, id: Uuid) {
    let Some(team) = game.rules.roster.team_of(id) else {
        return;
    };
    let site = &game.layout.teams[team];
    let index = game
        .rules
        .roster
        .team(team)
        .iter()
        .position(|m| *m == id)
        .unwrap_or(0);
    let spot = site.spawns[index % site.spawns.len()];
    player.set_gamemode(GameMode::Survival);
    player.teleport(
        (spot[0], spot[1], spot[2]),
        Some(site.yaw),
        Some(0.0),
        player.get_world(),
    );
    game.falling.remove(&id);
    let gear = game.gear.get(&id).copied().unwrap_or_default();
    menus::kit(player, team, gear);
    game.combat.setup(player);
}

fn run_generators(game: &mut Game, server: &Server) {
    for (_, generator) in &mut game.generators {
        generator.run(10);
    }
    let players: Vec<(Uuid, pumpkin_plugin_api::Player)> = server
        .get_all_players()
        .into_iter()
        .map(|p| (play::to_uuid(&p.get_id()), p))
        .filter(|(id, _)| game.alive(*id))
        .collect();
    for (at, generator) in &mut game.generators {
        if generator.stored == 0 {
            continue;
        }
        let (gx, gy, gz) = (
            f64::from(at[0]) + 0.5,
            f64::from(at[1]) + 1.0,
            f64::from(at[2]) + 0.5,
        );
        let taker = players.iter().find(|(_, p)| {
            let (x, y, z) = p.get_position();
            (x - gx).abs() <= 1.6 && (z - gz).abs() <= 1.6 && (gy - 0.5..gy + 2.5).contains(&y)
        });
        if let Some((_, player)) = taker {
            let amount = generator.collect();
            let key = gens::resource(generator.tier).item();
            hallow_islands::items::give(player, key, amount);
        }
    }
}

/// Once a second: phases, respawns, sidebars.
fn second(game: &mut Game, server: &Server, world: &World) {
    let now = game.secs();
    match game.phase {
        Phase::Waiting | Phase::Countdown { .. } => {
            let roster = &game.rules.roster;
            let next = game.start.next(
                game.phase,
                now,
                roster.player_count(),
                roster.capacity(),
                roster.teams_with_players(),
            );
            match (game.phase, next) {
                (Phase::Waiting, Phase::Countdown { starts }) => {
                    broadcast(
                        server,
                        &format!("§eThe match starts in §f{}§e s.", starts - now),
                    );
                }
                (Phase::Countdown { .. }, Phase::Waiting) => {
                    broadcast(server, "§cNot enough players; waiting for more.");
                }
                _ => {}
            }
            game.phase = next;
            if let Phase::Countdown { starts } = game.phase {
                let left = starts.saturating_sub(now);
                if left == 0 {
                    begin(game, server, world);
                } else if [10, 5, 4, 3, 2, 1].contains(&left) {
                    broadcast(server, &format!("§eStarting in §f{left}§e..."));
                }
            }
        }
        Phase::Playing { since } => {
            for id in game.rules.roster.respawns(now) {
                if let Some(player) = server.get_player_by_uuid(play::to_wit(id)) {
                    spawn(game, &player, id);
                    play::title(&player, "§a§lRESPAWNED", "");
                }
            }
            let played = now - since;
            if played == rules::EGGS_CRACK_SECS {
                broadcast(server, "§c§lSUDDEN DEATH! §r§7Every egg has cracked.");
                for (_, out) in game.rules.crack_all() {
                    for id in out {
                        broadcast(server, &format!("{} §7is out.", game.display(id)));
                    }
                }
                let air = generator::block_state("air").unwrap_or(0);
                for site in &game.layout.teams {
                    if let Some(egg) = site.egg {
                        world.set_block_state(block_pos(egg), air, BlockFlags::NOTIFY_LISTENERS);
                    }
                }
                check_end(game, server, world);
            } else if played >= rules::DRAW_SECS {
                end(game, server, world, None);
            } else {
                check_end(game, server, world);
            }
        }
        Phase::Over { drains } => {
            if now >= drains && !game.drained {
                game.drained = true;
                broadcast(server, "§7Sending you back to the lobby...");
                play::set_state("draining");
            }
        }
    }
    sidebars(game, server);
}

fn sidebars(game: &mut Game, server: &Server) {
    let now = game.secs();
    let status = match game.phase {
        Phase::Waiting => "§fWaiting for players".to_owned(),
        Phase::Countdown { starts } => {
            format!("§fStarting in §a{}", play::clock(starts - now.min(starts)))
        }
        Phase::Playing { since } => {
            let played = now - since;
            if played < rules::EGGS_CRACK_SECS {
                format!(
                    "§fEggs crack in §e{}",
                    play::clock(rules::EGGS_CRACK_SECS - played)
                )
            } else {
                format!(
                    "§fDraw in §c{}",
                    play::clock(rules::DRAW_SECS.saturating_sub(played))
                )
            }
        }
        Phase::Over { .. } => "§6Match over".to_owned(),
    };
    let mut lines = vec![
        format!("§7{}", game.layout.name),
        String::new(),
        status,
        String::new(),
    ];
    let roster = &game.rules.roster;
    for team in 0..roster.team_count() {
        let colour = COLOURS[team % COLOURS.len()];
        let members = roster.team(team);
        let state = if game.rules.eggs[team] && (!game.playing() || roster.team_in(team)) {
            "§a✔".to_owned()
        } else if roster.team_in(team) {
            let left = members
                .iter()
                .filter(|m| {
                    roster
                        .get(**m)
                        .is_some_and(|m| m.online && m.life != Life::Out)
                })
                .count();
            format!("§e{left}")
        } else {
            "§c✘".to_owned()
        };
        let label = if members.is_empty() && game.playing() {
            "§8".to_owned()
        } else {
            colour.code.to_owned()
        };
        lines.push(format!(
            "{label}{} §f{}: {state}",
            &colour.name[..1],
            colour.name
        ));
    }
    lines.push(String::new());
    for player in server.get_all_players() {
        let id = play::to_uuid(&player.get_id());
        let mut own = lines.clone();
        if let Some(member) = game.rules.roster.get(id) {
            own.push(format!("§fKills: §a{}", member.kills));
            own.push(format!("§fFinal kills: §a{}", member.final_kills));
            own.push(String::new());
        }
        own.push("§6xena studios".to_owned());
        game.sidebars.show(&player, TITLE, &own);
    }
}

fn test_hook(
    game: &mut Game,
    server: &Server,
    world: &World,
    player: &pumpkin_plugin_api::Player,
    id: Uuid,
    hook: &TestHook,
) {
    tracing::info!("eggwars: test hook {hook:?} from {id}");
    match hook {
        TestHook::Start => {
            if let Phase::Countdown { starts } = game.phase {
                game.phase = Phase::Countdown {
                    starts: starts.min(game.secs() + 3),
                };
            }
        }
        TestHook::Die => game.push(Action::Void(id)),
        TestHook::BreakEgg => {
            let own = game.rules.roster.team_of(id);
            let target =
                (0..game.rules.eggs.len()).find(|t| Some(*t) != own && game.rules.eggs[*t]);
            match target {
                Some(team) => hit_egg(game, server, world, Some(id), team),
                None => play::chat(player, "§7No egg to break."),
            }
        }
    }
}

// ---- Events --------------------------------------------------------------

fn player_id(player: &pumpkin_plugin_api::Player) -> Uuid {
    play::to_uuid(&player.get_id())
}

struct OnJoin;

impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, event: PlayerJoinEventData) -> PlayerJoinEventData {
        let id = player_id(&event.player);
        let entity = i32::try_from(event.player.as_entity().get_id()).unwrap_or(-1);
        with(|game| {
            game.entities.insert(entity, id);
            game.after(JOIN_DELAY, Action::Join(id));
        });
        event
    }
}

struct OnLeave;

impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, event: PlayerLeaveEventData) -> PlayerLeaveEventData {
        let id = player_id(&event.player);
        with(|game| game.push(Action::Leave(id)));
        event
    }
}

struct OnBreak;

impl EventHandler<BlockBreakEvent> for OnBreak {
    fn handle(&self, _server: Server, mut event: BlockBreakEventData) -> BlockBreakEventData {
        let at = pos(&event.block_pos);
        let id = event.player.as_ref().map(player_id);
        with(|game| {
            if !game.playing() || id.is_some_and(|id| !game.alive(id)) {
                event.cancelled = true;
            } else if let Some(team) = game.egg_team(at) {
                event.cancelled = true;
                if let Some(id) = id {
                    game.push(Action::HitEgg(id, team));
                }
            } else if game.placed.remove(&at) {
                // A player's block: fine to break.
            } else {
                event.cancelled = true;
            }
        });
        event
    }
}

struct OnPlace;

impl EventHandler<BlockPlaceEvent> for OnPlace {
    fn handle(&self, _server: Server, mut event: BlockPlaceEventData) -> BlockPlaceEventData {
        let at = pos(&event.block_pos);
        let id = player_id(&event.player);
        with(|game| {
            let on_pad = game.generators.iter().any(|(g, _)| {
                g[0] == at[0] && g[2] == at[2] && (g[1] + 1..=g[1] + 2).contains(&at[1])
            });
            let outside = f64::from(at[0].abs().max(at[2].abs())) > game.layout.border;
            if !game.playing() || !game.alive(id) || on_pad || outside || at[1] > game.build_limit {
                event.cancelled = true;
            } else {
                game.placed.insert(at);
            }
        });
        event
    }
}

struct OnInteract;

impl EventHandler<PlayerInteractEvent> for OnInteract {
    fn handle(
        &self,
        _server: Server,
        mut event: PlayerInteractEventData,
    ) -> PlayerInteractEventData {
        let Some(at) = event.clicked_pos.as_ref().map(pos) else {
            return event;
        };
        let id = player_id(&event.player);
        let action = event.action;
        with(|game| {
            if let Some(team) = game.egg_team(at) {
                // The egg would teleport away when touched; it's ours to handle.
                event.cancelled = true;
                if action == InteractAction::LeftClickBlock && game.playing() {
                    game.push(Action::HitEgg(id, team));
                }
            } else if action == InteractAction::RightClickBlock
                && let Some(g) = game.generator_at(at)
                && game.playing()
            {
                event.cancelled = true;
                game.push(Action::OpenUpgrade(id, g));
            }
        });
        event
    }
}

struct OnInteractEntity;

impl EventHandler<PlayerInteractEntityEvent> for OnInteractEntity {
    fn handle(
        &self,
        _server: Server,
        mut event: PlayerInteractEntityEventData,
    ) -> PlayerInteractEntityEventData {
        let id = player_id(&event.player);
        let entity = event.entity_id;
        let open = event.action != EntityInteractionAction::Attack;
        with(|game| {
            if game.shops.contains_key(&entity) {
                event.cancelled = true;
                if open && game.alive(id) {
                    game.push(Action::OpenShop(id));
                }
            }
        });
        event
    }
}

struct OnClick;

impl EventHandler<InventoryClickEvent> for OnClick {
    fn handle(
        &self,
        _server: Server,
        mut event: InventoryClickEventData,
    ) -> InventoryClickEventData {
        let id = player_id(&event.player);
        let slot = event.slot;
        with(|game| {
            if game.menus.contains_key(&id) {
                event.cancelled = true;
                if let Ok(slot) = u32::try_from(slot) {
                    game.push(Action::Click(id, slot));
                }
            }
        });
        event
    }
}

struct OnClose;

impl EventHandler<InventoryCloseEvent> for OnClose {
    fn handle(&self, _server: Server, event: InventoryCloseEventData) -> InventoryCloseEventData {
        let id = player_id(&event.player);
        with(|game| game.menus.remove(&id));
        event
    }
}

struct OnDamage;

impl EventHandler<EntityDamageEvent> for OnDamage {
    fn handle(&self, _server: Server, mut event: EntityDamageEventData) -> EntityDamageEventData {
        let entity = event.entity_id;
        with(|game| {
            if game.shops.contains_key(&entity) {
                event.cancelled = true;
            } else if let Some(id) = game.entities.get(&entity).copied()
                && (!game.playing() || !game.alive(id))
            {
                event.cancelled = true;
            }
        });
        event
    }
}

struct OnHit;

impl EventHandler<EntityDamageByEntityEvent> for OnHit {
    fn handle(
        &self,
        _server: Server,
        mut event: EntityDamageByEntityEventData,
    ) -> EntityDamageByEntityEventData {
        let (victim, attacker) = (event.entity_id, event.damager_id);
        with(|game| {
            if game.shops.contains_key(&victim) {
                event.cancelled = true;
                return;
            }
            let (Some(v), Some(a)) = (
                game.entities.get(&victim).copied(),
                game.entities.get(&attacker).copied(),
            ) else {
                return;
            };
            if game.rules.roster.teammates(v, a) {
                event.cancelled = true;
                return;
            }
            let now = game.secs();
            game.rules.roster.hit(v, a, now);
            event.damage = game.combat.damage(event.damage);
        });
        event
    }
}

struct OnDeath;

impl EventHandler<PlayerDeathEvent> for OnDeath {
    fn handle(&self, _server: Server, mut event: PlayerDeathEventData) -> PlayerDeathEventData {
        let id = player_id(&event.player);
        event.keep_inventory = false;
        with(|game| game.push(Action::Died(id)));
        event
    }
}

struct OnMove;

impl EventHandler<PlayerMoveEvent> for OnMove {
    fn handle(&self, _server: Server, event: PlayerMoveEventData) -> PlayerMoveEventData {
        let y = event.to_position.1;
        let id = player_id(&event.player);
        with(|game| {
            if y < f64::from(game.layout.void_y) && game.falling.insert(id) {
                game.push(Action::Void(id));
            }
        });
        event
    }
}

struct OnChat;

impl EventHandler<PlayerChatEvent> for OnChat {
    fn handle(&self, _server: Server, mut event: PlayerChatEventData) -> PlayerChatEventData {
        if let Some(hook) = play::parse_test_hook(&event.message) {
            let id = player_id(&event.player);
            event.cancelled = true;
            with(|game| game.push(Action::Hook(id, hook)));
        }
        event
    }
}

fn setup_world(world: &World) {
    let rules = [
        (GameRule::AdvanceTime, GameRuleValue::Bool(false)),
        (GameRule::AdvanceWeather, GameRuleValue::Bool(false)),
        (GameRule::SpawnMobs, GameRuleValue::Bool(false)),
        (GameRule::SpawnMonsters, GameRuleValue::Bool(false)),
        (GameRule::ShowDeathMessages, GameRuleValue::Bool(false)),
        (GameRule::ImmediateRespawn, GameRuleValue::Bool(true)),
        (GameRule::KeepInventory, GameRuleValue::Bool(false)),
    ];
    for (rule, value) in rules {
        world.set_game_rule(rule, value);
    }
    world.set_time_of_day(6000);
}

struct EggWars;

impl Plugin for EggWars {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Xena Studios".into()],
            description: "Egg Wars: guard your egg, break theirs, last team standing wins.".into(),
            dependencies: vec![],
            permissions: vec![
                permissions::FS_READ_DATA.into(),
                format!("{}{}", permissions::SYS_ENV_PREFIX, play::TEST_HOOKS_VAR),
                format!("{}{}", permissions::SYS_ENV_PREFIX, play::MAP_VAR),
                format!("{}EGGWARS_MIN_PLAYERS", permissions::SYS_ENV_PREFIX),
                format!("{}HALLOW_COMBAT", permissions::SYS_ENV_PREFIX),
            ],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        let maps = play::load_maps(&format!("{}/maps", context.get_data_folder()), "teams");
        let wanted = std::env::var(play::MAP_VAR).ok();
        let index = play::pick_map(&maps, wanted.as_deref(), play::roll())
            .ok_or("eggwars: no maps in the data folder's maps/")?;
        let (id, map) = &maps[index];
        let layout = map.layout();
        let server = context.get_server();
        let world = generator::overworld(&server).ok_or("eggwars: no overworld")?;
        generator::build_into(&world, &layout)?;
        setup_world(&world);
        let generators = layout
            .all_generators()
            .map(|g| (g.at, Generator::new(g.tier, g.level)))
            .collect();
        let build_limit = layout
            .shapes
            .iter()
            .map(|s| s.bounds().1[1])
            .max()
            .unwrap_or(64)
            + 24;
        let min_players = std::env::var("EGGWARS_MIN_PLAYERS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(MIN_PLAYERS);
        tracing::info!(
            "eggwars: map {} ({id}), {} teams of {}",
            layout.name,
            layout.teams.len(),
            layout.team_size
        );
        let rules = Rules::new(layout.teams.len(), usize::from(layout.team_size));
        *GAME.lock().unwrap_or_else(|err| err.into_inner()) = Some(Game {
            rules,
            phase: Phase::Waiting,
            start: Start {
                min_players,
                countdown: rules::COUNTDOWN_SECS,
                full_countdown: rules::FULL_COUNTDOWN_SECS,
            },
            ticks: 0,
            generators,
            placed: BTreeSet::new(),
            build_limit,
            shops: BTreeMap::new(),
            entities: BTreeMap::new(),
            menus: BTreeMap::new(),
            gear: BTreeMap::new(),
            actions: Vec::new(),
            later: Vec::new(),
            sidebars: Sidebars::default(),
            falling: BTreeSet::new(),
            combat: Combat::from_env(),
            drained: false,
            layout,
        });
        let hooks = play::test_hooks_on();
        let results = [
            (
                "join",
                context.register_event_handler(OnJoin, EventPriority::Normal, false),
            ),
            (
                "leave",
                context.register_event_handler(OnLeave, EventPriority::Normal, false),
            ),
            (
                "break",
                context.register_event_handler(OnBreak, EventPriority::Normal, true),
            ),
            (
                "place",
                context.register_event_handler(OnPlace, EventPriority::Normal, true),
            ),
            (
                "interact",
                context.register_event_handler(OnInteract, EventPriority::Normal, true),
            ),
            (
                "interact entity",
                context.register_event_handler(OnInteractEntity, EventPriority::Normal, true),
            ),
            (
                "click",
                context.register_event_handler(OnClick, EventPriority::Normal, true),
            ),
            (
                "close",
                context.register_event_handler(OnClose, EventPriority::Normal, false),
            ),
            (
                "damage",
                context.register_event_handler(OnDamage, EventPriority::Normal, true),
            ),
            (
                "hit",
                context.register_event_handler(OnHit, EventPriority::Normal, true),
            ),
            (
                "death",
                context.register_event_handler(OnDeath, EventPriority::Normal, true),
            ),
            (
                "move",
                context.register_event_handler(OnMove, EventPriority::Normal, false),
            ),
        ];
        for (event, result) in results {
            if let Err(err) = result {
                tracing::warn!("eggwars: can't subscribe to {event} events: {err}");
            }
        }
        if hooks {
            tracing::warn!(
                "eggwars: test hooks are on ({}); not for a real network",
                play::TEST_HOOKS_VAR
            );
            context
                .register_event_handler(OnChat, EventPriority::Normal, true)
                .map_err(|err| format!("eggwars: can't subscribe to chat: {err}"))?;
        }
        scheduler::schedule_repeating_task(PERIOD, PERIOD, |server| tick(&server));
        Ok(())
    }
}

register_plugin!(EggWars);
