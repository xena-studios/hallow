//! Feeds Pumpkin's player events into the [`signals`](crate::signals)
//! collector.
//!
//! Pumpkin handles a player's packets inside the tick, and fires these events
//! from there, waiting for every handler. Each handler here only reads what
//! it needs from the event and the player, then updates a map: no I/O, and no
//! lock held across a call into Pumpkin.
//!
//! What Pumpkin's plugin API (v0.1) can't give us, and so isn't measured:
//!
//! - **Rotation.** `PlayerMoveEvent` fires only when the position changes and
//!   carries no yaw or pitch, and look-only packets fire nothing, so rotation
//!   snaps (aim) can't be seen from here. Vine measures them from packets.
//! - **Ground state.** Neither the move event nor the player exposes
//!   `on_ground`, so air time isn't measured.
//! - **Reach on mobs.** There's no entity lookup by id and no hitbox size, so
//!   reach is measured only when the target is a player.

use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use hallow_proto::signal::PlayerSignals;
use pumpkin_plugin_api::{
    Context, Server,
    events::{
        EventHandler, EventPriority, PlayerAnimationEvent, PlayerInteractEntityEvent,
        PlayerJoinEvent, PlayerMoveEvent,
    },
    events_wit::{
        EntityInteractionAction, PlayerAnimationEventData, PlayerInteractEntityEventData,
        PlayerJoinEventData, PlayerMoveEventData,
    },
    wit::pumpkin::plugin::uuid::Uuid as WitUuid,
};
use uuid::Uuid;

use crate::signals::{self, Collector};

/// Present once [`register`] ran; the handlers do nothing before that.
static COLLECTOR: Mutex<Option<Collector>> = Mutex::new(None);
static CLOCK: OnceLock<Instant> = OnceLock::new();

/// Milliseconds on a monotonic clock that starts with the plugin.
pub fn now_ms() -> u64 {
    let origin = CLOCK.get_or_init(Instant::now);
    u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

pub fn uuid(id: &WitUuid) -> Uuid {
    Uuid::from_u64_pair(id.high, id.low)
}

fn with<R>(f: impl FnOnce(&mut Collector) -> R) -> Option<R> {
    COLLECTOR
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .as_mut()
        .map(f)
}

/// Starts the first window and subscribes to the events. A handler that
/// fails to register is logged and skipped; the rest still run.
pub fn register(context: &Context) {
    *COLLECTOR.lock().unwrap_or_else(|err| err.into_inner()) = Some(Collector::new(now_ms()));
    // Non-blocking and lowest priority: run after any plugin that cancels or
    // rewrites the event, and see what Pumpkin will actually apply.
    let results = [
        (
            "move",
            context.register_event_handler(OnMove, EventPriority::Lowest, false),
        ),
        (
            "animation",
            context.register_event_handler(OnSwing, EventPriority::Lowest, false),
        ),
        (
            "interact-entity",
            context.register_event_handler(OnAttack, EventPriority::Lowest, false),
        ),
        (
            "join",
            context.register_event_handler(OnJoin, EventPriority::Lowest, false),
        ),
    ];
    for (event, result) in results {
        if let Err(err) = result {
            tracing::warn!("hallow-server: can't subscribe to {event} events: {err}");
        }
    }
}

/// Ends the window if it's over and returns its samples. Also refreshes the
/// entity ids from the players online, which runs off the tick.
pub fn take_due(server: &Server) -> Option<Vec<PlayerSignals>> {
    let now = now_ms();
    if !with(|c| c.due(now))? {
        return None;
    }
    let players: Vec<_> = server
        .get_all_players()
        .iter()
        .filter_map(|player| {
            let entity = i32::try_from(player.as_entity().get_id()).ok()?;
            Some((entity, uuid(&player.get_id())))
        })
        .collect();
    let at = unix_ms();
    with(|c| {
        c.retrack(players);
        c.finish(now, at)
    })
}

/// Stops collecting; the unfinished window is dropped.
pub fn stop() {
    COLLECTOR
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .take();
}

struct OnMove;

impl EventHandler<PlayerMoveEvent> for OnMove {
    fn handle(&self, _server: Server, event: PlayerMoveEventData) -> PlayerMoveEventData {
        if !event.cancelled {
            let player = uuid(&event.player.get_id());
            let now = now_ms();
            with(|c| c.moved(player, now, event.from_position, event.to_position));
        }
        event
    }
}

struct OnSwing;

impl EventHandler<PlayerAnimationEvent> for OnSwing {
    fn handle(&self, _server: Server, event: PlayerAnimationEventData) -> PlayerAnimationEventData {
        // Left clicks swing the main hand; off-hand swings come from using
        // an item, and LeaveBed isn't a swing.
        if !event.cancelled && event.animation_type == "ArmSwingMain" {
            let player = uuid(&event.player.get_id());
            let now = now_ms();
            with(|c| c.swung(player, now));
        }
        event
    }
}

struct OnAttack;

impl EventHandler<PlayerInteractEntityEvent> for OnAttack {
    fn handle(
        &self,
        server: Server,
        event: PlayerInteractEntityEventData,
    ) -> PlayerInteractEntityEventData {
        if event.cancelled || event.action != EntityInteractionAction::Attack {
            return event;
        }
        let player = uuid(&event.player.get_id());
        let now = now_ms();
        // Pumpkin already refuses attacks beyond the interaction range plus
        // one block, so this sees reach up to that limit.
        let reach = with(|c| c.player(event.entity_id))
            .flatten()
            .and_then(|target| {
                let (high, low) = target.as_u64_pair();
                server.get_player_by_uuid(WitUuid { high, low })
            })
            .map(|target| {
                let eye = signals::eye(event.player.get_position(), event.sneaking);
                signals::reach(eye, target.get_position())
            });
        with(|c| c.attacked(player, now, reach));
        event
    }
}

struct OnJoin;

impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, event: PlayerJoinEventData) -> PlayerJoinEventData {
        if let Ok(entity) = i32::try_from(event.player.as_entity().get_id()) {
            let player = uuid(&event.player.get_id());
            with(|c| c.track(entity, player));
        }
        event
    }
}
