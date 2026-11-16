//! Anticheat signals: per-player movement and combat measurements, summed
//! over a window ([`WINDOW_SECONDS`]) and sent to the control plane as one
//! [`SignalBatch`](hallow_proto::signal::SignalBatch). Measurement only:
//! nothing here acts on a player.
//!
//! This module is pure: the event handlers in [`crate::collect`] feed it
//! what Pumpkin reports, with a monotonic clock in milliseconds, and the
//! repeating task takes the finished window. The metric names are listed
//! in the crate README and docs/ANTICHEAT.md.
//!
//! | Metric | From | Meaning |
//! |---|---|---|
//! | `moves` | `PlayerMoveEvent` | Accepted position changes in the window |
//! | `horizontal_speed_max` | `PlayerMoveEvent` | Most horizontal path length in any one second, blocks/s |
//! | `vertical_speed_max` | `PlayerMoveEvent` | Most upward path length in any one second, blocks/s |
//! | `swings` | `PlayerAnimationEvent` | Main-hand arm swings in the window |
//! | `cps_max` | `PlayerAnimationEvent` | Most main-hand swings in any one second |
//! | `attacks` | `PlayerInteractEntityEvent` | Attacks on entities in the window |
//! | `attack_reach_max` | `PlayerInteractEntityEvent` | Longest attack on a player: attacker's eye to the target's hitbox, blocks; only when measured |
//!
//! Speeds and clicks use one-second buckets from the start of the window, not
//! the time between two events: Pumpkin handles a player's packets in the
//! tick, so several moves can land in the same millisecond after a lag spike.

use std::collections::{BTreeMap, HashMap};

use hallow_proto::signal::{PlayerSignals, WINDOW_SECONDS};
use uuid::Uuid;

pub const WINDOW_MS: u64 = WINDOW_SECONDS * 1000;
const BUCKET_MS: u64 = 1000;

/// Vanilla player eye heights and hitbox, in blocks.
const EYE_STANDING: f64 = 1.62;
const EYE_SNEAKING: f64 = 1.27;
const PLAYER_HALF_WIDTH: f64 = 0.3;
const PLAYER_HEIGHT: f64 = 1.8;

pub type Vec3 = (f64, f64, f64);

/// Where a player's eyes are, given their feet.
pub fn eye(feet: Vec3, sneaking: bool) -> Vec3 {
    let height = if sneaking { EYE_SNEAKING } else { EYE_STANDING };
    (feet.0, feet.1 + height, feet.2)
}

/// Distance from `eye` to the nearest point of a standing player's hitbox
/// with its feet at `target`. A sneaking or swimming target's box is lower,
/// so this can come out a little short when hitting one from above.
pub fn reach(eye: Vec3, target: Vec3) -> f64 {
    let axis = |at: f64, min: f64, max: f64| at - at.clamp(min, max);
    let dx = axis(
        eye.0,
        target.0 - PLAYER_HALF_WIDTH,
        target.0 + PLAYER_HALF_WIDTH,
    );
    let dy = axis(eye.1, target.1, target.1 + PLAYER_HEIGHT);
    let dz = axis(
        eye.2,
        target.2 - PLAYER_HALF_WIDTH,
        target.2 + PLAYER_HALF_WIDTH,
    );
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// What happened in the current one-second bucket.
#[derive(Default)]
struct Bucket {
    index: u64,
    horizontal: f64,
    rising: f64,
    swings: u32,
}

/// One player's numbers for the current window.
#[derive(Default)]
struct PlayerWindow {
    moves: u32,
    swings: u32,
    attacks: u32,
    reach_max: Option<f64>,
    horizontal_max: f64,
    rising_max: f64,
    cps_max: u32,
    bucket: Bucket,
}

impl PlayerWindow {
    /// Moves to the bucket `index`, closing the current one if it's older.
    fn bucket(&mut self, index: u64) -> &mut Bucket {
        if index != self.bucket.index {
            self.close_bucket();
            self.bucket = Bucket {
                index,
                ..Bucket::default()
            };
        }
        &mut self.bucket
    }

    fn close_bucket(&mut self) {
        let seconds = BUCKET_MS as f64 / 1000.0;
        self.horizontal_max = self.horizontal_max.max(self.bucket.horizontal / seconds);
        self.rising_max = self.rising_max.max(self.bucket.rising / seconds);
        self.cps_max = self.cps_max.max(self.bucket.swings);
    }

    fn metrics(mut self) -> BTreeMap<String, f64> {
        self.close_bucket();
        let mut metrics = BTreeMap::from([
            ("moves".to_owned(), f64::from(self.moves)),
            (
                "horizontal_speed_max".to_owned(),
                round(self.horizontal_max),
            ),
            ("vertical_speed_max".to_owned(), round(self.rising_max)),
            ("swings".to_owned(), f64::from(self.swings)),
            ("cps_max".to_owned(), f64::from(self.cps_max)),
            ("attacks".to_owned(), f64::from(self.attacks)),
        ]);
        if let Some(reach) = self.reach_max {
            metrics.insert("attack_reach_max".to_owned(), round(reach));
        }
        metrics
    }
}

/// Three decimal places: millimetres are plenty and keep the rows short.
fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

/// Every player's numbers for the current window, and which entity id is
/// which player so an attack on an entity can be measured.
pub struct Collector {
    started_ms: u64,
    players: HashMap<Uuid, PlayerWindow>,
    entities: HashMap<i32, Uuid>,
}

impl Collector {
    pub fn new(now_ms: u64) -> Self {
        Self {
            started_ms: now_ms,
            players: HashMap::new(),
            entities: HashMap::new(),
        }
    }

    fn window(&mut self, player: Uuid, now_ms: u64) -> (&mut PlayerWindow, u64) {
        let index = now_ms.saturating_sub(self.started_ms) / BUCKET_MS;
        (self.players.entry(player).or_default(), index)
    }

    /// A move Pumpkin accepted, from `from` to `to`.
    pub fn moved(&mut self, player: Uuid, now_ms: u64, from: Vec3, to: Vec3) {
        let (window, index) = self.window(player, now_ms);
        window.moves += 1;
        let bucket = window.bucket(index);
        bucket.horizontal += (to.0 - from.0).hypot(to.2 - from.2);
        bucket.rising += (to.1 - from.1).max(0.0);
    }

    /// A main-hand arm swing: a left click, or an attack.
    pub fn swung(&mut self, player: Uuid, now_ms: u64) {
        let (window, index) = self.window(player, now_ms);
        window.swings += 1;
        window.bucket(index).swings += 1;
    }

    /// An attack on an entity; `reach` when the target was a player.
    pub fn attacked(&mut self, player: Uuid, now_ms: u64, reach: Option<f64>) {
        let (window, _) = self.window(player, now_ms);
        window.attacks += 1;
        if let Some(reach) = reach {
            window.reach_max = Some(window.reach_max.map_or(reach, |max| max.max(reach)));
        }
    }

    /// Notes which player an entity id belongs to.
    pub fn track(&mut self, entity: i32, player: Uuid) {
        self.entities.insert(entity, player);
    }

    /// Replaces every entity id with the players online now, which drops
    /// players who left.
    pub fn retrack(&mut self, players: impl IntoIterator<Item = (i32, Uuid)>) {
        self.entities = players.into_iter().collect();
    }

    /// The player with this entity id, if it's one.
    pub fn player(&self, entity: i32) -> Option<Uuid> {
        self.entities.get(&entity).copied()
    }

    /// Whether the window is over.
    pub fn due(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.started_ms) >= WINDOW_MS
    }

    /// Ends the window: one sample per player who did anything in it, and a
    /// new window starts. `at_ms` is the Unix time now.
    pub fn finish(&mut self, now_ms: u64, at_ms: u64) -> Vec<PlayerSignals> {
        let window_ms = u32::try_from(now_ms.saturating_sub(self.started_ms)).unwrap_or(u32::MAX);
        self.started_ms = now_ms;
        let mut samples: Vec<_> = std::mem::take(&mut self.players)
            .into_iter()
            .map(|(player, window)| PlayerSignals {
                player,
                at_ms,
                window_ms,
                // Behind Vine every client looks like the server's version
                // here; Vine reports the real one.
                protocol: None,
                metrics: window.metrics(),
            })
            .collect();
        samples.sort_by_key(|sample| sample.player);
        samples
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALEX: Uuid = Uuid::from_u128(1);
    const STEVE: Uuid = Uuid::from_u128(2);

    fn metric(sample: &PlayerSignals, name: &str) -> f64 {
        sample.metrics[name]
    }

    #[test]
    fn walking_speed_is_per_second() {
        let mut c = Collector::new(0);
        // 4.3 blocks/s on x, split across 20 moves a second for 3 seconds.
        for tick in 0..60_u32 {
            let x = f64::from(tick) * 0.215;
            c.moved(
                ALEX,
                u64::from(tick) * 50,
                (x, 64.0, 0.0),
                (x + 0.215, 64.0, 0.0),
            );
        }
        let samples = c.finish(WINDOW_MS, 1_700_000_000_000);
        assert_eq!(samples.len(), 1);
        let s = &samples[0];
        assert_eq!(s.player, ALEX);
        assert_eq!(s.at_ms, 1_700_000_000_000);
        assert_eq!(s.window_ms, 30_000);
        assert_eq!(s.protocol, None);
        assert_eq!(metric(s, "moves"), 60.0);
        assert!((metric(s, "horizontal_speed_max") - 4.3).abs() < 1e-9);
        assert_eq!(metric(s, "vertical_speed_max"), 0.0);
        assert_eq!(metric(s, "attacks"), 0.0);
        assert!(!s.metrics.contains_key("attack_reach_max"));
    }

    #[test]
    fn bunched_moves_dont_spike() {
        let mut c = Collector::new(0);
        // A lag spike: ten moves of 0.2 blocks handled in the same millisecond.
        for i in 0..10_u32 {
            let x = f64::from(i) * 0.2;
            c.moved(ALEX, 1500, (x, 64.0, 0.0), (x + 0.2, 64.0, 0.0));
        }
        let s = &c.finish(WINDOW_MS, 0)[0];
        assert!((metric(s, "horizontal_speed_max") - 2.0).abs() < 1e-9);
    }

    #[test]
    fn diagonal_and_vertical() {
        let mut c = Collector::new(0);
        c.moved(ALEX, 10, (0.0, 64.0, 0.0), (3.0, 65.0, 4.0));
        // Falling doesn't count towards vertical speed.
        c.moved(ALEX, 20, (3.0, 65.0, 4.0), (3.0, 60.0, 4.0));
        // A later second with a faster climb.
        c.moved(ALEX, 2100, (3.0, 60.0, 4.0), (3.0, 63.5, 4.0));
        let s = &c.finish(WINDOW_MS, 0)[0];
        assert_eq!(metric(s, "horizontal_speed_max"), 5.0);
        assert_eq!(metric(s, "vertical_speed_max"), 3.5);
        assert_eq!(metric(s, "moves"), 3.0);
    }

    #[test]
    fn clicks_per_second() {
        let mut c = Collector::new(0);
        for i in 0..8 {
            c.swung(ALEX, 100 + i * 100);
        }
        for i in 0..14 {
            c.swung(ALEX, 5000 + i * 60);
        }
        c.attacked(ALEX, 5100, None);
        let s = &c.finish(WINDOW_MS, 0)[0];
        assert_eq!(metric(s, "swings"), 22.0);
        assert_eq!(metric(s, "cps_max"), 14.0);
        assert_eq!(metric(s, "attacks"), 1.0);
        assert!(!s.metrics.contains_key("attack_reach_max"));
    }

    #[test]
    fn reach_keeps_the_longest() {
        let mut c = Collector::new(0);
        c.attacked(ALEX, 0, Some(2.5));
        c.attacked(ALEX, 1, Some(3.25));
        c.attacked(ALEX, 2, None);
        c.attacked(ALEX, 3, Some(1.0));
        let s = &c.finish(WINDOW_MS, 0)[0];
        assert_eq!(metric(s, "attacks"), 4.0);
        assert_eq!(metric(s, "attack_reach_max"), 3.25);
    }

    #[test]
    fn reach_to_the_hitbox() {
        let feet = (0.0, 64.0, 0.0);
        // Level, 3 blocks to the target's centre: 2.7 to its side.
        let at = eye(feet, false);
        assert!((reach(at, (3.0, 64.0, 0.0)) - 2.7).abs() < 1e-9);
        // Inside the box's height range, so only horizontal distance counts.
        assert!((reach(eye(feet, true), (0.0, 64.0, 2.3)) - 2.0).abs() < 1e-9);
        // Above the target: vertical distance to the top of its head.
        let above = reach(at, (0.0, 60.0, 0.0));
        assert!((above - (65.62 - 61.8)).abs() < 1e-9);
        // Overlapping boxes.
        assert_eq!(reach(at, feet), 0.0);
    }

    #[test]
    fn windows_reset_and_skip_idle_players() {
        let mut c = Collector::new(1000);
        assert!(!c.due(1000 + WINDOW_MS - 1));
        assert!(c.due(1000 + WINDOW_MS));
        c.swung(STEVE, 2000);
        c.moved(ALEX, 2000, (0.0, 0.0, 0.0), (1.0, 0.0, 0.0));
        let first = c.finish(1000 + WINDOW_MS + 500, 42);
        assert_eq!(
            first.iter().map(|s| s.player).collect::<Vec<_>>(),
            [ALEX, STEVE]
        );
        assert_eq!(first[0].window_ms, 30_500);
        assert!(!c.due(1000 + WINDOW_MS + 501));
        // Nobody did anything in the next window.
        assert!(c.finish(1000 + 2 * WINDOW_MS + 500, 43).is_empty());
    }

    #[test]
    fn tracks_entity_ids() {
        let mut c = Collector::new(0);
        c.track(7, ALEX);
        assert_eq!(c.player(7), Some(ALEX));
        assert_eq!(c.player(8), None);
        c.retrack([(8, STEVE)]);
        assert_eq!(c.player(7), None);
        assert_eq!(c.player(8), Some(STEVE));
    }

    #[test]
    fn metric_names() {
        let mut c = Collector::new(0);
        c.attacked(ALEX, 0, Some(1.0));
        let s = &c.finish(WINDOW_MS, 0)[0];
        assert_eq!(
            s.metrics.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "attack_reach_max",
                "attacks",
                "cps_max",
                "horizontal_speed_max",
                "moves",
                "swings",
                "vertical_speed_max",
            ]
        );
    }
}
