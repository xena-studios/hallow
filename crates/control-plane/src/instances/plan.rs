//! The lifecycle planner: given what the control plane sees, what to create,
//! mark and end. Pure, so every decision is unit tested; [`super`] gathers
//! the inputs and carries the actions out.
//!
//! Per instance:
//! - `starting` becomes `ready` once its server registers, or ends if it
//!   hasn't within [`Settings::start_timeout_ms`];
//! - a `ready` game ends when its server says it's done (`draining` with
//!   nobody on it), when it has been empty for [`Settings::empty_grace_ms`]
//!   after having had players, or when its server unregistered;
//! - `creating`, `ending` and `deleting` are owned by a task; if one stays
//!   longer than [`Settings::stuck_ms`] (its control plane died) the
//!   instance is ended again, and `failed` ones are ended (deleted) after
//!   [`Settings::retry_ms`]. While a kind has a `failed` instance waiting
//!   out that time, no new instance of the kind is created: an unreachable
//!   node or a missing image is retried every `retry_ms`, not every pass.
//!
//! Per game kind: the queue's entries are placed the way the matchmaker
//! would (fullest ready server with room for the whole entry, then into
//! instances on their way up); entries that still don't fit need new
//! instances, packed `players.max` to an instance. On top of that, `warm`
//! unused instances are kept while anyone is online. Unused instances beyond
//! that are ended after [`Settings::idle_ms`]. Lobbies and hubs keep at
//! least `warm` instances, and more when their players fill them past
//! [`LOBBY_FILL_PERCENT`] on average. Nothing goes past `max_instances`.
//!
//! New instances go on the node with the fewest instances (then by name),
//! on its lowest free game port if it has a port range. A node with a slot
//! count (from the fleet manager) takes instances only while their players
//! (`players.max` each) fit; when no node has room, players wait in the
//! queue until the fleet manager adds one.

use std::collections::{BTreeMap, BTreeSet};

use hallow_proto::{
    instance::{InstanceInfo, InstanceState, KindRole, KindSpec},
    server::{ServerInfo, ServerRole, ServerState},
};

use crate::orchestrator::NodeSlot;

/// Timings, in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// A started instance whose server hasn't registered by then is ended.
    pub start_timeout_ms: u64,
    /// A game empty this long after having players is over.
    pub empty_grace_ms: u64,
    /// An unused instance beyond the warm count is ended after this.
    pub idle_ms: u64,
    /// A failed instance is cleaned up (and so retried) after this.
    pub retry_ms: u64,
    /// A task-owned state held this long means the task died.
    pub stuck_ms: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            start_timeout_ms: 120_000,
            empty_grace_ms: 30_000,
            idle_ms: 300_000,
            retry_ms: 10_000,
            stuck_ms: 120_000,
        }
    }
}

/// Everything a plan is made from.
#[derive(Clone, Copy, Debug)]
pub struct World<'a> {
    pub now_ms: u64,
    pub kinds: &'a BTreeMap<String, KindSpec>,
    pub instances: &'a [InstanceInfo],
    pub servers: &'a [ServerInfo],
    /// Each kind's queue entries' sizes (players still online), oldest first.
    pub queued: &'a BTreeMap<String, Vec<u32>>,
    pub players_online: u32,
    pub nodes: &'a [NodeSlot],
    pub settings: &'a Settings,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Start a new instance of `kind`.
    Create {
        kind: String,
        node: String,
        port: Option<u16>,
    },
    /// Its server registered: `starting` → `ready`.
    Ready { name: String },
    /// Bookkeeping for the end-of-game check.
    Track {
        name: String,
        had_players: bool,
        empty_since_ms: Option<u64>,
    },
    /// Stop and delete it.
    End { name: String, reason: String },
}

/// See the module docs. Actions come per instance by name, then creates per
/// kind by name.
#[must_use]
pub fn plan(world: &World<'_>) -> Vec<Action> {
    let mut actions = Vec::new();
    let servers: BTreeMap<&str, &ServerInfo> =
        world.servers.iter().map(|s| (s.name.as_str(), s)).collect();
    let mut instances: Vec<&InstanceInfo> = world.instances.iter().collect();
    instances.sort_by(|a, b| a.name.cmp(&b.name));

    // Instances ended by this plan, and every instance that may still take
    // players after it.
    let mut ended: BTreeSet<&str> = BTreeSet::new();
    for instance in &instances {
        if let Some(action) = instance_action(world, instance, servers.get(instance.name.as_str()))
        {
            if matches!(action, Action::End { .. }) {
                ended.insert(&instance.name);
            }
            actions.push(action);
        }
    }

    let mut load: BTreeMap<&str, u32> = world.nodes.iter().map(|n| (n.name.as_str(), 0)).collect();
    let mut ports: BTreeMap<&str, BTreeSet<u16>> = BTreeMap::new();
    let mut used: BTreeMap<&str, u32> = BTreeMap::new();
    for instance in &instances {
        if let Some(count) = load.get_mut(instance.node.as_str()) {
            *count += 1;
        }
        if instance.state.is_live() && !ended.contains(instance.name.as_str()) {
            *used.entry(&instance.node).or_default() += slots_of(world.kinds, &instance.kind);
        }
        if let Some(port) = instance.port {
            ports.entry(&instance.node).or_default().insert(port);
        }
    }

    let none: Vec<u32> = Vec::new();
    for (kind, spec) in world.kinds {
        let live: Vec<&InstanceInfo> = instances
            .iter()
            .copied()
            .filter(|i| i.kind == *kind && i.state.is_live() && !ended.contains(i.name.as_str()))
            .collect();
        let queued = world.queued.get(kind).unwrap_or(&none);
        let (wanted, surplus) = match spec.role {
            KindRole::Game => game_demand(world, kind, spec, &live, &servers, &ended, queued),
            KindRole::Hub | KindRole::Lobby => lobby_demand(world, spec, &live, &servers),
        };
        for name in surplus {
            actions.push(Action::End {
                name,
                reason: "not needed".into(),
            });
        }
        let backing_off = instances.iter().any(|i| {
            i.kind == *kind && i.state == InstanceState::Failed && !ended.contains(i.name.as_str())
        });
        let room = if backing_off {
            0
        } else {
            spec.scaling
                .max_instances
                .map_or(u32::MAX, |max| max.saturating_sub(live.len() as u32))
        };
        let need = slots_of(world.kinds, kind);
        for _ in 0..wanted.min(room) {
            let Some((node, port)) = pick_node(world.nodes, &load, &used, &ports, need) else {
                break;
            };
            *load.entry(node).or_default() += 1;
            *used.entry(node).or_default() += need;
            if let Some(port) = port {
                ports.entry(node).or_default().insert(port);
            }
            actions.push(Action::Create {
                kind: kind.clone(),
                node: node.to_owned(),
                port,
            });
        }
    }
    actions
}

fn instance_action(
    world: &World<'_>,
    instance: &InstanceInfo,
    server: Option<&&ServerInfo>,
) -> Option<Action> {
    let settings = world.settings;
    let held = world.now_ms.saturating_sub(instance.state_since_ms);
    let end = |reason: &str| {
        Some(Action::End {
            name: instance.name.clone(),
            reason: reason.to_owned(),
        })
    };
    match instance.state {
        InstanceState::Creating | InstanceState::Ending | InstanceState::Deleting => {
            if held > settings.stuck_ms {
                end(&format!("stuck {}", instance.state.as_str()))
            } else {
                None
            }
        }
        InstanceState::Failed => (held > settings.retry_ms).then(|| Action::End {
            name: instance.name.clone(),
            reason: instance.reason.clone().unwrap_or_else(|| "failed".into()),
        }),
        InstanceState::Starting => {
            if server.is_some() {
                Some(Action::Ready {
                    name: instance.name.clone(),
                })
            } else if held > settings.start_timeout_ms {
                end(&format!(
                    "its server didn't register within {} s",
                    settings.start_timeout_ms / 1000
                ))
            } else {
                None
            }
        }
        InstanceState::Ready => {
            let Some(server) = server else {
                return end("its server unregistered");
            };
            let Some(kind) = world.kinds.get(&instance.kind) else {
                return (server.players == 0).then(|| Action::End {
                    name: instance.name.clone(),
                    reason: "its kind was removed".into(),
                });
            };
            if kind.role != KindRole::Game {
                return None;
            }
            if server.state == ServerState::Draining && server.players == 0 {
                return end("its game ended");
            }
            let had_players = instance.had_players || server.players > 0;
            let empty_since_ms = if server.players > 0 || !had_players {
                None
            } else {
                Some(instance.empty_since_ms.unwrap_or(world.now_ms))
            };
            if let Some(since) = empty_since_ms
                && world.now_ms.saturating_sub(since) >= settings.empty_grace_ms
            {
                return end("empty after its game");
            }
            (had_players != instance.had_players || empty_since_ms != instance.empty_since_ms).then(
                || Action::Track {
                    name: instance.name.clone(),
                    had_players,
                    empty_since_ms,
                },
            )
        }
    }
}

/// How many new instances a game kind needs, and which unused ones it can
/// lose.
fn game_demand(
    world: &World<'_>,
    kind: &str,
    spec: &KindSpec,
    live: &[&InstanceInfo],
    servers: &BTreeMap<&str, &ServerInfo>,
    ended: &BTreeSet<&str>,
    queued: &[u32],
) -> (u32, Vec<String>) {
    let max = spec.players.max.max(1);
    // Every server the matchmaker could place on: (name, players, max).
    let ending: BTreeSet<&str> = world
        .instances
        .iter()
        .filter(|i| !i.state.is_live())
        .map(|i| i.name.as_str())
        .chain(ended.iter().copied())
        .collect();
    let mut open: Vec<(&str, u32, u32)> = servers
        .values()
        .filter(|s| {
            s.kind == kind
                && s.role == ServerRole::Game
                && s.state == ServerState::Ready
                && !ending.contains(s.name.as_str())
        })
        .map(|s| (s.name.as_str(), s.players, s.max_players))
        .collect();
    open.sort_by(|a, b| a.0.cmp(b.0));
    // Instances on their way up, as empty bins.
    let mut pending: Vec<u32> = live
        .iter()
        .filter(|i| matches!(i.state, InstanceState::Creating | InstanceState::Starting))
        .map(|_| 0)
        .collect();
    let mut new_bins: Vec<u32> = Vec::new();
    let mut used: BTreeSet<&str> = BTreeSet::new();
    for &size in queued {
        if size == 0 || size > max {
            // Nobody left, or never fits: the matchmaker drops or keeps it.
            continue;
        }
        let target = open
            .iter_mut()
            .filter(|(_, players, cap)| cap.saturating_sub(*players) >= size)
            .max_by_key(|(_, players, _)| *players);
        if let Some((name, players, _)) = target {
            *players += size;
            used.insert(name);
            continue;
        }
        if let Some(bin) = pending
            .iter_mut()
            .chain(new_bins.iter_mut())
            .find(|bin| max - **bin >= size)
        {
            *bin += size;
            continue;
        }
        new_bins.push(size);
    }

    let warm = if world.players_online > 0 {
        spec.scaling.warm
    } else {
        0
    };
    // Unused ready instances, oldest first, and empty pending bins.
    let mut unused: Vec<&InstanceInfo> = live
        .iter()
        .copied()
        .filter(|i| {
            i.state == InstanceState::Ready
                && !i.had_players
                && !used.contains(i.name.as_str())
                && servers
                    .get(i.name.as_str())
                    .is_some_and(|s| s.players == 0 && s.state == ServerState::Ready)
        })
        .collect();
    unused.sort_by_key(|i| (i.state_since_ms, i.name.clone()));
    let idle = unused.len() as u32 + pending.iter().filter(|bin| **bin == 0).count() as u32;
    let wanted = new_bins.len() as u32 + warm.saturating_sub(idle);

    let mut surplus = Vec::new();
    if new_bins.is_empty() && idle > warm {
        let extra = (idle - warm) as usize;
        surplus = unused
            .iter()
            .rev()
            .filter(|i| world.now_ms.saturating_sub(i.state_since_ms) >= world.settings.idle_ms)
            .take(extra)
            .map(|i| i.name.clone())
            .collect();
        surplus.sort();
    }
    (wanted, surplus)
}

/// Lobbies and hubs: at least `warm` instances, and enough that their
/// players fill them to no more than [`LOBBY_FILL_PERCENT`], so a busy
/// lobby gets a new instance before it is full. Empty ones beyond that are
/// ended after the idle time.
fn lobby_demand(
    world: &World<'_>,
    spec: &KindSpec,
    live: &[&InstanceInfo],
    servers: &BTreeMap<&str, &ServerInfo>,
) -> (u32, Vec<String>) {
    let players: u32 = live
        .iter()
        .filter_map(|i| servers.get(i.name.as_str()))
        .map(|s| s.players)
        .sum();
    let fill = (spec.players.max.max(1) * LOBBY_FILL_PERCENT / 100).max(1);
    let target = spec.scaling.warm.max(players.div_ceil(fill));
    let count = live.len() as u32;
    if count < target {
        return (target - count, Vec::new());
    }
    let mut empty: Vec<&InstanceInfo> = live
        .iter()
        .copied()
        .filter(|i| {
            i.state == InstanceState::Ready
                && servers.get(i.name.as_str()).is_some_and(|s| s.players == 0)
                && world.now_ms.saturating_sub(i.state_since_ms) >= world.settings.idle_ms
        })
        .collect();
    empty.sort_by(|a, b| a.name.cmp(&b.name));
    let surplus = empty
        .into_iter()
        .take((count - target) as usize)
        .map(|i| i.name.clone())
        .collect();
    (0, surplus)
}

/// How full a lobby kind's instances get, on average, before another
/// starts.
pub const LOBBY_FILL_PERCENT: u32 = 80;

/// The slots one instance of `kind` takes: its `players.max`.
fn slots_of(kinds: &BTreeMap<String, KindSpec>, kind: &str) -> u32 {
    kinds.get(kind).map_or(0, |k| k.players.max)
}

/// The least loaded node with a free port (if it has a range) and room for
/// `need` more slots (if it has a slot count).
fn pick_node<'a>(
    nodes: &'a [NodeSlot],
    load: &BTreeMap<&str, u32>,
    used: &BTreeMap<&str, u32>,
    ports: &BTreeMap<&str, BTreeSet<u16>>,
    need: u32,
) -> Option<(&'a str, Option<u16>)> {
    let mut best: Option<(u32, &'a str, Option<u16>)> = None;
    for node in nodes {
        let taken = used.get(node.name.as_str()).copied().unwrap_or(0);
        if node.slots.is_some_and(|cap| taken + need > cap) {
            continue;
        }
        let port = match node.ports {
            None => None,
            Some((lo, hi)) => {
                let taken = ports.get(node.name.as_str());
                match (lo..=hi).find(|p| taken.is_none_or(|t| !t.contains(p))) {
                    Some(port) => Some(port),
                    None => continue,
                }
            }
        };
        let count = load.get(node.name.as_str()).copied().unwrap_or(0);
        if best.is_none_or(|(c, name, _)| (count, node.name.as_str()) < (c, name)) {
            best = Some((count, &node.name, port));
        }
    }
    best.map(|(_, name, port)| (name, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hallow_proto::instance::{KindPlayers, KindScaling};
    use uuid::Uuid;

    const NOW: u64 = 1_000_000;

    fn game(warm: u32, max: u32, max_instances: Option<u32>) -> KindSpec {
        KindSpec {
            image: "hallow-test-game:dev".into(),
            role: KindRole::Game,
            lobby: Some("hub".into()),
            players: KindPlayers { min: 1, max },
            scaling: KindScaling {
                warm,
                max_instances,
            },
            env: BTreeMap::new(),
            memory_mb: None,
            updated_by: None,
        }
    }

    fn instance(name: &str, state: InstanceState, since: u64) -> InstanceInfo {
        InstanceInfo {
            name: name.into(),
            kind: "tg".into(),
            uuid: Uuid::nil(),
            image: "img".into(),
            node: "n1".into(),
            state,
            state_since_ms: since,
            created_ms: since,
            address: None,
            port: None,
            backend_id: None,
            players: 0,
            had_players: false,
            empty_since_ms: None,
            reason: None,
        }
    }

    fn server(name: &str, players: u32, state: ServerState) -> ServerInfo {
        ServerInfo {
            name: name.into(),
            kind: "tg".into(),
            version: "dev".into(),
            role: ServerRole::Game,
            address: format!("{name}:25565"),
            max_players: 4,
            players,
            state,
            tick_ms: None,
        }
    }

    fn nodes(names: &[&str]) -> Vec<NodeSlot> {
        names
            .iter()
            .map(|n| NodeSlot {
                name: (*n).into(),
                ports: None,
                slots: None,
            })
            .collect()
    }

    struct Case {
        kinds: BTreeMap<String, KindSpec>,
        instances: Vec<InstanceInfo>,
        servers: Vec<ServerInfo>,
        queued: BTreeMap<String, Vec<u32>>,
        online: u32,
        nodes: Vec<NodeSlot>,
        settings: Settings,
    }

    impl Case {
        fn new(spec: KindSpec) -> Self {
            Self {
                kinds: [("tg".to_owned(), spec)].into(),
                instances: Vec::new(),
                servers: Vec::new(),
                queued: BTreeMap::new(),
                online: 1,
                nodes: nodes(&["n1"]),
                settings: Settings::default(),
            }
        }

        fn queue(mut self, sizes: &[u32]) -> Self {
            self.queued.insert("tg".into(), sizes.to_vec());
            self
        }

        fn plan(&self) -> Vec<Action> {
            plan(&World {
                now_ms: NOW,
                kinds: &self.kinds,
                instances: &self.instances,
                servers: &self.servers,
                queued: &self.queued,
                players_online: self.online,
                nodes: &self.nodes,
                settings: &self.settings,
            })
        }
    }

    fn creates(actions: &[Action]) -> usize {
        actions
            .iter()
            .filter(|a| matches!(a, Action::Create { .. }))
            .count()
    }

    fn ends(actions: &[Action]) -> Vec<(&str, &str)> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::End { name, reason } => Some((name.as_str(), reason.as_str())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_queue_with_no_server_starts_one() {
        let actions = Case::new(game(0, 4, None)).queue(&[1]).plan();
        assert_eq!(
            actions,
            [Action::Create {
                kind: "tg".into(),
                node: "n1".into(),
                port: None
            }]
        );
    }

    #[test]
    fn a_ready_server_with_room_needs_nothing() {
        let mut case = Case::new(game(0, 4, None)).queue(&[2]);
        case.instances = vec![instance("tg-a", InstanceState::Ready, 0)];
        case.instances[0].had_players = true;
        case.servers = vec![server("tg-a", 1, ServerState::Ready)];
        assert_eq!(case.plan(), []);
        // A static server (no instance record) counts too.
        case.instances.clear();
        case.servers = vec![server("game-1", 0, ServerState::Ready)];
        assert_eq!(case.plan(), []);
    }

    #[test]
    fn entries_pack_into_starting_instances_then_new_ones() {
        let mut case = Case::new(game(0, 4, None)).queue(&[3, 2, 2, 1]);
        case.instances = vec![instance("tg-a", InstanceState::Starting, NOW)];
        // 3 → tg-a; 2 → new bin 1; 2 → bin 1 (full); 1 → tg-a (full).
        assert_eq!(creates(&case.plan()), 1);
        // In game servers aren't room.
        case.servers = vec![server("tg-z", 0, ServerState::InGame)];
        assert_eq!(creates(&case.plan()), 1);
    }

    #[test]
    fn a_party_too_big_for_the_room_left_gets_its_own_instance() {
        let mut case = Case::new(game(0, 4, None)).queue(&[2]);
        case.instances = vec![instance("tg-a", InstanceState::Ready, 0)];
        case.servers = vec![server("tg-a", 3, ServerState::Ready)];
        assert_eq!(creates(&case.plan()), 1);
        // A party bigger than any instance never fits: nothing starts for it.
        let case = Case::new(game(0, 4, None)).queue(&[5]);
        assert_eq!(creates(&case.plan()), 0);
    }

    #[test]
    fn warm_instances_while_players_are_online() {
        let mut case = Case::new(game(2, 4, None));
        assert_eq!(creates(&case.plan()), 2);
        case.instances = vec![
            instance("tg-a", InstanceState::Ready, 0),
            instance("tg-b", InstanceState::Creating, NOW),
        ];
        case.servers = vec![server("tg-a", 0, ServerState::Ready)];
        assert_eq!(case.plan(), []);
        // An instance whose game is under way isn't warm.
        case.servers[0].players = 3;
        case.instances[0].had_players = true;
        assert_eq!(creates(&case.plan()), 1);
        // Nobody online: no warm pool.
        case.online = 0;
        assert_eq!(creates(&case.plan()), 0);
    }

    #[test]
    fn unused_instances_beyond_warm_end_after_the_idle_time() {
        let mut case = Case::new(game(1, 4, None));
        case.instances = vec![
            instance("tg-a", InstanceState::Ready, 0),
            instance("tg-b", InstanceState::Ready, NOW - 1000),
        ];
        case.servers = vec![
            server("tg-a", 0, ServerState::Ready),
            server("tg-b", 0, ServerState::Ready),
        ];
        // tg-b hasn't idled long enough; tg-a has.
        assert_eq!(ends(&case.plan()), [("tg-a", "not needed")]);
        // Both have: the newest goes first.
        case.instances[1].state_since_ms = 0;
        assert_eq!(ends(&case.plan()), [("tg-b", "not needed")]);
        // Everyone left: both go.
        case.online = 0;
        assert_eq!(
            ends(&case.plan()),
            [("tg-a", "not needed"), ("tg-b", "not needed")]
        );
        // But not one the queue is about to use.
        let case = Case {
            queued: [("tg".to_owned(), vec![1])].into(),
            ..case
        };
        assert_eq!(ends(&case.plan()), [("tg-a", "not needed")]);
    }

    #[test]
    fn max_instances_caps_creates() {
        let mut case = Case::new(game(0, 2, Some(2))).queue(&[2, 2, 2]);
        assert_eq!(creates(&case.plan()), 2);
        case.instances = vec![instance("tg-a", InstanceState::Starting, NOW)];
        assert_eq!(creates(&case.plan()), 1);
        // Ending instances don't count.
        case.instances
            .push(instance("tg-b", InstanceState::Ending, NOW));
        assert_eq!(creates(&case.plan()), 1);
    }

    #[test]
    fn starting_becomes_ready_or_times_out() {
        let mut case = Case::new(game(0, 4, None));
        case.instances = vec![instance("tg-a", InstanceState::Starting, NOW - 5000)];
        assert_eq!(case.plan(), []);
        case.servers = vec![server("tg-a", 0, ServerState::Ready)];
        assert_eq!(
            case.plan(),
            [Action::Ready {
                name: "tg-a".into()
            }]
        );
        case.servers.clear();
        case.instances[0].state_since_ms = NOW - 121_000;
        assert_eq!(
            ends(&case.plan()),
            [("tg-a", "its server didn't register within 120 s")]
        );
    }

    #[test]
    fn a_game_ends_when_it_empties_after_players() {
        let mut case = Case::new(game(0, 4, None));
        case.online = 0;
        case.instances = vec![instance("tg-a", InstanceState::Ready, 0)];
        case.servers = vec![server("tg-a", 2, ServerState::InGame)];
        assert_eq!(
            case.plan(),
            [Action::Track {
                name: "tg-a".into(),
                had_players: true,
                empty_since_ms: None
            }]
        );
        case.instances[0].had_players = true;
        assert_eq!(case.plan(), []);
        case.servers[0].players = 0;
        assert_eq!(
            case.plan(),
            [Action::Track {
                name: "tg-a".into(),
                had_players: true,
                empty_since_ms: Some(NOW)
            }]
        );
        case.instances[0].empty_since_ms = Some(NOW - 29_000);
        assert_eq!(case.plan(), []);
        case.instances[0].empty_since_ms = Some(NOW - 30_000);
        assert_eq!(ends(&case.plan()), [("tg-a", "empty after its game")]);
        // Someone came back in time: the clock stops.
        case.instances[0].empty_since_ms = Some(NOW - 29_000);
        case.servers[0].players = 1;
        assert_eq!(
            case.plan(),
            [Action::Track {
                name: "tg-a".into(),
                had_players: true,
                empty_since_ms: None
            }]
        );
    }

    #[test]
    fn draining_and_vanished_servers_end() {
        let mut case = Case::new(game(0, 4, None));
        case.instances = vec![
            instance("tg-a", InstanceState::Ready, 0),
            instance("tg-b", InstanceState::Ready, 0),
            instance("tg-c", InstanceState::Ready, 0),
        ];
        case.servers = vec![
            server("tg-a", 0, ServerState::Draining),
            server("tg-b", 1, ServerState::Draining),
        ];
        assert_eq!(
            ends(&case.plan()),
            [
                ("tg-a", "its game ended"),
                ("tg-c", "its server unregistered")
            ]
        );
    }

    #[test]
    fn stuck_and_failed_instances_are_cleaned_up() {
        let mut case = Case::new(game(0, 4, None));
        let mut failed = instance("tg-f", InstanceState::Failed, NOW - 5000);
        failed.reason = Some("no such image".into());
        case.instances = vec![
            instance("tg-a", InstanceState::Creating, NOW - 121_000),
            instance("tg-b", InstanceState::Deleting, NOW - 1000),
            failed,
        ];
        assert_eq!(ends(&case.plan()), [("tg-a", "stuck creating")]);
        case.instances[2].state_since_ms = NOW - 11_000;
        assert_eq!(
            ends(&case.plan()),
            [("tg-a", "stuck creating"), ("tg-f", "no such image")]
        );
    }

    #[test]
    fn a_failure_holds_off_new_instances_until_it_is_cleaned_up() {
        let mut spec = game(1, 50, None);
        spec.role = KindRole::Lobby;
        let mut case = Case::new(spec);
        case.instances = vec![instance("tg-f", InstanceState::Failed, NOW - 5000)];
        // Not every pass: the failure waits out retry_ms first.
        assert_eq!(creates(&case.plan()), 0);
        assert!(ends(&case.plan()).is_empty());
        // Then it is ended, and a new one goes in its place.
        case.instances[0].state_since_ms = NOW - 11_000;
        let actions = case.plan();
        assert_eq!(ends(&actions), [("tg-f", "failed")]);
        assert_eq!(creates(&actions), 1);
    }

    #[test]
    fn an_ending_instance_takes_no_players() {
        let mut case = Case::new(game(0, 4, None)).queue(&[1]);
        case.instances = vec![instance("tg-a", InstanceState::Ending, NOW)];
        case.servers = vec![server("tg-a", 0, ServerState::Ready)];
        assert_eq!(creates(&case.plan()), 1);
    }

    #[test]
    fn removed_kinds_end_once_empty() {
        let mut case = Case::new(game(0, 4, None));
        case.kinds.clear();
        case.instances = vec![
            instance("tg-a", InstanceState::Ready, 0),
            instance("tg-b", InstanceState::Ready, 0),
        ];
        case.servers = vec![
            server("tg-a", 0, ServerState::Ready),
            server("tg-b", 2, ServerState::InGame),
        ];
        assert_eq!(ends(&case.plan()), [("tg-a", "its kind was removed")]);
    }

    #[test]
    fn lobbies_keep_warm_instances() {
        let mut spec = game(2, 50, None);
        spec.role = KindRole::Lobby;
        let mut case = Case::new(spec);
        case.online = 0;
        assert_eq!(creates(&case.plan()), 2);
        case.instances = vec![
            instance("tg-a", InstanceState::Ready, 0),
            instance("tg-b", InstanceState::Ready, 0),
            instance("tg-c", InstanceState::Ready, 0),
        ];
        case.servers = vec![
            server("tg-a", 3, ServerState::Ready),
            server("tg-b", 0, ServerState::Ready),
            server("tg-c", 0, ServerState::Ready),
        ];
        assert_eq!(ends(&case.plan()), [("tg-b", "not needed")]);
        // Lobbies don't end for being empty after players.
        case.instances.truncate(2);
        case.instances[1].had_players = true;
        case.servers.truncate(2);
        assert_eq!(case.plan(), []);
    }

    #[test]
    fn busy_lobbies_scale_on_players() {
        let mut spec = game(1, 10, Some(3));
        spec.role = KindRole::Lobby;
        let mut case = Case::new(spec);
        case.instances = vec![instance("tg-a", InstanceState::Ready, 0)];
        // 8 of 10 is the most an instance fills to on average.
        case.servers = vec![server("tg-a", 8, ServerState::Ready)];
        assert_eq!(creates(&case.plan()), 0);
        case.servers[0].players = 9;
        assert_eq!(creates(&case.plan()), 1);
        case.instances
            .push(instance("tg-b", InstanceState::Starting, NOW));
        assert_eq!(case.plan(), []);
        // Never past max_instances.
        case.servers[0].players = 40;
        assert_eq!(creates(&case.plan()), 1);
        // Quiet again: the empty spare goes after the idle time.
        case.instances[1].state = InstanceState::Ready;
        case.instances[1].state_since_ms = 0;
        case.servers[0].players = 5;
        case.servers.push(server("tg-b", 0, ServerState::Ready));
        assert_eq!(ends(&case.plan()), [("tg-b", "not needed")]);
    }

    #[test]
    fn creates_spread_over_the_least_loaded_nodes_and_free_ports() {
        let mut case = Case::new(game(0, 1, None)).queue(&[1, 1, 1]);
        case.nodes = vec![
            NodeSlot {
                name: "n1".into(),
                ports: Some((30000, 30001)),
                slots: None,
            },
            NodeSlot {
                name: "n2".into(),
                ports: Some((30000, 30000)),
                slots: None,
            },
        ];
        let mut busy = instance("tg-a", InstanceState::Ready, 0);
        busy.port = Some(30000);
        busy.had_players = true;
        case.instances = vec![busy];
        case.servers = vec![server("tg-a", 4, ServerState::InGame)];
        let placed: Vec<(String, Option<u16>)> = case
            .plan()
            .into_iter()
            .filter_map(|a| match a {
                Action::Create { node, port, .. } => Some((node, port)),
                _ => None,
            })
            .collect();
        // n1 has tg-a on 30000; n2 is empty. Then n1:30001; then both are
        // out of ports.
        assert_eq!(
            placed,
            [
                ("n2".to_owned(), Some(30000)),
                ("n1".to_owned(), Some(30001))
            ]
        );
    }

    #[test]
    fn nodes_take_instances_only_while_their_slots_last() {
        // Four players a game; n1 holds 10 slots and already runs one game.
        let mut case = Case::new(game(0, 4, None)).queue(&[4, 4, 4, 4]);
        case.nodes = vec![
            NodeSlot {
                name: "n1".into(),
                ports: None,
                slots: Some(10),
            },
            NodeSlot {
                name: "n2".into(),
                ports: None,
                slots: Some(4),
            },
        ];
        let mut busy = instance("tg-a", InstanceState::Ready, 0);
        busy.had_players = true;
        case.instances = vec![busy];
        case.servers = vec![server("tg-a", 4, ServerState::InGame)];
        let placed: Vec<String> = case
            .plan()
            .into_iter()
            .filter_map(|a| match a {
                Action::Create { node, .. } => Some(node),
                _ => None,
            })
            .collect();
        // n2 takes one (4 of 4), n1 one more (8 of 10); the other two wait.
        assert_eq!(placed, ["n2", "n1"]);
    }
}
