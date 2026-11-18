//! A game mode lobby's plugin (`eggwars-lobby`, `skywars-lobby`): what
//! players see between matches.
//!
//! - **Welcome.** A title with the mode's name, and a chat line of buttons,
//!   one per game kind the lobby queues for, each running `/play <kind>`
//!   (Vine's queue command), plus `/hub`.
//! - **Play item.** A nether star in the middle of the hotbar; right-click
//!   it for the buttons again.
//! - **Safe spawn.** Adventure mode at the world spawn, no damage, no
//!   hunger, the item fixed in place, and back to spawn after a fall.
//!
//! Settings come from the environment, set in the lobby's `hallow.toml`:
//! `LOBBY_TITLE` (`§`-coloured) and `LOBBY_MODES`
//! (`kind=Label|About;...`, see [`modes::parse`]).
//!
//! Buttons are clickable chat because a plugin can't queue a player
//! itself: queueing is Vine's `/play`, and the control plane only takes
//! `POST /v1/queues` from proxies. A menu that queues needs a server-scoped
//! queue route; see the PR notes.

mod modes;

use std::collections::BTreeSet;
use std::sync::Mutex;

use modes::Mode;
use pumpkin_plugin_api::{
    Context, ItemStack, Player, Plugin, PluginMetadata, Server,
    common::GameMode,
    events::{
        EntityDamageEvent, EventHandler, EventPriority, FoodLevelChangeEvent, InventoryClickEvent,
        PlayerDropItemEvent, PlayerInteractEvent, PlayerJoinEvent, PlayerLeaveEvent,
        PlayerMoveEvent, PlayerSwapHandsEvent,
    },
    events_wit::{
        EntityDamageEventData, FoodLevelChangeEventData, InteractAction, InventoryClickEventData,
        PlayerDropItemEventData, PlayerInteractEventData, PlayerJoinEventData,
        PlayerLeaveEventData, PlayerMoveEventData, PlayerSwapHandsEventData,
    },
    permissions, register_plugin, scheduler,
    text::TextComponent,
    wit::pumpkin::plugin::uuid::Uuid as WitUuid,
};
use uuid::Uuid;

const NAME: &str = "hallow_game_lobby";
const PERIOD: u64 = 2;
const SETUP_DELAY: u64 = 20;
const ITEM_SLOT: u8 = 4;
const VOID_Y: f64 = -70.0;

#[derive(Debug, PartialEq, Eq)]
enum Action {
    Setup(Uuid),
    Buttons(Uuid),
    Rescue(Uuid),
}

#[derive(Default)]
struct Lobby {
    title: String,
    modes: Vec<Mode>,
    actions: Vec<Action>,
    later: Vec<(u64, Action)>,
    falling: BTreeSet<Uuid>,
    ticks: u64,
}

static LOBBY: Mutex<Option<Lobby>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Lobby) -> R) -> Option<R> {
    LOBBY
        .try_lock()
        .ok()
        .and_then(|mut guard| guard.as_mut().map(f))
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

fn tick(server: &Server) {
    let Some((actions, title, modes)) = with(|lobby| {
        lobby.ticks += PERIOD;
        let now = lobby.ticks;
        let (due, wait): (Vec<_>, Vec<_>) = std::mem::take(&mut lobby.later)
            .into_iter()
            .partition(|(at, _)| *at <= now);
        lobby.later = wait;
        let mut actions = std::mem::take(&mut lobby.actions);
        actions.extend(due.into_iter().map(|(_, a)| a));
        (actions, lobby.title.clone(), lobby.modes.clone())
    }) else {
        return;
    };
    for action in actions {
        let id = match &action {
            Action::Setup(id) | Action::Buttons(id) | Action::Rescue(id) => *id,
        };
        let Some(player) = server.get_player_by_uuid(to_wit(id)) else {
            continue;
        };
        match action {
            Action::Setup(_) => setup(&player, &title, &modes),
            Action::Buttons(_) => player.send_system_message(buttons(&modes), false),
            Action::Rescue(_) => {
                to_spawn(&player);
                with(|lobby| lobby.falling.remove(&id));
            }
        }
    }
}

fn to_spawn(player: &Player) {
    let world = player.get_world();
    let spawn = world.get_spawn_location();
    let position = (
        f64::from(spawn.pos.x) + 0.5,
        f64::from(spawn.pos.y),
        f64::from(spawn.pos.z) + 0.5,
    );
    player.teleport(position, Some(spawn.yaw), Some(spawn.pitch), world);
}

fn play_item() -> ItemStack {
    let item = ItemStack::new("minecraft:nether_star", 1);
    item.set_custom_name(Some(legacy("§a§lPlay §7(right-click)").italic(false)));
    item.set_lore(vec![
        legacy("§7Pick a mode and join the queue.").italic(false),
    ]);
    item
}

/// "[Solo] [Teams] [Hub]", each a button.
fn buttons(modes: &[Mode]) -> TextComponent {
    let mut line = legacy("§7Play:");
    for mode in modes {
        let hover = if mode.about.is_empty() {
            format!("§eClick to queue for {}", mode.label)
        } else {
            format!("§f{}\n§eClick to queue", mode.about)
        };
        let button = legacy(&format!("§a§l[{}]", mode.label))
            .click_run_command(&format!("/play {}", mode.kind))
            .hover_show_text(legacy(&hover));
        line = line.add_child(legacy(" ")).add_child(button);
    }
    let hub = legacy("§7[Hub]")
        .click_run_command("/hub")
        .hover_show_text(legacy("§7Back to the hub"));
    line.add_child(legacy(" ")).add_child(hub)
}

fn setup(player: &Player, title: &str, modes: &[Mode]) {
    if player.get_gamemode() == GameMode::Survival {
        player.set_gamemode(GameMode::Adventure);
    }
    to_spawn(player);
    player.set_invulnerable(true);
    let inventory = player.get_inventory();
    inventory.clear_all();
    player.set_inventory_item(ITEM_SLOT, Some(play_item()));
    inventory.set_selected_slot(ITEM_SLOT);
    player.send_title_animation(10, 50, 15);
    player.show_subtitle(legacy("§7Pick a mode to play"));
    player.show_title(legacy(title));
    player.send_system_message(legacy(&format!("§7Welcome to the {title} §7lobby!")), false);
    player.send_system_message(buttons(modes), false);
    player.send_system_message(
        legacy("§7Or type §f/play <mode>§7. §f/leavequeue §7to stop queueing."),
        false,
    );
}

fn protected(player: &Player) -> bool {
    !matches!(
        player.get_gamemode(),
        GameMode::Creative | GameMode::Spectator
    )
}

struct OnJoin;
impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _s: Server, event: PlayerJoinEventData) -> PlayerJoinEventData {
        let id = to_uuid(&event.player.get_id());
        with(|lobby| {
            lobby
                .later
                .push((lobby.ticks + SETUP_DELAY, Action::Setup(id)))
        });
        event
    }
}

struct OnLeave;
impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _s: Server, event: PlayerLeaveEventData) -> PlayerLeaveEventData {
        let id = to_uuid(&event.player.get_id());
        with(|lobby| lobby.falling.remove(&id));
        event
    }
}

struct OnInteract;
impl EventHandler<PlayerInteractEvent> for OnInteract {
    fn handle(&self, _s: Server, mut event: PlayerInteractEventData) -> PlayerInteractEventData {
        let right = matches!(
            event.action,
            InteractAction::RightClickAir | InteractAction::RightClickBlock
        );
        if right && event.player.get_selected_slot() == ITEM_SLOT {
            let id = to_uuid(&event.player.get_id());
            with(|lobby| lobby.actions.push(Action::Buttons(id)));
            event.cancelled = true;
        }
        event
    }
}

struct OnClick;
impl EventHandler<InventoryClickEvent> for OnClick {
    fn handle(&self, _s: Server, mut event: InventoryClickEventData) -> InventoryClickEventData {
        if protected(&event.player) {
            event.cancelled = true;
        }
        event
    }
}

struct OnDrop;
impl EventHandler<PlayerDropItemEvent> for OnDrop {
    fn handle(&self, _s: Server, mut event: PlayerDropItemEventData) -> PlayerDropItemEventData {
        if protected(&event.player) {
            event.cancelled = true;
        }
        event
    }
}

struct OnSwap;
impl EventHandler<PlayerSwapHandsEvent> for OnSwap {
    fn handle(&self, _s: Server, mut event: PlayerSwapHandsEventData) -> PlayerSwapHandsEventData {
        if protected(&event.player) {
            event.cancelled = true;
        }
        event
    }
}

struct OnDamage;
impl EventHandler<EntityDamageEvent> for OnDamage {
    fn handle(&self, _s: Server, mut event: EntityDamageEventData) -> EntityDamageEventData {
        event.cancelled = true;
        event
    }
}

struct OnHunger;
impl EventHandler<FoodLevelChangeEvent> for OnHunger {
    fn handle(&self, _s: Server, mut event: FoodLevelChangeEventData) -> FoodLevelChangeEventData {
        event.cancelled = true;
        event
    }
}

struct OnMove;
impl EventHandler<PlayerMoveEvent> for OnMove {
    fn handle(&self, _s: Server, event: PlayerMoveEventData) -> PlayerMoveEventData {
        if event.to_position.1 < VOID_Y {
            let id = to_uuid(&event.player.get_id());
            with(|lobby| {
                if lobby.falling.insert(id) {
                    lobby.actions.push(Action::Rescue(id));
                }
            });
        }
        event
    }
}

struct GameLobby;

impl Plugin for GameLobby {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Xena Studios".into()],
            description: "A game mode lobby: welcome, queue buttons and a safe spawn.".into(),
            dependencies: vec![],
            permissions: vec![
                format!("{}LOBBY_TITLE", permissions::SYS_ENV_PREFIX),
                format!("{}LOBBY_MODES", permissions::SYS_ENV_PREFIX),
            ],
        }
    }

    fn on_load(&self, context: Context) -> Result<(), String> {
        let modes = modes::parse(&std::env::var("LOBBY_MODES").unwrap_or_default());
        if modes.is_empty() {
            tracing::warn!("game-lobby: LOBBY_MODES is empty; the lobby has nothing to queue for");
        }
        *LOBBY.lock().unwrap_or_else(|err| err.into_inner()) = Some(Lobby {
            title: std::env::var("LOBBY_TITLE").unwrap_or_else(|_| "§6§lLOBBY".into()),
            modes,
            ..Lobby::default()
        });
        let results = [
            context.register_event_handler(OnJoin, EventPriority::Normal, false),
            context.register_event_handler(OnLeave, EventPriority::Normal, false),
            context.register_event_handler(OnInteract, EventPriority::Normal, true),
            context.register_event_handler(OnClick, EventPriority::Normal, true),
            context.register_event_handler(OnDrop, EventPriority::Normal, true),
            context.register_event_handler(OnSwap, EventPriority::Normal, true),
            context.register_event_handler(OnDamage, EventPriority::Normal, true),
            context.register_event_handler(OnHunger, EventPriority::Normal, true),
            context.register_event_handler(OnMove, EventPriority::Normal, false),
        ];
        for result in results {
            if let Err(err) = result {
                tracing::warn!("game-lobby: can't subscribe to an event: {err}");
            }
        }
        scheduler::schedule_repeating_task(PERIOD, PERIOD, |server| tick(&server));
        Ok(())
    }
}

register_plugin!(GameLobby);
