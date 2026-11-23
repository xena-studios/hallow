//! SkyWars for Hallow: players start in glass cages on their own islands,
//! loot the chests, bridge to the richer middle and fight. One life each;
//! the last player (solo) or team (teams) standing wins. Chests refill
//! twice, and late in the match the world border closes in. Rules and
//! tuning are in `docs/games/SKYWARS.md`.
//!
//! The same plugin runs both kinds: `skywars` (solo) and `skywars-teams`,
//! set by `SKYWARS_MODE` in each kind's `hallow.toml`. Maps say which mode
//! they're for. One match per instance; the map is built fresh as its
//! chunks generate.
//!
//! Like EggWars, event handlers only note things and decide cancellations;
//! the repeating task does the rest.

mod loot;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard};

use hallow_islands::{
    build,
    combat::Combat,
    generator,
    map::{Layout, Loot, Pos},
    play::{self, COLOURS, Sidebars, TestHook},
    roster::{Life, Phase, Roster, Start},
};
use pumpkin_plugin_api::{
    Context, ItemStack, Player, Plugin, PluginMetadata, Server, World,
    block_entity::BlockEntityType,
    common::{BlockPos, GameMode},
    enchantments_wit::Enchantment,
    events::{
        BlockBreakEvent, BlockPlaceEvent, EntityDamageByEntityEvent, EntityDamageEvent,
        EventHandler, EventPriority, PlayerChatEvent, PlayerDeathEvent, PlayerJoinEvent,
        PlayerLeaveEvent, PlayerMoveEvent,
    },
    events_wit::{
        BlockBreakEventData, BlockPlaceEventData, EntityDamageByEntityEventData,
        EntityDamageEventData, PlayerChatEventData, PlayerDeathEventData, PlayerJoinEventData,
        PlayerLeaveEventData, PlayerMoveEventData,
    },
    permissions, register_plugin, scheduler,
    wit::pumpkin::plugin::game_rules::{GameRule, GameRuleValue},
    world::BlockFlags,
};
use uuid::Uuid;

use crate::loot::{Enchant, Kind, Rng};

const NAME: &str = "hallow_skywars";
const PERIOD: u64 = 2;
const JOIN_DELAY: u64 = 20;
const MIN_PLAYERS: usize = 2;
const TITLE: &str = "§b§lSKY WARS";

/// Timings, in seconds from the start.
const REFILLS: [u64; 2] = [4 * 60, 8 * 60];
const BORDER_SECS: u64 = 10 * 60;
const BORDER_SHRINK_SECS: u64 = 2 * 60;
const BORDER_END: f64 = 24.0;
const DRAW_SECS: u64 = 14 * 60;
const OVER_SECS: u64 = 8;
const COUNTDOWN_SECS: u64 = 20;
const FULL_COUNTDOWN_SECS: u64 = 6;

#[derive(Clone, Debug, PartialEq)]
enum Action {
    Join(Uuid),
    Leave(Uuid),
    Died(Uuid),
    Void(Uuid),
    Respawn(Uuid),
    Spectate(Uuid),
    Hook(Uuid, TestHook),
}

/// A chest still to place or fill.
#[derive(Clone, Debug)]
struct Chest {
    at: Pos,
    loot: Loot,
    must: Vec<Kind>,
}

struct Game {
    layout: Layout,
    roster: Roster,
    phase: Phase,
    start: Start,
    ticks: u64,
    chests: Vec<Chest>,
    /// Chests not placed and filled yet.
    pending: Vec<usize>,
    refilled: usize,
    border_started: bool,
    entities: BTreeMap<i32, Uuid>,
    actions: Vec<Action>,
    later: Vec<(u64, Action)>,
    sidebars: Sidebars,
    falling: BTreeSet<Uuid>,
    combat: Combat,
    rng: Rng,
    drained: bool,
}

static GAME: Mutex<Option<Game>> = Mutex::new(None);

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

const fn block_pos(p: Pos) -> BlockPos {
    BlockPos {
        x: p[0],
        y: p[1],
        z: p[2],
    }
}

fn player_id(player: &Player) -> Uuid {
    play::to_uuid(&player.get_id())
}

impl Game {
    const fn secs(&self) -> u64 {
        self.ticks / 20
    }

    fn after(&mut self, ticks: u64, action: Action) {
        self.later.push((self.ticks + ticks, action));
    }

    const fn playing(&self) -> bool {
        matches!(self.phase, Phase::Playing { .. })
    }

    fn alive(&self, id: Uuid) -> bool {
        self.roster
            .get(id)
            .is_some_and(|m| m.online && m.life == Life::Alive)
    }

    fn solo(&self) -> bool {
        self.layout.team_size <= 1
    }

    fn display(&self, id: Uuid) -> String {
        let Some(member) = self.roster.get(id) else {
            return "§7?".into();
        };
        if self.solo() {
            format!("§e{}", member.name)
        } else {
            format!(
                "{}{}",
                COLOURS[member.team % COLOURS.len()].code,
                member.name
            )
        }
    }

    fn winner_name(&self, team: usize) -> String {
        if self.solo() {
            self.roster
                .team(team)
                .first()
                .map_or_else(|| "?".into(), |id| self.display(*id))
        } else {
            let colour = COLOURS[team % COLOURS.len()];
            format!("{}{} team", colour.code, colour.name)
        }
    }
}

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
        | Action::Hook(id, _) => *id,
    };
    if action == Action::Leave(id) {
        leave(game, server, id);
        return;
    }
    let Some(player) = server.get_player_by_uuid(play::to_wit(id)) else {
        return;
    };
    match action {
        Action::Join(_) => join(game, &player, id),
        Action::Leave(_) => {}
        Action::Died(_) => {
            died(game, server, id, false);
            game.after(2, Action::Respawn(id));
        }
        Action::Void(_) => {
            if game.playing() && game.alive(id) {
                player.get_inventory().clear_all();
                died(game, server, id, true);
                to_spectate(game, &player);
            } else if game.roster.get(id).is_some() && !game.playing() {
                game.falling.remove(&id);
                to_cage(game, &player, id);
            } else {
                to_spectate(game, &player);
            }
        }
        Action::Respawn(_) => {
            player.respawn();
            game.after(10, Action::Spectate(id));
        }
        Action::Spectate(_) => to_spectate(game, &player),
        Action::Hook(_, hook) => match hook {
            TestHook::Start => {
                if let Phase::Countdown { starts } = game.phase {
                    game.phase = Phase::Countdown {
                        starts: starts.min(game.secs() + 3),
                    };
                }
            }
            TestHook::Die => game.actions.push(Action::Void(id)),
            TestHook::BreakEgg => play::chat(&player, "§7There are no eggs in Sky Wars."),
        },
    }
    let _ = world;
}

/// The spawn point of a player's cage.
fn cage_spot(game: &Game, id: Uuid) -> Option<([f64; 3], f32)> {
    let team = game.roster.team_of(id)?;
    let site = &game.layout.teams[team];
    let index = game
        .roster
        .team(team)
        .iter()
        .position(|m| *m == id)
        .unwrap_or(0);
    Some((site.spawns[index % site.spawns.len()], site.yaw))
}

fn to_cage(game: &Game, player: &Player, id: Uuid) {
    let Some((spot, yaw)) = cage_spot(game, id) else {
        return;
    };
    player.set_gamemode(GameMode::Adventure);
    player.get_inventory().clear_all();
    player.teleport(
        (spot[0], spot[1], spot[2]),
        Some(yaw),
        Some(0.0),
        player.get_world(),
    );
}

fn join(game: &mut Game, player: &Player, id: Uuid) {
    let waiting = matches!(game.phase, Phase::Waiting | Phase::Countdown { .. });
    if waiting && let Some(team) = game.roster.join(id, &player.get_name()) {
        to_cage(game, player, id);
        let subtitle = if game.solo() {
            "§7Last one standing wins".to_owned()
        } else {
            let colour = COLOURS[team % COLOURS.len()];
            format!("§7You're on {}{} §7team", colour.code, colour.name)
        };
        play::title(player, TITLE, &subtitle);
        play::chat(
            player,
            &format!(
                "§bSky Wars §7on §f{}§7. Loot your chests, the middle has better ones. One life: last {} standing wins.",
                game.layout.name,
                if game.solo() { "player" } else { "team" }
            ),
        );
        return;
    }
    play::chat(player, "§7The match is under way; you're spectating.");
    to_spectate(game, player);
}

fn to_spectate(game: &mut Game, player: &Player) {
    let spot = game.layout.spectate;
    player.set_gamemode(GameMode::Spectator);
    player.teleport((spot[0], spot[1], spot[2]), None, None, player.get_world());
    game.falling.remove(&player_id(player));
}

fn leave(game: &mut Game, server: &Server, id: Uuid) {
    game.sidebars.forget(id);
    game.falling.remove(&id);
    game.entities.retain(|_, p| *p != id);
    match game.phase {
        Phase::Waiting | Phase::Countdown { .. } => game.roster.remove(id),
        Phase::Playing { .. } => {
            if game.alive(id) {
                let who = game.display(id);
                game.roster.leave(id);
                server.broadcast(&format!("{who} §7left the match."));
                check_end(game, server);
            } else {
                game.roster.leave(id);
            }
        }
        Phase::Over { .. } => {}
    }
}

fn died(game: &mut Game, server: &Server, id: Uuid, void: bool) {
    if !game.playing() {
        return;
    }
    let now = game.secs();
    let Some(death) = game.roster.death(id, None, now, None) else {
        return;
    };
    let victim = game.display(id);
    let line = match death.killer {
        Some(killer) => {
            let how = if void {
                "was knocked into the void by"
            } else {
                "was slain by"
            };
            format!("{victim} §7{how} {}§7.", game.display(killer))
        }
        None if void => format!("{victim} §7fell into the void."),
        None => format!("{victim} §7died."),
    };
    let left = game.roster.players_in();
    server.broadcast(&format!("{line} §8({left} left)"));
    play::stat(id, "deaths", 1);
    if let Some(killer) = death.killer {
        play::stat(killer, "kills", 1);
    }
    if let Some(player) = server.get_player_by_uuid(play::to_wit(id)) {
        play::title(&player, "§c§lYOU DIED", "§7You're spectating now");
    }
    check_end(game, server);
}

fn check_end(game: &mut Game, server: &Server) {
    if !game.playing() {
        return;
    }
    match game.roster.teams_in().as_slice() {
        [] => end(game, server, None),
        [team] => end(game, server, Some(*team)),
        _ => {}
    }
}

fn end(game: &mut Game, server: &Server, winner: Option<usize>) {
    game.phase = Phase::Over {
        drains: game.secs() + OVER_SECS,
    };
    let headline = match winner {
        Some(team) => format!("{} §6wins!", game.winner_name(team)),
        None => "§7Nobody wins this time.".to_owned(),
    };
    server.broadcast(&format!("§6§l» §r§6Match over! {headline}"));
    let mut best: Vec<(u32, String)> = game
        .roster
        .members()
        .map(|(id, m)| (m.kills, game.display(*id)))
        .filter(|(k, _)| *k > 0)
        .collect();
    best.sort_by(|a, b| b.0.cmp(&a.0));
    for (i, (kills, who)) in best.iter().take(3).enumerate() {
        server.broadcast(&format!("§7 #{} {who} §7· §f{kills} §7kills", i + 1));
    }
    let members: Vec<(Uuid, usize)> = game.roster.members().map(|(id, m)| (*id, m.team)).collect();
    for (id, team) in members {
        let won = winner == Some(team);
        play::stat(id, "games_played", 1);
        play::stat(id, if won { "wins" } else { "losses" }, 1);
        if let Some(player) = server.get_player_by_uuid(play::to_wit(id)) {
            let title = if won {
                "§6§lVICTORY!"
            } else {
                "§c§lGAME OVER"
            };
            play::title(&player, title, &headline);
        }
    }
}

fn begin(game: &mut Game, server: &Server, world: &World) {
    game.phase = Phase::Playing { since: game.secs() };
    play::set_state("in_game");
    let air = generator::block_state("air").unwrap_or(0);
    for site in &game.layout.teams {
        for spawn in &site.spawns {
            for at in build::cage(*spawn) {
                world.set_block_state(block_pos(at), air, BlockFlags::NOTIFY_LISTENERS);
            }
        }
    }
    game.pending = (0..game.chests.len()).collect();
    place_chests(game, world);
    let members: Vec<Uuid> = game.roster.members().map(|(id, _)| *id).collect();
    for id in members {
        if let Some(player) = server.get_player_by_uuid(play::to_wit(id)) {
            player.set_gamemode(GameMode::Survival);
            player.get_inventory().clear_all();
            game.combat.setup(&player);
            play::title(&player, "§a§lGO!", "§7Loot, bridge, fight");
        }
    }
    server.broadcast("§a§lThe cages are open! §r§7Chests refill at 4:00 and 8:00.");
}

/// Places and fills the chests still pending; ones in chunks that aren't
/// loaded yet are tried again next second.
fn place_chests(game: &mut Game, world: &World) {
    let Some(chest) = generator::block_state("chest") else {
        return;
    };
    let pending = std::mem::take(&mut game.pending);
    for index in pending {
        let at = game.chests[index].at;
        if world.get_block_state_id(block_pos(at)) != chest {
            world.set_block_state(block_pos(at), chest, BlockFlags::NOTIFY_LISTENERS);
        }
        if !fill(game, world, index) {
            game.pending.push(index);
        }
    }
}

/// Fills chest `index` afresh; false if it isn't there (yet).
fn fill(game: &mut Game, world: &World, index: usize) -> bool {
    let Chest { at, loot, must } = game.chests[index].clone();
    let Some(BlockEntityType::ChestBlockEntity(chest)) = world.get_block_entity(block_pos(at))
    else {
        return false;
    };
    let container = chest.get_container();
    container.clear();
    for (slot, item) in loot::fill(loot, &must, &mut game.rng) {
        container.set_stack(slot, Some(stack(item)));
    }
    true
}

fn stack(item: loot::Item) -> ItemStack {
    let stack = ItemStack::new(item.key, item.count);
    if let Some((enchant, level)) = item.enchant {
        let enchantment = match enchant {
            Enchant::Sharpness => Enchantment::Sharpness,
            Enchant::Protection => Enchantment::Protection,
            Enchant::Power => Enchantment::Power,
            Enchant::Knockback => Enchantment::Knockback,
            Enchant::Efficiency => Enchantment::Efficiency,
        };
        stack.add_enchantment(enchantment, level);
    }
    stack
}

fn second(game: &mut Game, server: &Server, world: &World) {
    let now = game.secs();
    match game.phase {
        Phase::Waiting | Phase::Countdown { .. } => {
            let next = game.start.next(
                game.phase,
                now,
                game.roster.player_count(),
                game.roster.capacity(),
                game.roster.teams_with_players(),
            );
            match (game.phase, next) {
                (Phase::Waiting, Phase::Countdown { starts }) => {
                    server.broadcast(&format!("§eThe cages open in §f{}§e s.", starts - now));
                }
                (Phase::Countdown { .. }, Phase::Waiting) => {
                    server.broadcast("§cNot enough players; waiting for more.");
                }
                _ => {}
            }
            game.phase = next;
            if let Phase::Countdown { starts } = game.phase {
                let left = starts.saturating_sub(now);
                if left == 0 {
                    begin(game, server, world);
                } else if [10, 5, 4, 3, 2, 1].contains(&left) {
                    server.broadcast(&format!("§eCages open in §f{left}§e..."));
                }
            }
        }
        Phase::Playing { since } => {
            if !game.pending.is_empty() {
                place_chests(game, world);
            }
            let played = now - since;
            if game.refilled < REFILLS.len() && played >= REFILLS[game.refilled] {
                game.refilled += 1;
                for index in 0..game.chests.len() {
                    fill(game, world, index);
                }
                server.broadcast("§e§lCHESTS REFILLED! §r§7Every chest has new loot.");
            }
            if !game.border_started && played >= BORDER_SECS {
                game.border_started = true;
                world
                    .get_world_border()
                    .set_size_transition(BORDER_END, BORDER_SHRINK_SECS);
                server.broadcast("§c§lTHE BORDER IS CLOSING! §r§7Head for the middle.");
            }
            if played >= DRAW_SECS {
                end(game, server, None);
            } else {
                check_end(game, server);
            }
        }
        Phase::Over { drains } => {
            if now >= drains && !game.drained {
                game.drained = true;
                server.broadcast("§7Sending you back to the lobby...");
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
            format!(
                "§fCages open in §a{}",
                play::clock(starts.saturating_sub(now))
            )
        }
        Phase::Playing { since } => {
            let played = now - since;
            match REFILLS.get(game.refilled) {
                Some(at) => format!("§fRefill in §e{}", play::clock(at.saturating_sub(played))),
                None if played < BORDER_SECS => {
                    format!("§fBorder in §c{}", play::clock(BORDER_SECS - played))
                }
                None => format!(
                    "§fDraw in §c{}",
                    play::clock(DRAW_SECS.saturating_sub(played))
                ),
            }
        }
        Phase::Over { .. } => "§6Match over".to_owned(),
    };
    let left = if game.solo() {
        format!("§fPlayers left: §a{}", game.roster.players_in())
    } else {
        format!("§fTeams left: §a{}", game.roster.teams_in().len())
    };
    let mode = if game.solo() { "Solo" } else { "Teams" };
    let lines = vec![
        format!("§7{} · {mode}", game.layout.name),
        String::new(),
        status,
        String::new(),
        left,
        String::new(),
    ];
    for player in server.get_all_players() {
        let id = player_id(&player);
        let mut own = lines.clone();
        if let Some(member) = game.roster.get(id) {
            own.push(format!("§fKills: §a{}", member.kills));
            own.push(String::new());
        }
        own.push("§bxena studios".to_owned());
        game.sidebars.show(&player, TITLE, &own);
    }
}

// ---- Events --------------------------------------------------------------

struct OnJoin;
impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _s: Server, event: PlayerJoinEventData) -> PlayerJoinEventData {
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
    fn handle(&self, _s: Server, event: PlayerLeaveEventData) -> PlayerLeaveEventData {
        let id = player_id(&event.player);
        with(|game| game.actions.push(Action::Leave(id)));
        event
    }
}

/// Before the match nothing is built or broken; after the start, anything.
struct OnBreak;
impl EventHandler<BlockBreakEvent> for OnBreak {
    fn handle(&self, _s: Server, mut event: BlockBreakEventData) -> BlockBreakEventData {
        let id = event.player.as_ref().map(player_id);
        with(|game| {
            if !game.playing() || id.is_some_and(|id| !game.alive(id)) {
                event.cancelled = true;
            }
        });
        event
    }
}

struct OnPlace;
impl EventHandler<BlockPlaceEvent> for OnPlace {
    fn handle(&self, _s: Server, mut event: BlockPlaceEventData) -> BlockPlaceEventData {
        let id = player_id(&event.player);
        with(|game| {
            if !game.playing() || !game.alive(id) {
                event.cancelled = true;
            }
        });
        event
    }
}

struct OnDamage;
impl EventHandler<EntityDamageEvent> for OnDamage {
    fn handle(&self, _s: Server, mut event: EntityDamageEventData) -> EntityDamageEventData {
        let entity = event.entity_id;
        with(|game| {
            if let Some(id) = game.entities.get(&entity).copied()
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
        _s: Server,
        mut event: EntityDamageByEntityEventData,
    ) -> EntityDamageByEntityEventData {
        let (victim, attacker) = (event.entity_id, event.damager_id);
        with(|game| {
            let (Some(v), Some(a)) = (
                game.entities.get(&victim).copied(),
                game.entities.get(&attacker).copied(),
            ) else {
                return;
            };
            if game.roster.teammates(v, a) {
                event.cancelled = true;
                return;
            }
            let now = game.secs();
            game.roster.hit(v, a, now);
            event.damage = game.combat.damage(event.damage);
        });
        event
    }
}

struct OnDeath;
impl EventHandler<PlayerDeathEvent> for OnDeath {
    fn handle(&self, _s: Server, mut event: PlayerDeathEventData) -> PlayerDeathEventData {
        let id = player_id(&event.player);
        event.keep_inventory = false;
        with(|game| game.actions.push(Action::Died(id)));
        event
    }
}

struct OnMove;
impl EventHandler<PlayerMoveEvent> for OnMove {
    fn handle(&self, _s: Server, event: PlayerMoveEventData) -> PlayerMoveEventData {
        let y = event.to_position.1;
        let id = player_id(&event.player);
        with(|game| {
            if y < f64::from(game.layout.void_y) && game.falling.insert(id) {
                game.actions.push(Action::Void(id));
            }
        });
        event
    }
}

struct OnChat;
impl EventHandler<PlayerChatEvent> for OnChat {
    fn handle(&self, _s: Server, mut event: PlayerChatEventData) -> PlayerChatEventData {
        if let Some(hook) = play::parse_test_hook(&event.message) {
            let id = player_id(&event.player);
            event.cancelled = true;
            with(|game| game.actions.push(Action::Hook(id, hook)));
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

struct SkyWars;

impl Plugin for SkyWars {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Xena Studios".into()],
            description: "Sky Wars: loot, bridge and fight; last one standing wins.".into(),
            dependencies: vec![],
            permissions: vec![
                permissions::FS_READ_DATA.into(),
                format!("{}{}", permissions::SYS_ENV_PREFIX, play::TEST_HOOKS_VAR),
                format!("{}{}", permissions::SYS_ENV_PREFIX, play::MAP_VAR),
                format!("{}SKYWARS_MODE", permissions::SYS_ENV_PREFIX),
                format!("{}SKYWARS_MIN_PLAYERS", permissions::SYS_ENV_PREFIX),
                format!("{}HALLOW_COMBAT", permissions::SYS_ENV_PREFIX),
            ],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        let mode = std::env::var("SKYWARS_MODE").unwrap_or_else(|_| "solo".into());
        let maps = play::load_maps(&format!("{}/maps", context.get_data_folder()), &mode);
        let wanted = std::env::var(play::MAP_VAR).ok();
        let roll = play::roll();
        let index = play::pick_map(&maps, wanted.as_deref(), roll)
            .ok_or_else(|| format!("skywars: no {mode} maps in the data folder's maps/"))?;
        let (id, map) = &maps[index];
        let layout = map.layout();
        let server = context.get_server();
        let world = generator::overworld(&server).ok_or("skywars: no overworld")?;
        generator::build_into(&world, &layout)?;
        setup_world(&world);
        let mut chests = Vec::new();
        for team in &layout.teams {
            let count = team.chests.len();
            for (i, chest) in team.chests.iter().enumerate() {
                chests.push(Chest {
                    at: chest.at,
                    loot: chest.loot,
                    must: if chest.loot == Loot::Island {
                        loot::island_guarantees(count, i)
                    } else {
                        Vec::new()
                    },
                });
            }
        }
        for chest in &layout.chests {
            chests.push(Chest {
                at: chest.at,
                loot: chest.loot,
                must: Vec::new(),
            });
        }
        let min_players = std::env::var("SKYWARS_MIN_PLAYERS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(MIN_PLAYERS);
        tracing::info!(
            "skywars: {mode} on {} ({id}), {} islands of {}, {} chests",
            layout.name,
            layout.teams.len(),
            layout.team_size,
            chests.len()
        );
        *GAME.lock().unwrap_or_else(|err| err.into_inner()) = Some(Game {
            roster: Roster::new(layout.teams.len(), usize::from(layout.team_size)),
            phase: Phase::Waiting,
            start: Start {
                min_players,
                countdown: COUNTDOWN_SECS,
                full_countdown: FULL_COUNTDOWN_SECS,
            },
            ticks: 0,
            chests,
            pending: Vec::new(),
            refilled: 0,
            border_started: false,
            entities: BTreeMap::new(),
            actions: Vec::new(),
            later: Vec::new(),
            sidebars: Sidebars::default(),
            falling: BTreeSet::new(),
            combat: Combat::from_env(),
            rng: Rng::new(roll),
            drained: false,
            layout,
        });
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
                tracing::warn!("skywars: can't subscribe to {event} events: {err}");
            }
        }
        if play::test_hooks_on() {
            tracing::warn!(
                "skywars: test hooks are on ({}); not for a real network",
                play::TEST_HOOKS_VAR
            );
            context
                .register_event_handler(OnChat, EventPriority::Normal, true)
                .map_err(|err| format!("skywars: can't subscribe to chat: {err}"))?;
        }
        scheduler::schedule_repeating_task(PERIOD, PERIOD, |server| tick(&server));
        Ok(())
    }
}

register_plugin!(SkyWars);

#[cfg(test)]
mod map_tests {
    use hallow_islands::{build::audit, map::Map};

    /// Every map in `maps/` parses, suits its mode (solo maps have teams of
    /// one, teams maps of two, each spawn caged with chests on its island),
    /// and is playable as built.
    #[test]
    fn every_map_is_playable() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../maps");
        let (mut solo, mut teams) = (0, 0);
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            let map = Map::parse(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            let layout = map.layout();
            let name = path.display();
            match map.modes.as_slice() {
                [m] if m == "solo" => {
                    solo += 1;
                    assert_eq!(layout.team_size, 1, "{name}");
                    assert!(layout.teams.len() >= 6, "{name}");
                }
                [m] if m == "teams" => {
                    teams += 1;
                    assert_eq!(layout.team_size, 2, "{name}");
                    for team in &layout.teams {
                        assert_eq!(team.spawns.len(), 2, "{name}: a cage each");
                    }
                }
                other => panic!("{name}: modes must be [\"solo\"] or [\"teams\"], not {other:?}"),
            }
            for team in &layout.teams {
                assert!(team.cage, "{name}");
                assert!(team.chests.len() >= 2, "{name}");
            }
            assert!(layout.chests.len() >= 4, "{name}: the middle's chests");
            let problems = audit(&layout);
            assert!(problems.is_empty(), "{name}: {problems:#?}");
        }
        assert!(
            solo >= 3 && teams >= 2,
            "three solo maps and two teams maps"
        );
    }
}
