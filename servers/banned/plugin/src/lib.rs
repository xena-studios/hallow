//! The banned lobby's Pumpkin plugin. Banned players are sent here instead
//! of being disconnected; the control plane keeps them here and moves them
//! to the hub when the ban ends or is lifted.
//!
//! - **A closed room.** A small dark room high above the world's spawn,
//!   built on the first join (and again if it is ever missing). Everyone is
//!   put in it in adventure mode, can't be hurt, get hungry, drop items or
//!   fall out, and is invisible to the others, off the tab list, with no
//!   name tags and no pushing (a scoreboard team).
//! - **Why they're here.** On arrival, a title and a chat block: the reason,
//!   who issued it (as the panel shows it), when, when it ends, the
//!   punishment id and how to appeal (`appeal` text and a clickable
//!   `appeal_url`). `/help` and `/appeal` show it again.
//! - **A live countdown.** The action bar, every second: the time left on a
//!   temporary ban, or "permanently banned" and the id. When it runs out:
//!   "Your ban has ended. Sending you to the hub…".
//! - **Nobody to talk to.** Chat is cancelled, and so is every Pumpkin
//!   command except `/help` and `/appeal`. Proxy commands (`/hub`,
//!   `/server`, `/lobby`, `/play`, `/party`) never reach this server; the
//!   proxy and control plane refuse them for banned players.
//!
//! The ban comes from Hallow's server plugin over IPC (`ban <uuid>`; see
//! `hallow_server::bans`), which holds the link to the control plane, so
//! this plugin needs no permissions.
//!
//! Event handlers only note what to do; one repeating task does it, as in
//! the hub's plugin.

mod text;

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use hallow_proto::punishment::BanInfo;
use pumpkin_plugin_api::{
    CollisionRule, Context, NametagVisibility, Player, Plugin, PluginMetadata, ScoreboardTeamExt,
    Server, TeamSettingsBuilder,
    common::GameMode,
    events::{
        EntityDamageEvent, EventHandler, EventPriority, FoodLevelChangeEvent, InventoryClickEvent,
        PlayerChatEvent, PlayerCommandSendEvent, PlayerDropItemEvent, PlayerJoinEvent,
        PlayerLeaveEvent, PlayerMoveEvent, PlayerSwapHandsEvent,
    },
    events_wit::{
        EntityDamageEventData, FoodLevelChangeEventData, InventoryClickEventData,
        PlayerChatEventData, PlayerCommandSendEventData, PlayerDropItemEventData,
        PlayerJoinEventData, PlayerLeaveEventData, PlayerMoveEventData, PlayerSwapHandsEventData,
    },
    ipc::send_ipc_message,
    register_plugin, scheduler,
    text::TextComponent,
    wit::pumpkin::plugin::{common::BlockPos, uuid::Uuid as WitUuid, world::BlockFlags},
};
use uuid::Uuid;

const NAME: &str = "hallow_banned";
const SERVER_PLUGIN: &str = "hallow_server";
/// The task runs every other tick.
const TASK_PERIOD_TICKS: u64 = 2;
/// The countdown and the ban lookups run once a second.
const SECOND_TICKS: u64 = 20;
/// How long after a join the player is put in the room. Pumpkin teleports a
/// joining player itself; a second teleport too soon after can be answered
/// with a kick ("Wrong teleport id"), as the hub found.
const SETUP_DELAY_TICKS: u64 = 20;
/// The room's floor, high above anything the world generates at spawn.
const ROOM_FLOOR_Y: i32 = 200;
/// Half the room's outer width: walls at ±3, so a 5×5 inside.
const ROOM_HALF: i32 = 3;
/// Inside height: floor, three blocks of air, ceiling.
const ROOM_HEIGHT: i32 = 4;
const WALL: &str = "minecraft:polished_blackstone_bricks";
const LIGHT: &str = "minecraft:sea_lantern";
const TEAM: &str = "hallow_banned";

/// What the task does next.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// A player joined: the room, invisibility, the team.
    Setup(Uuid),
    /// `/help` or `/appeal`: the ban again.
    Info(Uuid),
    NoChat(Uuid),
    NoCommand(Uuid),
    /// They got out of the room somehow.
    Rescue(Uuid),
}

/// What we know of one player here.
#[derive(Default)]
struct Seen {
    ban: Option<BanInfo>,
    /// The control plane says they aren't banned (any more).
    lifted: bool,
    /// The arrival title and chat block were shown.
    greeted: bool,
    /// Put in the room.
    set_up: bool,
}

#[derive(Default)]
struct Banned {
    actions: Vec<Action>,
    later: Vec<(u64, Action)>,
    players: BTreeMap<Uuid, Seen>,
    ticks: u64,
}

static STATE: Mutex<Option<Banned>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Banned) -> R) -> Option<R> {
    STATE
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .as_mut()
        .map(f)
}

fn to_uuid(id: &WitUuid) -> Uuid {
    Uuid::from_u64_pair(id.high, id.low)
}

fn to_wit(id: Uuid) -> WitUuid {
    let (high, low) = id.as_u64_pair();
    WitUuid { high, low }
}

fn legacy(text: &str) -> TextComponent {
    TextComponent::from_legacy_string(text)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// What the server plugin knows of a player's ban.
enum Lookup {
    Banned(BanInfo),
    NotBanned,
    /// Still being fetched, or the network can't say right now.
    Unknown,
}

fn look_up(id: Uuid) -> Lookup {
    match send_ipc_message(SERVER_PLUGIN, format!("ban {id}").as_bytes()) {
        Ok(Ok(json)) => match serde_json::from_slice::<BanInfo>(&json) {
            Ok(ban) => Lookup::Banned(ban),
            Err(err) => {
                tracing::warn!("hallow-banned: bad ban for {id}: {err}");
                Lookup::Unknown
            }
        },
        Ok(Err(reply)) if reply == "not banned" => Lookup::NotBanned,
        _ => Lookup::Unknown,
    }
}

/// Runs every [`TASK_PERIOD_TICKS`].
fn tick(server: &Server) {
    let Some((actions, ticks)) = with(|state| {
        state.ticks += TASK_PERIOD_TICKS;
        let now = state.ticks;
        let (due, wait) = std::mem::take(&mut state.later)
            .into_iter()
            .partition::<Vec<_>, _>(|(at, _)| *at <= now);
        state.later = wait;
        let mut actions = std::mem::take(&mut state.actions);
        actions.extend(due.into_iter().map(|(_, action)| action));
        (actions, now)
    }) else {
        return;
    };
    for action in actions {
        let id = match &action {
            Action::Setup(id)
            | Action::Info(id)
            | Action::NoChat(id)
            | Action::NoCommand(id)
            | Action::Rescue(id) => *id,
        };
        let Some(player) = server.get_player_by_uuid(to_wit(id)) else {
            continue;
        };
        match action {
            Action::Setup(_) => setup(&player, id),
            Action::Info(_) => info(&player, id),
            Action::NoChat(_) => player.send_system_message(legacy(text::NO_CHAT), false),
            Action::NoCommand(_) => player.send_system_message(legacy(text::NO_COMMANDS), false),
            Action::Rescue(_) => to_room(&player),
        }
    }
    if ticks.is_multiple_of(SECOND_TICKS) {
        every_second(server);
    }
}

/// Looks up everyone's ban, greets those whose ban just arrived, and
/// redraws the countdown.
fn every_second(server: &Server) {
    let now = now_ms();
    for player in server.get_all_players() {
        let id = to_uuid(&player.get_id());
        let lookup = look_up(id);
        let greet = with(|state| {
            let seen = state.players.entry(id).or_default();
            match lookup {
                Lookup::Banned(ban) => {
                    seen.ban = Some(ban);
                    seen.lifted = false;
                }
                Lookup::NotBanned => seen.lifted = true,
                Lookup::Unknown => {}
            }
            let greet = seen.set_up && !seen.greeted && seen.ban.is_some();
            if greet {
                seen.greeted = true;
            }
            greet
        })
        .unwrap_or(false);
        if greet {
            greet_player(&player, id, now);
        }
        let bar = with(|state| {
            let seen = state.players.get(&id)?;
            Some(match (&seen.ban, seen.lifted) {
                (_, true) => text::LIFTED.to_owned(),
                (Some(ban), false) => text::action_bar(ban, now),
                (None, false) => text::WAITING.to_owned(),
            })
        })
        .flatten();
        if let Some(bar) = bar {
            player.show_actionbar(legacy(&bar));
        }
    }
}

fn greet_player(player: &Player, id: Uuid, now: u64) {
    let Some(ban) = with(|state| state.players.get(&id).and_then(|s| s.ban.clone())).flatten()
    else {
        return;
    };
    player.send_title_animation(10, 80, 20);
    player.show_subtitle(legacy(&format!("§7{}", ban.reason)));
    player.show_title(legacy("§c§lYou are banned"));
    show_ban(player, &ban, now);
}

/// The chat block, with the appeal link if there is one.
fn show_ban(player: &Player, ban: &BanInfo, now: u64) {
    for line in text::ban_lines(ban, now) {
        player.send_system_message(legacy(&line), false);
    }
    if let Some(url) = &ban.appeal_url {
        let link = legacy(&format!("§b§n{url}"))
            .click_open_url(url)
            .hover_show_text(legacy("§7Open the appeal page"));
        player.send_system_message(legacy("§7Appeal at: ").add_child(link), false);
    }
    player.send_system_message(
        legacy("§8§m                                        "),
        false,
    );
}

fn info(player: &Player, id: Uuid) {
    match with(|state| state.players.get(&id).and_then(|s| s.ban.clone())).flatten() {
        Some(ban) => show_ban(player, &ban, now_ms()),
        None => player.send_system_message(legacy(text::WAITING), false),
    }
}

/// The room's centre column: above the world spawn.
fn room_centre(player: &Player) -> (i32, i32) {
    let spawn = player.get_world().get_spawn_location();
    (spawn.pos.x, spawn.pos.z)
}

/// Builds the room if it isn't there. Pumpkin drops block writes to
/// unloaded chunks, so this runs with a player standing at spawn, and is
/// checked again on every join.
fn build_room(player: &Player) {
    let world = player.get_world();
    let (cx, cz) = room_centre(player);
    let floor = BlockPos {
        x: cx,
        y: ROOM_FLOOR_Y,
        z: cz,
    };
    if world.get_block_state_id(floor) != 0 {
        return;
    }
    let flags = BlockFlags::NOTIFY_LISTENERS | BlockFlags::FORCE_STATE;
    let top = ROOM_FLOOR_Y + ROOM_HEIGHT;
    for x in -ROOM_HALF..=ROOM_HALF {
        for z in -ROOM_HALF..=ROOM_HALF {
            for y in ROOM_FLOOR_Y..=top {
                let shell =
                    x.abs() == ROOM_HALF || z.abs() == ROOM_HALF || y == ROOM_FLOOR_Y || y == top;
                if !shell {
                    continue;
                }
                let block = if y == top && x == 0 && z == 0 {
                    LIGHT
                } else {
                    WALL
                };
                let pos = BlockPos {
                    x: cx + x,
                    y,
                    z: cz + z,
                };
                if !world.set_block_by_name(pos, block, flags) {
                    tracing::warn!("hallow-banned: Pumpkin doesn't know {block}");
                }
            }
        }
    }
    tracing::info!("hallow-banned: built the room above ({cx}, {cz})");
}

fn to_room(player: &Player) {
    let (cx, cz) = room_centre(player);
    let world = player.get_world();
    let position = (
        f64::from(cx) + 0.5,
        f64::from(ROOM_FLOOR_Y + 1),
        f64::from(cz) + 0.5,
    );
    player.teleport(position, Some(0.0), Some(0.0), world);
}

/// Everyone is invisible to everyone else here: no name tags, no pushing.
fn join_team(player: &Player) {
    let scoreboard = player.get_world().get_scoreboard();
    let team = scoreboard.get_team_handle(TEAM).unwrap_or_else(|| {
        scoreboard.register_new_team(
            TEAM,
            TeamSettingsBuilder::new()
                .see_friendly_invisibles(false)
                .nametag_visibility(NametagVisibility::Never)
                .collision_rule(CollisionRule::Never)
                .build(),
        )
    });
    team.add_player(&player.get_name());
}

fn setup(player: &Player, id: Uuid) {
    if player.get_gamemode() != GameMode::Adventure {
        player.set_gamemode(GameMode::Adventure);
    }
    build_room(player);
    to_room(player);
    // Hunger and damage are cancelled by the event handlers (setting them
    // from here would fire those events back into this plugin).
    player.set_invulnerable(true);
    player.get_inventory().as_inventory().clear();
    player.as_entity().set_invisible(true);
    player.set_tab_list_listed(false);
    join_team(player);
    player.set_tab_list_header_footer(
        legacy("§c§lBanned"),
        legacy("§7Type §f/appeal §7to see how to appeal."),
    );
    with(|state| state.players.entry(id).or_default().set_up = true);
}

struct OnJoin;

impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, event: PlayerJoinEventData) -> PlayerJoinEventData {
        let id = to_uuid(&event.player.get_id());
        with(|state| {
            state.players.insert(id, Seen::default());
            state
                .later
                .push((state.ticks + SETUP_DELAY_TICKS, Action::Setup(id)));
        });
        event
    }
}

struct OnLeave;

impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, event: PlayerLeaveEventData) -> PlayerLeaveEventData {
        let id = to_uuid(&event.player.get_id());
        with(|state| state.players.remove(&id));
        event
    }
}

/// Nobody hears anybody here.
struct OnChat;

impl EventHandler<PlayerChatEvent> for OnChat {
    fn handle(&self, _server: Server, mut event: PlayerChatEventData) -> PlayerChatEventData {
        event.cancelled = true;
        let id = to_uuid(&event.player.get_id());
        with(|state| state.actions.push(Action::NoChat(id)));
        event
    }
}

/// The command's name, without a slash or arguments, lowercased.
fn command_name(command: &str) -> String {
    command
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

/// Only `/help` and `/appeal`, both answered here.
struct OnCommand;

impl EventHandler<PlayerCommandSendEvent> for OnCommand {
    fn handle(
        &self,
        _server: Server,
        mut event: PlayerCommandSendEventData,
    ) -> PlayerCommandSendEventData {
        event.cancelled = true;
        let id = to_uuid(&event.player.get_id());
        let action = match command_name(&event.command).as_str() {
            "help" | "appeal" | "ban" | "baninfo" => Action::Info(id),
            _ => Action::NoCommand(id),
        };
        with(|state| state.actions.push(action));
        event
    }
}

struct OnDamage;

impl EventHandler<EntityDamageEvent> for OnDamage {
    fn handle(&self, _server: Server, mut event: EntityDamageEventData) -> EntityDamageEventData {
        event.cancelled = true;
        event
    }
}

struct OnHunger;

impl EventHandler<FoodLevelChangeEvent> for OnHunger {
    fn handle(
        &self,
        _server: Server,
        mut event: FoodLevelChangeEventData,
    ) -> FoodLevelChangeEventData {
        event.cancelled = true;
        event
    }
}

struct OnDrop;

impl EventHandler<PlayerDropItemEvent> for OnDrop {
    fn handle(
        &self,
        _server: Server,
        mut event: PlayerDropItemEventData,
    ) -> PlayerDropItemEventData {
        event.cancelled = true;
        event
    }
}

struct OnSwapHands;

impl EventHandler<PlayerSwapHandsEvent> for OnSwapHands {
    fn handle(
        &self,
        _server: Server,
        mut event: PlayerSwapHandsEventData,
    ) -> PlayerSwapHandsEventData {
        event.cancelled = true;
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
        event.cancelled = true;
        event
    }
}

/// Back into the room if they're ever outside it once set up.
struct OnMove;

impl EventHandler<PlayerMoveEvent> for OnMove {
    fn handle(&self, _server: Server, event: PlayerMoveEventData) -> PlayerMoveEventData {
        let y = event.to_position.1;
        let outside =
            !(f64::from(ROOM_FLOOR_Y)..=f64::from(ROOM_FLOOR_Y + ROOM_HEIGHT + 1)).contains(&y);
        if outside {
            let id = to_uuid(&event.player.get_id());
            with(|state| {
                let set_up = state.players.get(&id).is_some_and(|s| s.set_up);
                let queued = state.actions.contains(&Action::Rescue(id));
                if set_up && !queued {
                    state.actions.push(Action::Rescue(id));
                }
            });
        }
        event
    }
}

struct HallowBanned;

impl Plugin for HallowBanned {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Xena Studios".into()],
            description: "The banned lobby: why you're banned, when it ends and how to appeal."
                .into(),
            dependencies: vec![],
            permissions: vec![],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        *STATE.lock().unwrap_or_else(|err| err.into_inner()) = Some(Banned::default());
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
                "chat",
                context.register_event_handler(OnChat, EventPriority::Highest, true),
            ),
            (
                "command",
                context.register_event_handler(OnCommand, EventPriority::Highest, true),
            ),
            (
                "damage",
                context.register_event_handler(OnDamage, EventPriority::Normal, true),
            ),
            (
                "hunger",
                context.register_event_handler(OnHunger, EventPriority::Normal, true),
            ),
            (
                "drop",
                context.register_event_handler(OnDrop, EventPriority::Normal, true),
            ),
            (
                "swap hands",
                context.register_event_handler(OnSwapHands, EventPriority::Normal, true),
            ),
            (
                "inventory click",
                context.register_event_handler(OnClick, EventPriority::Normal, true),
            ),
            (
                "move",
                context.register_event_handler(OnMove, EventPriority::Normal, false),
            ),
        ];
        for (event, result) in results {
            if let Err(err) = result {
                tracing::warn!("hallow-banned: can't subscribe to {event} events: {err}");
            }
        }
        scheduler::schedule_repeating_task(TASK_PERIOD_TICKS, TASK_PERIOD_TICKS, |server| {
            tick(&server);
        });
        tracing::info!("hallow-banned: the banned lobby is ready");
        Ok(())
    }
}

register_plugin!(HallowBanned);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_names() {
        assert_eq!(command_name("appeal"), "appeal");
        assert_eq!(command_name("/Help 2"), "help");
        assert_eq!(command_name("msg bob hi"), "msg");
        assert_eq!(command_name(""), "");
    }
}
