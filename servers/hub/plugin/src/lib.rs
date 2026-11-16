//! The hub's Pumpkin plugin: what makes the hub a hub rather than an empty
//! world.
//!
//! - **Game menu.** A compass in the middle of the hotbar. Right-click it
//!   for a chest listing every game mode with how many are playing; click a
//!   mode to go to its lobby. The welcome message lists the modes too, as
//!   clickable text that runs `/lobby <mode>` (Vine's command, which works
//!   from any server).
//! - **Welcome.** A title with the network's player count, and a chat line
//!   with the modes.
//! - **Tab list.** A header and footer with the player count, refreshed
//!   every [`TAB_EVERY_TICKS`].
//! - **Safe spawn.** Everyone joins at the world spawn in adventure mode
//!   (creative and spectator staff keep theirs), can't be hurt or get
//!   hungry, can't drop or move the menu item, and is put back at spawn if
//!   they fall out of the world.
//!
//! The game modes and moving players come from Hallow's server plugin over
//! IPC (`lobbies` and `send <uuid> <mode>`; see `hallow_server::lobbies`),
//! which holds the link to the control plane. The only permission it asks
//! for is `sys.env.HALLOW_SERVER_NAME`, for the tab list.
//!
//! **Not on the tick.** Event handlers only note what to do; one repeating
//! task does it a tick or two later. That keeps handlers short and avoids
//! calling back into a player (opening a chest, teleporting) from inside
//! the event Pumpkin is still handling for them.

mod menu;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hallow_proto::lobby::LobbyOverview;
use pumpkin_plugin_api::{
    Context, ItemStack, Player, Plugin, PluginMetadata, Screen, Server,
    common::GameMode,
    events::{
        EntityDamageEvent, EventHandler, EventPriority, FoodLevelChangeEvent, InventoryClickEvent,
        InventoryCloseEvent, PlayerDropItemEvent, PlayerInteractEvent, PlayerJoinEvent,
        PlayerLeaveEvent, PlayerMoveEvent, PlayerSwapHandsEvent,
    },
    events_wit::{
        EntityDamageEventData, FoodLevelChangeEventData, InteractAction, InventoryClickEventData,
        InventoryCloseEventData, PlayerDropItemEventData, PlayerInteractEventData,
        PlayerJoinEventData, PlayerLeaveEventData, PlayerMoveEventData, PlayerSwapHandsEventData,
    },
    gui::Gui,
    ipc::send_ipc_message,
    permissions, register_plugin, scheduler,
    text::TextComponent,
    wit::pumpkin::plugin::uuid::Uuid as WitUuid,
};
use uuid::Uuid;

const NAME: &str = "hallow_hub";
const SERVER_PLUGIN: &str = "hallow_server";
/// The task runs every other tick.
const TASK_PERIOD_TICKS: u64 = 2;
/// How often the tab list is redrawn.
pub const TAB_EVERY_TICKS: u64 = 100;
/// How long after a join the player is set up.
const SETUP_DELAY_TICKS: u64 = 20;
/// Players within this many blocks of the spawn count as at it.
const SPAWN_RADIUS: f64 = 8.0;
/// Below this height a player has fallen out of the world.
const VOID_Y: f64 = -70.0;
/// The game modes are asked for at most this often.
const OVERVIEW_EVERY: Duration = Duration::from_secs(1);

/// What the task does next.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// A player joined: spawn, items, welcome.
    Setup(Uuid),
    OpenMenu(Uuid),
    /// They clicked a mode in the menu.
    Pick(Uuid, String),
    /// They fell out of the world.
    Rescue(Uuid),
}

#[derive(Default)]
struct Hub {
    actions: Vec<Action>,
    /// Actions that wait until the task has run up to a tick count.
    later: Vec<(u64, Action)>,
    /// Players with the menu open: slot → mode.
    open: BTreeMap<Uuid, BTreeMap<u32, String>>,
    /// Players waiting to be put back at spawn.
    falling: BTreeSet<Uuid>,
    overview: Option<LobbyOverview>,
    asked: Option<Instant>,
    ticks: u64,
    server_name: String,
}

static HUB: Mutex<Option<Hub>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Hub) -> R) -> Option<R> {
    HUB.lock()
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

fn text(legacy: &str) -> TextComponent {
    // Item names and lore are italic unless told otherwise.
    TextComponent::from_legacy_string(legacy).italic(false)
}

/// The game modes, asked of the server plugin at most every
/// [`OVERVIEW_EVERY`]; the last answer if it can't say now.
fn overview() -> Option<LobbyOverview> {
    let due = with(|hub| {
        let due = hub.asked.is_none_or(|at| at.elapsed() >= OVERVIEW_EVERY);
        if due {
            hub.asked = Some(Instant::now());
        }
        due
    })?;
    if due {
        // An error means not fetched yet, or not linked to a control plane.
        if let Ok(Ok(json)) = send_ipc_message(SERVER_PLUGIN, b"lobbies") {
            match serde_json::from_slice::<LobbyOverview>(&json) {
                Ok(fresh) => {
                    with(|hub| hub.overview = Some(fresh));
                }
                Err(err) => tracing::warn!("hallow-hub: bad lobby list: {err}"),
            }
        }
    }
    with(|hub| hub.overview.clone())?
}

/// Runs every [`TASK_PERIOD_TICKS`].
fn tick(server: &Server) {
    let Some((actions, ticks)) = with(|hub| {
        hub.ticks += TASK_PERIOD_TICKS;
        let now = hub.ticks;
        let (due, wait) = std::mem::take(&mut hub.later)
            .into_iter()
            .partition::<Vec<_>, _>(|(at, _)| *at <= now);
        hub.later = wait;
        let mut actions = std::mem::take(&mut hub.actions);
        actions.extend(due.into_iter().map(|(_, action)| action));
        (actions, now)
    }) else {
        return;
    };
    for action in actions {
        let id = match &action {
            Action::Setup(id) | Action::OpenMenu(id) | Action::Pick(id, _) | Action::Rescue(id) => {
                *id
            }
        };
        let Some(player) = server.get_player_by_uuid(to_wit(id)) else {
            continue;
        };
        match action {
            Action::Setup(_) => setup(&player),
            Action::OpenMenu(_) => open_menu(&player, id),
            Action::Pick(_, mode) => pick(&player, id, &mode),
            Action::Rescue(_) => {
                to_spawn(&player, false);
                with(|hub| hub.falling.remove(&id));
            }
        }
    }
    if ticks.is_multiple_of(TAB_EVERY_TICKS) {
        let online = overview().map(|o| o.online);
        for player in server.get_all_players() {
            tab_list(&player, online);
        }
    }
}

fn tab_list(player: &Player, online: Option<u32>) {
    let server = with(|hub| hub.server_name.clone()).unwrap_or_default();
    let (header, footer) = menu::tab_list(online, &server);
    player.set_tab_list_header_footer(
        TextComponent::from_legacy_string(&header),
        TextComponent::from_legacy_string(&footer),
    );
}

/// Puts `player` at the world spawn. With `if_away`, only if they aren't
/// near it already: Pumpkin spawns players there, and a second teleport
/// right behind its own can reach the client before the first is confirmed,
/// which Pumpkin answers with a kick ("Wrong teleport id").
fn to_spawn(player: &Player, if_away: bool) {
    let world = player.get_world();
    let spawn = world.get_spawn_location();
    let position = (
        f64::from(spawn.pos.x) + 0.5,
        f64::from(spawn.pos.y),
        f64::from(spawn.pos.z) + 0.5,
    );
    let (x, y, z) = player.get_position();
    let away = (x - position.0).abs().max((z - position.2).abs()) > SPAWN_RADIUS
        || (y - position.1).abs() > SPAWN_RADIUS;
    if if_away && !away {
        return;
    }
    player.teleport(position, Some(spawn.yaw), Some(spawn.pitch), world);
}

fn menu_item() -> ItemStack {
    let item = ItemStack::new("minecraft:compass", 1);
    item.set_custom_name(Some(text(&format!("§a§l{} §7(right-click)", menu::TITLE))));
    item.set_lore(vec![text("§7Pick a game to play.")]);
    item
}

fn setup(player: &Player) {
    if player.get_gamemode() == GameMode::Survival {
        player.set_gamemode(GameMode::Adventure);
    }
    to_spawn(player, true);
    // Hunger and damage are cancelled by the event handlers. Setting food or
    // health from here would fire those events back into this plugin from
    // inside its own task, which can hang the server (found on the dev network).
    player.set_invulnerable(true);
    let inventory = player.get_inventory();
    inventory.as_inventory().clear();
    player.set_inventory_item(menu::ITEM_SLOT, Some(menu_item()));
    inventory.set_selected_slot(menu::ITEM_SLOT);

    let overview = overview();
    let online = overview.as_ref().map(|o| o.online);
    player.send_title_animation(10, 60, 20);
    player.show_subtitle(TextComponent::from_legacy_string(&match online {
        Some(1) => "§71 player online".to_owned(),
        Some(n) => format!("§7{n} players online"),
        None => "§7Welcome!".to_owned(),
    }));
    player.show_title(TextComponent::from_legacy_string("§6§lXENA STUDIOS"));
    player.send_system_message(
        TextComponent::from_legacy_string(&format!(
            "§6Welcome to Xena Studios, §e{}§6!",
            player.get_name()
        )),
        false,
    );
    player.send_system_message(modes_line(overview.as_ref()), false);
    player.send_system_message(
        TextComponent::from_legacy_string(
            "§7Right-click the §aGame Menu §7compass or type §f/lobby <mode>§7. §f/hub §7brings you back here.",
        ),
        false,
    );
    tab_list(player, online);
}

/// "Games: [Test Game] [Skywars]", each clickable.
fn modes_line(overview: Option<&LobbyOverview>) -> TextComponent {
    let modes = overview.map(|o| o.modes.as_slice()).unwrap_or_default();
    if modes.is_empty() {
        return TextComponent::from_legacy_string("§7No games are up yet.");
    }
    let mut line = TextComponent::from_legacy_string("§7Games:");
    for mode in modes {
        let button =
            TextComponent::from_legacy_string(&format!("§a[{}]", menu::title_case(&mode.name)))
                .click_run_command(&format!("/lobby {}", mode.name))
                .hover_show_text(TextComponent::from_legacy_string(&format!(
                    "§f{} §7playing\n§eClick to play!",
                    mode.players
                )));
        line = line
            .add_child(TextComponent::from_legacy_string(" "))
            .add_child(button);
    }
    line
}

fn open_menu(player: &Player, id: Uuid) {
    let modes = overview().map(|o| o.modes).unwrap_or_default();
    let gui = Gui::new(Screen::Generic9x3, text(&format!("§8{}", menu::TITLE)));
    gui.set_allow_grab_items(false);
    gui.set_allow_put_items(false);
    let entries = menu::layout(&modes);
    if entries.is_empty() {
        let item = ItemStack::new("minecraft:barrier", 1);
        item.set_custom_name(Some(text("§cNo games are up yet")));
        item.set_lore(vec![text("§7Try again in a moment.")]);
        gui.set_item(13, item);
    }
    let mut slots = BTreeMap::new();
    for entry in entries {
        let item = ItemStack::new(entry.icon, 1);
        item.set_custom_name(Some(text(&entry.name)));
        item.set_lore(entry.lore.iter().map(|line| text(line)).collect());
        gui.set_item(entry.slot, item);
        slots.insert(entry.slot, entry.mode);
    }
    with(|hub| hub.open.insert(id, slots));
    player.open_gui(gui);
}

/// Pumpkin's plugin API (v0.1) can't close a player's chest; it closes
/// when the proxy moves them.
fn pick(player: &Player, id: Uuid, mode: &str) {
    with(|hub| hub.open.remove(&id));
    let message = format!("send {id} {mode}");
    if !matches!(
        send_ipc_message(SERVER_PLUGIN, message.as_bytes()),
        Ok(Ok(_))
    ) {
        player.send_system_message(
            TextComponent::from_legacy_string(
                "§cThe network is unavailable right now. Try again in a moment.",
            ),
            false,
        );
    }
}

/// Staff in creative or spectator may move items about.
fn protected(player: &Player) -> bool {
    !matches!(
        player.get_gamemode(),
        GameMode::Creative | GameMode::Spectator
    )
}

struct OnJoin;

impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, event: PlayerJoinEventData) -> PlayerJoinEventData {
        let id = to_uuid(&event.player.get_id());
        // A second after the join, once the client has settled at the
        // position Pumpkin gave it.
        with(|hub| {
            hub.later
                .push((hub.ticks + SETUP_DELAY_TICKS, Action::Setup(id)))
        });
        event
    }
}

struct OnLeave;

impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, event: PlayerLeaveEventData) -> PlayerLeaveEventData {
        let id = to_uuid(&event.player.get_id());
        with(|hub| {
            hub.open.remove(&id);
            hub.falling.remove(&id);
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
        let right_click = matches!(
            event.action,
            InteractAction::RightClickAir | InteractAction::RightClickBlock
        );
        if right_click && event.player.get_selected_slot() == menu::ITEM_SLOT {
            let id = to_uuid(&event.player.get_id());
            with(|hub| hub.actions.push(Action::OpenMenu(id)));
            event.cancelled = true;
        }
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
        let id = to_uuid(&event.player.get_id());
        let picked = with(|hub| {
            let slots = hub.open.get(&id)?;
            Some(
                u32::try_from(event.slot)
                    .ok()
                    .and_then(|slot| slots.get(&slot).cloned()),
            )
        })
        .flatten();
        match picked {
            // The menu is open: nothing moves, and a mode's icon picks it.
            Some(mode) => {
                event.cancelled = true;
                if let Some(mode) = mode {
                    with(|hub| hub.actions.push(Action::Pick(id, mode)));
                }
            }
            None => {
                if protected(&event.player) {
                    event.cancelled = true;
                }
            }
        }
        event
    }
}

struct OnClose;

impl EventHandler<InventoryCloseEvent> for OnClose {
    fn handle(&self, _server: Server, event: InventoryCloseEventData) -> InventoryCloseEventData {
        let id = to_uuid(&event.player.get_id());
        with(|hub| hub.open.remove(&id));
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
        if protected(&event.player) {
            event.cancelled = true;
        }
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
        if protected(&event.player) {
            event.cancelled = true;
        }
        event
    }
}

/// Nothing gets hurt in the hub.
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

struct OnMove;

impl EventHandler<PlayerMoveEvent> for OnMove {
    fn handle(&self, _server: Server, event: PlayerMoveEventData) -> PlayerMoveEventData {
        if event.to_position.1 < VOID_Y {
            let id = to_uuid(&event.player.get_id());
            with(|hub| {
                if hub.falling.insert(id) {
                    hub.actions.push(Action::Rescue(id));
                }
            });
        }
        event
    }
}

struct HallowHub;

impl Plugin for HallowHub {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Xena Studios".into()],
            description: "The hub: game menu, welcome, tab list and a safe spawn.".into(),
            dependencies: vec![],
            permissions: vec![format!("{}HALLOW_SERVER_NAME", permissions::SYS_ENV_PREFIX)],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        *HUB.lock().unwrap_or_else(|err| err.into_inner()) = Some(Hub {
            // Pre-approved in hallow.toml; the instance's name, such as `hub-3fa2c1`.
            server_name: std::env::var("HALLOW_SERVER_NAME").unwrap_or_default(),
            ..Hub::default()
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
                "interact",
                context.register_event_handler(OnInteract, EventPriority::Normal, true),
            ),
            (
                "inventory click",
                context.register_event_handler(OnClick, EventPriority::Normal, true),
            ),
            (
                "inventory close",
                context.register_event_handler(OnClose, EventPriority::Normal, false),
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
                "damage",
                context.register_event_handler(OnDamage, EventPriority::Normal, true),
            ),
            (
                "hunger",
                context.register_event_handler(OnHunger, EventPriority::Normal, true),
            ),
            (
                "move",
                context.register_event_handler(OnMove, EventPriority::Normal, false),
            ),
        ];
        for (event, result) in results {
            if let Err(err) = result {
                tracing::warn!("hallow-hub: can't subscribe to {event} events: {err}");
            }
        }
        scheduler::schedule_repeating_task(TASK_PERIOD_TICKS, TASK_PERIOD_TICKS, |server| {
            tick(&server);
        });
        tracing::info!("hallow-hub: the game menu is ready");
        Ok(())
    }
}

register_plugin!(HallowHub);
