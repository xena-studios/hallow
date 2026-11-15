//! Anticheat signals measured at the proxy (docs/ANTICHEAT.md, phase 1).
//!
//! Vine's plugin API has no packet-level hook, so the packet timing metrics
//! the proxy is meant to own (`packets_per_second_max`, `attacks`, `cps_max`,
//! `rotation_snaps`, `swings`) can't be measured yet; docs/ANTICHEAT.md
//! describes the Vine API they need. What the events do show is measured:
//!
//! | Metric | |
//! |---|---|
//! | `chat_messages` | Chat messages sent, muted ones included |
//! | `chat_per_second_max` | Most chat messages within any one second |
//! | `commands` | Commands sent, the proxy's and the servers' |
//! | `commands_per_second_max` | Most commands within any one second |
//! | `plugin_messages` | Plugin messages (custom payloads) the client sent |
//! | `server_switches` | Server changes after the first connect |
//! | `online_seconds` | Time online during the window |
//! | `session_seconds` | Time since login, at the end of the window |
//!
//! Every player online during a window gets one sample when it ends, even
//! with no activity. This module is pure: callers pass the time in, so it is
//! unit-tested without Vine.

use std::collections::{BTreeMap, HashMap, VecDeque};

use hallow_proto::signal::PlayerSignals;
use uuid::Uuid;

/// The span the `*_per_second_max` metrics count over.
const SECOND_MS: u64 = 1000;

/// Counts events and keeps the most seen within any one second.
#[derive(Default)]
struct Rate {
    total: u32,
    /// Event times within the last second.
    recent: VecDeque<u64>,
    max: u32,
}

impl Rate {
    fn record(&mut self, now_ms: u64) {
        self.total += 1;
        while self
            .recent
            .front()
            .is_some_and(|&at| at + SECOND_MS <= now_ms)
        {
            self.recent.pop_front();
        }
        self.recent.push_back(now_ms);
        self.max = self
            .max
            .max(u32::try_from(self.recent.len()).unwrap_or(u32::MAX));
    }

    /// Starts a new window; events from the last second still count towards
    /// its first second.
    fn reset(&mut self) {
        self.total = 0;
        self.max = 0;
    }
}

struct Player {
    protocol: Option<i32>,
    /// When the player logged in (or was first seen).
    joined_ms: u64,
    /// When they left, if they did during this window.
    left_ms: Option<u64>,
    /// Whether they reached a server yet; the first connect isn't a switch.
    on_server: bool,
    chat: Rate,
    commands: Rate,
    plugin_messages: u32,
    server_switches: u32,
}

impl Player {
    fn new(protocol: Option<i32>, now_ms: u64) -> Self {
        Self {
            protocol,
            joined_ms: now_ms,
            left_ms: None,
            on_server: false,
            chat: Rate::default(),
            commands: Rate::default(),
            plugin_messages: 0,
            server_switches: 0,
        }
    }
}

/// Per-player measurements for the current window.
pub struct Recorder {
    window_start_ms: u64,
    players: HashMap<Uuid, Player>,
}

impl Recorder {
    pub fn new(now_ms: u64) -> Self {
        Self {
            window_start_ms: now_ms,
            players: HashMap::new(),
        }
    }

    /// The player for an event, tracked from now if they weren't yet (they
    /// were online before the plugin loaded).
    fn player(&mut self, uuid: Uuid, now_ms: u64) -> &mut Player {
        self.players
            .entry(uuid)
            .or_insert_with(|| Player::new(None, now_ms))
    }

    /// A login was admitted. A player who logs in again within a window keeps
    /// their counts.
    pub fn joined(&mut self, uuid: Uuid, protocol: Option<i32>, now_ms: u64) {
        let player = self
            .players
            .entry(uuid)
            .or_insert_with(|| Player::new(protocol, now_ms));
        if player.left_ms.take().is_some() {
            player.joined_ms = now_ms;
            player.on_server = false;
        }
        if protocol.is_some() {
            player.protocol = protocol;
        }
    }

    pub fn chat(&mut self, uuid: Uuid, now_ms: u64) {
        self.player(uuid, now_ms).chat.record(now_ms);
    }

    pub fn command(&mut self, uuid: Uuid, now_ms: u64) {
        self.player(uuid, now_ms).commands.record(now_ms);
    }

    pub fn plugin_message(&mut self, uuid: Uuid, now_ms: u64) {
        self.player(uuid, now_ms).plugin_messages += 1;
    }

    /// The player reached a server.
    pub fn connected(&mut self, uuid: Uuid, now_ms: u64) {
        let player = self.player(uuid, now_ms);
        if player.on_server {
            player.server_switches += 1;
        }
        player.on_server = true;
    }

    pub fn left(&mut self, uuid: Uuid, now_ms: u64) {
        if let Some(player) = self.players.get_mut(&uuid) {
            player.left_ms.get_or_insert(now_ms);
        }
    }

    /// Marks gone the players who aren't `live` and joined at least `grace_ms`
    /// ago (Vine doesn't always fire a disconnect, for example when the first
    /// connect fails).
    pub fn prune(&mut self, live: &[Uuid], grace_ms: u64, now_ms: u64) {
        for (uuid, player) in &mut self.players {
            if player.left_ms.is_none()
                && now_ms.saturating_sub(player.joined_ms) >= grace_ms
                && !live.contains(uuid)
            {
                player.left_ms = Some(now_ms);
            }
        }
    }

    /// Whether the current window has run `window_ms`.
    pub fn due(&self, window_ms: u64, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.window_start_ms) >= window_ms
    }

    /// Ends the window: one sample per player online during it. Players who
    /// left are forgotten and the rest start the next window at zero.
    pub fn flush(&mut self, now_ms: u64) -> Vec<PlayerSignals> {
        let start = self.window_start_ms;
        let window_ms = u32::try_from(now_ms.saturating_sub(start)).unwrap_or(u32::MAX);
        let mut samples: Vec<PlayerSignals> = self
            .players
            .iter()
            .map(|(&uuid, player)| {
                let end = player.left_ms.unwrap_or(now_ms);
                let online_ms = end.saturating_sub(player.joined_ms.max(start));
                let session_ms = end.saturating_sub(player.joined_ms);
                let metrics = BTreeMap::from([
                    ("chat_messages".to_owned(), f64::from(player.chat.total)),
                    ("chat_per_second_max".to_owned(), f64::from(player.chat.max)),
                    ("commands".to_owned(), f64::from(player.commands.total)),
                    (
                        "commands_per_second_max".to_owned(),
                        f64::from(player.commands.max),
                    ),
                    (
                        "plugin_messages".to_owned(),
                        f64::from(player.plugin_messages),
                    ),
                    (
                        "server_switches".to_owned(),
                        f64::from(player.server_switches),
                    ),
                    ("online_seconds".to_owned(), seconds(online_ms)),
                    ("session_seconds".to_owned(), seconds(session_ms)),
                ]);
                PlayerSignals {
                    player: uuid,
                    at_ms: now_ms,
                    window_ms,
                    protocol: player.protocol,
                    metrics,
                }
            })
            .collect();
        samples.sort_by_key(|sample| sample.player);

        self.players.retain(|_, player| player.left_ms.is_none());
        for player in self.players.values_mut() {
            player.chat.reset();
            player.commands.reset();
            player.plugin_messages = 0;
            player.server_switches = 0;
        }
        self.window_start_ms = now_ms;
        samples
    }
}

/// Milliseconds as seconds, to the tenth.
fn seconds(ms: u64) -> f64 {
    (ms / 100) as f64 / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: u64 = 30_000;

    fn uuid(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    fn metric(sample: &PlayerSignals, name: &str) -> f64 {
        sample.metrics[name]
    }

    #[test]
    fn idle_players_get_a_sample() {
        let mut recorder = Recorder::new(1_000);
        recorder.joined(uuid(1), Some(775), 1_000);
        let samples = recorder.flush(1_000 + WINDOW);
        assert_eq!(samples.len(), 1);
        let sample = &samples[0];
        assert_eq!(sample.player, uuid(1));
        assert_eq!(sample.at_ms, 1_000 + WINDOW);
        assert_eq!(sample.window_ms, 30_000);
        assert_eq!(sample.protocol, Some(775));
        assert_eq!(metric(sample, "chat_messages"), 0.0);
        assert_eq!(metric(sample, "online_seconds"), 30.0);
        assert_eq!(metric(sample, "session_seconds"), 30.0);
    }

    #[test]
    fn counts_chat_and_its_peak_second() {
        let mut recorder = Recorder::new(0);
        recorder.joined(uuid(1), None, 0);
        // Three within 1 s, then two more spread out.
        for at in [100, 500, 1_099, 5_000, 9_000] {
            recorder.chat(uuid(1), at);
        }
        let sample = &recorder.flush(WINDOW)[0];
        assert_eq!(metric(sample, "chat_messages"), 5.0);
        assert_eq!(metric(sample, "chat_per_second_max"), 3.0);
    }

    #[test]
    fn a_second_is_half_open() {
        let mut recorder = Recorder::new(0);
        for at in [0, 1_000, 2_000] {
            recorder.command(uuid(1), at);
        }
        let sample = &recorder.flush(WINDOW)[0];
        assert_eq!(metric(sample, "commands"), 3.0);
        assert_eq!(metric(sample, "commands_per_second_max"), 1.0);
    }

    #[test]
    fn the_first_connect_is_not_a_switch() {
        let mut recorder = Recorder::new(0);
        recorder.joined(uuid(1), None, 0);
        recorder.connected(uuid(1), 500);
        recorder.connected(uuid(1), 10_000);
        recorder.connected(uuid(1), 20_000);
        recorder.plugin_message(uuid(1), 600);
        let sample = &recorder.flush(WINDOW)[0];
        assert_eq!(metric(sample, "server_switches"), 2.0);
        assert_eq!(metric(sample, "plugin_messages"), 1.0);
    }

    #[test]
    fn windows_start_at_zero() {
        let mut recorder = Recorder::new(0);
        recorder.joined(uuid(1), Some(5), 0);
        recorder.chat(uuid(1), 1_000);
        recorder.connected(uuid(1), 1_000);
        recorder.connected(uuid(1), 2_000);
        recorder.flush(WINDOW);
        assert!(!recorder.due(WINDOW, 2 * WINDOW - 1));

        let sample = &recorder.flush(2 * WINDOW)[0];
        assert_eq!(metric(sample, "chat_messages"), 0.0);
        assert_eq!(metric(sample, "chat_per_second_max"), 0.0);
        assert_eq!(metric(sample, "server_switches"), 0.0);
        assert_eq!(metric(sample, "online_seconds"), 30.0);
        assert_eq!(metric(sample, "session_seconds"), 60.0);
        assert_eq!(sample.protocol, Some(5));
        // Still on a server: the next connect is a switch.
        recorder.connected(uuid(1), 2 * WINDOW + 1);
        assert_eq!(
            metric(&recorder.flush(3 * WINDOW)[0], "server_switches"),
            1.0
        );
    }

    #[test]
    fn leavers_get_a_last_sample_then_are_forgotten() {
        let mut recorder = Recorder::new(0);
        recorder.joined(uuid(1), None, 10_000);
        recorder.chat(uuid(1), 11_000);
        recorder.left(uuid(1), 20_000);
        let samples = recorder.flush(WINDOW);
        assert_eq!(samples.len(), 1);
        assert_eq!(metric(&samples[0], "online_seconds"), 10.0);
        assert_eq!(metric(&samples[0], "chat_messages"), 1.0);
        assert!(recorder.flush(2 * WINDOW).is_empty());
    }

    #[test]
    fn rejoining_within_a_window_keeps_counts() {
        let mut recorder = Recorder::new(0);
        recorder.joined(uuid(1), None, 0);
        recorder.chat(uuid(1), 1_000);
        recorder.left(uuid(1), 5_000);
        recorder.joined(uuid(1), Some(47), 20_000);
        let samples = recorder.flush(WINDOW);
        assert_eq!(samples.len(), 1);
        assert_eq!(metric(&samples[0], "chat_messages"), 1.0);
        assert_eq!(metric(&samples[0], "session_seconds"), 10.0);
        assert_eq!(samples[0].protocol, Some(47));
        assert_eq!(recorder.flush(2 * WINDOW).len(), 1);
    }

    #[test]
    fn events_track_players_seen_first_mid_session() {
        let mut recorder = Recorder::new(0);
        recorder.command(uuid(2), 15_000);
        let sample = &recorder.flush(WINDOW)[0];
        assert_eq!(sample.protocol, None);
        assert_eq!(metric(sample, "commands"), 1.0);
        assert_eq!(metric(sample, "online_seconds"), 15.0);
    }

    #[test]
    fn prune_spares_recent_logins() {
        let mut recorder = Recorder::new(0);
        recorder.joined(uuid(1), None, 0);
        recorder.joined(uuid(2), None, 0);
        recorder.joined(uuid(3), None, 25_000);
        recorder.prune(&[uuid(2)], 10_000, 29_000);
        let samples = recorder.flush(WINDOW);
        assert_eq!(samples.len(), 3);
        assert_eq!(metric(&samples[0], "online_seconds"), 29.0);
        // Only the pruned player is gone.
        assert_eq!(recorder.flush(2 * WINDOW).len(), 2);
    }

    #[test]
    fn due_after_a_window() {
        let recorder = Recorder::new(1_000);
        assert!(!recorder.due(WINDOW, 30_999));
        assert!(recorder.due(WINDOW, 31_000));
    }

    #[test]
    fn samples_are_ordered_and_serialise() {
        let mut recorder = Recorder::new(0);
        recorder.joined(uuid(9), None, 0);
        recorder.joined(uuid(3), None, 0);
        let samples = recorder.flush(WINDOW);
        assert_eq!(samples[0].player, uuid(3));
        let batch = hallow_proto::signal::SignalBatch {
            source: "proxy-1".into(),
            samples,
        };
        let json = serde_json::to_string(&batch).unwrap();
        assert!(json.contains("\"chat_per_second_max\":0.0"), "{json}");
    }
}
