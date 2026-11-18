//! The match: settings, the game's callbacks, and what the SDK does on its
//! own (lifecycle, teams, kits, sidebars, spectating, stats).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use pumpkin_plugin_api::{
    Context, Player, Server, World,
    common::GameMode,
    events::{
        EntityDamageByEntityEvent, EntityDamageEvent, EventHandler, EventPriority,
        FoodLevelChangeEvent, PlayerChatEvent, PlayerDeathEvent, PlayerJoinEvent, PlayerLeaveEvent,
        PlayerMoveEvent,
    },
    events_wit::{
        EntityDamageByEntityEventData, EntityDamageEventData, FoodLevelChangeEventData,
        PlayerChatEventData, PlayerDeathEventData, PlayerJoinEventData, PlayerLeaveEventData,
        PlayerMoveEventData,
    },
    scheduler,
    text::TextComponent,
    wit::pumpkin::plugin::uuid::Uuid as WitUuid,
    worldgen::GeneratorManager,
};
use uuid::Uuid;

use crate::arena::{self, Arena, Pick, Point};
use crate::kit::Kit;
use crate::lifecycle::{Lifecycle, Phase, Step};
use crate::network::{self, State};
use crate::sidebar::{self, Shown};
use crate::stats;
use crate::teams::{self, Team};

/// The SDK's task runs every this many ticks (a quarter second).
const TASK_TICKS: u64 = 5;
const TICKS_PER_SECOND: u64 = 20;
/// Wait this long after a join or respawn before moving a player, so the
/// client has confirmed the position Pumpkin gave it. Pumpkin kicks a client
/// that confirms a stale teleport ("Wrong teleport id") until
/// Pumpkin-MC/Pumpkin#4540 lands.
const SETTLE_TICKS: u64 = 20;
/// A hit counts towards a kill for this long.
const KILL_CREDIT_TICKS: u64 = 10 * TICKS_PER_SECOND;
/// Countdown seconds announced in chat.
const ANNOUNCE: [u64; 8] = [60, 30, 20, 10, 5, 3, 2, 1];

/// How a match runs. [`Settings::new`] reads `hallow.toml`'s values from
/// the environment the base image sets; change the rest in code.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The sidebar's title; legacy `§` codes allowed.
    pub title: String,
    /// Players needed to start (`players.min`, `HALLOW_MIN_PLAYERS`).
    pub min_players: usize,
    /// Players per match (`players.max`, `HALLOW_MAX_PLAYERS`).
    pub max_players: usize,
    /// The countdown once `min_players` are on.
    pub countdown_secs: u64,
    /// The countdown once `max_players` are on.
    pub full_countdown_secs: u64,
    /// The time limit; `None` for none.
    pub length_secs: Option<u64>,
    /// How long the result shows before everyone goes back to the lobby.
    pub ending_secs: u64,
    /// The teams; none for free for all.
    pub teams: Vec<Team>,
    /// The most players per team; the even share at least.
    pub team_size: Option<usize>,
    /// Whether teammates can hurt each other.
    pub friendly_fire: bool,
    /// In game, chat goes to the player's team and `!message` to everyone;
    /// out players chat with each other.
    pub team_chat: bool,
    /// End the match when one team (or player) is left.
    pub last_standing_wins: bool,
    /// Players' game mode while they play.
    pub gamemode: GameMode,
    /// How the map is picked (`maps.pick`, `HALLOW_MAPS_PICK`).
    pub map_pick: Pick,
    /// A chunk generator id (from `GeneratorManager::register`) for chunks
    /// the map doesn't have; `None` leaves them empty.
    pub generator: Option<u32>,
}

/// Environment variables the SDK reads; see [`permissions`](crate::permissions).
pub(crate) const ENV_VARS: [&str; 3] = [
    "HALLOW_MIN_PLAYERS",
    "HALLOW_MAX_PLAYERS",
    "HALLOW_MAPS_PICK",
];

fn env_number(name: &str) -> Option<usize> {
    std::env::var(name).ok()?.trim().parse().ok()
}

impl Settings {
    /// The defaults: `hallow.toml`'s players and map pick from the
    /// environment (2 to 16 players without it), a 10 s countdown (5 s once
    /// full), no time limit, 5 s of ending, free for all, no friendly fire,
    /// team chat, last one standing wins, survival.
    #[must_use]
    pub fn new(title: &str) -> Self {
        Self {
            title: title.to_owned(),
            min_players: env_number(ENV_VARS[0]).unwrap_or(2),
            max_players: env_number(ENV_VARS[1]).unwrap_or(16),
            countdown_secs: 10,
            full_countdown_secs: 5,
            length_secs: None,
            ending_secs: 5,
            teams: Vec::new(),
            team_size: None,
            friendly_fire: false,
            team_chat: true,
            last_standing_wins: true,
            gamemode: GameMode::Survival,
            map_pick: std::env::var(ENV_VARS[2])
                .map(|pick| Pick::parse(&pick))
                .unwrap_or_default(),
            generator: None,
        }
    }
}

/// Why a player is out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cause {
    /// Died, killed by `killer` if another player hit them in the last 10 s.
    Died { killer: Option<Uuid> },
    /// Fell below the map's `void_y`.
    Void { killer: Option<Uuid> },
    /// Left the server.
    Left,
    /// The game put them out, for its own reason.
    Game(String),
}

/// How a match ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A team won (its id).
    Team(String),
    /// A player won (free for all).
    Player(Uuid),
    Draw,
}

/// A game's rules on top of the SDK. Every method has a default, so a game
/// only writes what it changes. Each runs inside the SDK's task, so it may
/// call into Pumpkin freely.
#[allow(unused_variables)]
pub trait Minigame: Send + 'static {
    /// Players are in the arena on their spawns, with their kits.
    fn started(&mut self, game: &mut Match) {}
    /// Once a second while the match runs.
    fn tick(&mut self, game: &mut Match) {}
    /// `player` is out and on their way to spectating.
    fn eliminated(&mut self, game: &mut Match, player: Uuid, cause: &Cause) {}
    /// The time limit passed. By default the last team or player standing
    /// wins, or it's a draw.
    fn time_up(&mut self, game: &Match) -> Outcome {
        game.leader()
    }
    /// The match ended; the SDK has announced it and counted wins.
    fn ended(&mut self, game: &mut Match, outcome: &Outcome) {}
    /// What `player` starts with; `None` for an empty inventory.
    fn kit(&self, game: &Match, player: Uuid) -> Option<Kit> {
        None
    }
    /// Extra sidebar lines for `player`, under the SDK's own.
    fn sidebar(&self, game: &Match, player: Uuid) -> Vec<String> {
        Vec::new()
    }
}

/// Where a player is in the match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// On the server before the match starts.
    Waiting,
    /// In the match.
    Playing,
    /// Was in the match, now spectating.
    Out,
    /// Joined after the start: spectating.
    Watching,
}

#[derive(Clone, Debug)]
struct Member {
    name: String,
    entity: i32,
    online: bool,
    status: Status,
    team: Option<String>,
    /// Who last hit them, and when (in task ticks).
    hit_by: Option<(Uuid, u64)>,
}

#[derive(Debug)]
enum Action {
    /// Ready a waiting player: game mode, empty inventory.
    Prepare(Uuid),
    /// Bring a dead player back, then spectate.
    Respawn(Uuid),
    /// Spectator mode, at the map's spectator point.
    Spectate(Uuid),
}

type Later = Box<dyn FnOnce(&mut Match) + Send>;

/// The match as the game sees it. The game's [`Minigame`] methods get it.
pub struct Match {
    settings: Settings,
    plugin: String,
    server: Option<Server>,
    life: Lifecycle,
    members: BTreeMap<Uuid, Member>,
    preferences: BTreeMap<Uuid, String>,
    arena: Option<Arena>,
    arena_tried: bool,
    void_generator: u32,
    /// Teams (or players) that started the match.
    sides_started: usize,
    outcome: Option<Outcome>,
    ticks: u64,
    actions: Vec<(u64, Action)>,
    /// Put out by events or the game, not yet handled.
    out: Vec<(Uuid, Cause)>,
    /// The game asked to end the match.
    ending: Option<Outcome>,
    shown: BTreeMap<Uuid, Shown>,
}

fn to_uuid(id: &WitUuid) -> Uuid {
    Uuid::from_u64_pair(id.high, id.low)
}

fn to_wit(id: Uuid) -> WitUuid {
    let (high, low) = id.as_u64_pair();
    WitUuid { high, low }
}

fn text(legacy: &str) -> TextComponent {
    TextComponent::from_legacy_string(legacy)
}

impl Match {
    /// The server. Only valid inside the game's callbacks.
    ///
    /// # Panics
    /// Outside the SDK's task, which never hands the match out.
    #[must_use]
    pub fn server(&self) -> &Server {
        self.server
            .as_ref()
            .expect("the server is set while the task runs")
    }

    #[must_use]
    pub const fn settings(&self) -> &Settings {
        &self.settings
    }

    #[must_use]
    pub const fn phase(&self) -> Phase {
        self.life.phase
    }

    /// The map, once loaded. `None` when the repo has no maps.
    #[must_use]
    pub const fn arena(&self) -> Option<&Arena> {
        self.arena.as_ref()
    }

    /// Seconds since the match started.
    #[must_use]
    pub const fn elapsed_secs(&self) -> u64 {
        self.life.played
    }

    /// Seconds left, with a time limit.
    #[must_use]
    pub fn time_left_secs(&self) -> Option<u64> {
        self.life.time_left(&self.settings)
    }

    #[must_use]
    pub fn status(&self, player: Uuid) -> Option<Status> {
        self.members.get(&player).map(|m| m.status)
    }

    /// Everyone who started the match, out or not.
    #[must_use]
    pub fn players(&self) -> Vec<Uuid> {
        self.with_status(&[Status::Playing, Status::Out])
    }

    /// Everyone still in the match.
    #[must_use]
    pub fn alive(&self) -> Vec<Uuid> {
        self.with_status(&[Status::Playing])
    }

    #[must_use]
    pub fn is_alive(&self, player: Uuid) -> bool {
        self.status(player) == Some(Status::Playing)
    }

    fn with_status(&self, statuses: &[Status]) -> Vec<Uuid> {
        self.members
            .iter()
            .filter(|(_, m)| statuses.contains(&m.status))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Everyone on the server now.
    fn online(&self) -> Vec<Uuid> {
        self.members
            .iter()
            .filter(|(_, m)| m.online)
            .map(|(id, _)| *id)
            .collect()
    }

    #[must_use]
    pub fn name(&self, player: Uuid) -> Option<&str> {
        self.members.get(&player).map(|m| m.name.as_str())
    }

    /// `player`'s team, in a team game.
    #[must_use]
    pub fn team(&self, player: Uuid) -> Option<&Team> {
        let id = self.members.get(&player)?.team.as_ref()?;
        self.settings.teams.iter().find(|team| &team.id == id)
    }

    /// The members of team `id`, out or not.
    #[must_use]
    pub fn members(&self, id: &str) -> Vec<Uuid> {
        self.members
            .iter()
            .filter(|(_, m)| m.team.as_deref() == Some(id))
            .map(|(id, _)| *id)
            .collect()
    }

    /// The player, if they're online.
    #[must_use]
    pub fn player(&self, player: Uuid) -> Option<Player> {
        if !self.members.get(&player).is_some_and(|m| m.online) {
            return None;
        }
        self.server().get_player_by_uuid(to_wit(player))
    }

    /// Who last hit `player` in the last 10 s, for kill credit.
    #[must_use]
    pub fn last_attacker(&self, player: Uuid) -> Option<Uuid> {
        let (attacker, at) = self.members.get(&player)?.hit_by?;
        (self.ticks.saturating_sub(at) <= KILL_CREDIT_TICKS).then_some(attacker)
    }

    /// Before the match starts, asks for `player` to play on team `id`.
    /// Kept if the team has room when teams are balanced.
    pub fn prefer_team(&mut self, player: Uuid, id: &str) -> bool {
        let known = self.settings.teams.iter().any(|team| team.id == id);
        if known && matches!(self.life.phase, Phase::Waiting | Phase::Countdown) {
            self.preferences.insert(player, id.to_owned());
        }
        known
    }

    /// Puts `player` out: they spectate from the next task run.
    pub fn eliminate(&mut self, player: Uuid, cause: Cause) {
        if self.is_alive(player) {
            self.out.push((player, cause));
        }
    }

    /// Ends the match with `outcome` after the current callback.
    pub fn end(&mut self, outcome: Outcome) {
        if self.life.phase == Phase::InGame && self.ending.is_none() {
            self.ending = Some(outcome);
        }
    }

    /// Adds `amount` to `player`'s stat `name` for this server kind.
    pub fn add_stat(&self, player: Uuid, name: &str, amount: u32) {
        stats::add(player, name, amount);
    }

    /// A chat line to everyone on the server; legacy `§` codes allowed.
    pub fn broadcast(&self, message: &str) {
        for id in self.online() {
            if let Some(player) = self.player(id) {
                player.send_system_message(text(message), false);
            }
        }
    }

    /// A title (and subtitle) for everyone on the server.
    pub fn title(&self, title: &str, subtitle: &str) {
        for id in self.online() {
            if let Some(player) = self.player(id) {
                show_title(&player, title, subtitle);
            }
        }
    }

    /// The last team or player standing, or a draw.
    #[must_use]
    pub fn leader(&self) -> Outcome {
        let sides = self.sides_alive();
        match sides.into_iter().collect::<Vec<_>>().as_slice() {
            [Side::Team(id)] => Outcome::Team(id.clone()),
            [Side::Player(id)] => Outcome::Player(*id),
            _ => Outcome::Draw,
        }
    }

    fn sides_alive(&self) -> BTreeSet<Side> {
        self.alive().into_iter().map(|id| self.side(id)).collect()
    }

    fn side(&self, player: Uuid) -> Side {
        self.members
            .get(&player)
            .and_then(|m| m.team.clone())
            .map_or(Side::Player(player), Side::Team)
    }

    /// The outcome as a sentence, such as `§cRed§6 won the match!`.
    #[must_use]
    pub fn describe(&self, outcome: &Outcome) -> String {
        match outcome {
            Outcome::Team(id) => {
                let name = self
                    .settings
                    .teams
                    .iter()
                    .find(|team| &team.id == id)
                    .map_or_else(|| id.clone(), Team::colored_name);
                format!("{name}§6 won the match!")
            }
            Outcome::Player(id) => {
                format!("§f{}§6 won the match!", self.name(*id).unwrap_or("?"))
            }
            Outcome::Draw => "§6The match is a draw.".to_owned(),
        }
    }

    fn winners(&self, outcome: &Outcome) -> Vec<Uuid> {
        match outcome {
            Outcome::Team(id) => self
                .members(id)
                .into_iter()
                .filter(|player| self.players().contains(player))
                .collect(),
            Outcome::Player(id) => vec![*id],
            Outcome::Draw => Vec::new(),
        }
    }

    fn later(&mut self, ticks: u64, action: Action) {
        self.actions.push((self.ticks + ticks, action));
    }

    fn spectator_point(&self) -> Option<(Point, World)> {
        let arena = self.arena.as_ref()?;
        Some((arena.map.spectators, arena.world(self.server())?))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Side {
    Team(String),
    Player(Uuid),
}

fn show_title(player: &Player, title: &str, subtitle: &str) {
    player.send_title_animation(5, 40, 10);
    player.show_subtitle(text(subtitle));
    player.show_title(text(title));
}

struct Runtime {
    game: Box<dyn Minigame>,
    m: Match,
}

/// The SDK's state. Taken out while the task runs, so an event Pumpkin
/// fires back into the plugin from a call the task makes finds it empty and
/// lets the event through, instead of waiting on a lock it holds.
static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);
/// Work for the next task run, from commands and other plugins' calls.
static LATER: Mutex<Vec<Later>> = Mutex::new(Vec::new());

fn with<R>(f: impl FnOnce(&mut Match) -> R) -> Option<R> {
    RUNTIME
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .as_mut()
        .map(|runtime| f(&mut runtime.m))
}

/// Runs `f` on the match at the SDK's next task run, a quarter second at
/// most. For commands and anything else outside the game's callbacks.
pub fn later(f: impl FnOnce(&mut Match) + Send + 'static) {
    LATER
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .push(Box::new(f));
}

/// Starts the SDK for the game plugin `plugin` (its name in Pumpkin): call
/// it from `Plugin::on_load`.
pub fn start(
    context: &Context,
    plugin: &str,
    settings: Settings,
    game: impl Minigame,
) -> Result<(), String> {
    let void_generator = GeneratorManager::register(arena::Void);
    *RUNTIME.lock().unwrap_or_else(|err| err.into_inner()) = Some(Runtime {
        game: Box::new(game),
        m: Match {
            settings,
            plugin: plugin.to_owned(),
            server: None,
            life: Lifecycle::default(),
            members: BTreeMap::new(),
            preferences: BTreeMap::new(),
            arena: None,
            arena_tried: false,
            void_generator,
            sides_started: 0,
            outcome: None,
            ticks: 0,
            actions: Vec::new(),
            out: Vec::new(),
            ending: None,
            shown: BTreeMap::new(),
        },
    });
    let register = |result: Result<u32, String>, event: &str| {
        result.map_err(|err| format!("hallow-sdk: can't subscribe to {event}: {err}"))
    };
    register(
        context.register_event_handler(OnJoin, EventPriority::Normal, false),
        "joins",
    )?;
    register(
        context.register_event_handler(OnLeave, EventPriority::Normal, false),
        "leaves",
    )?;
    register(
        context.register_event_handler(OnDamage, EventPriority::High, true),
        "damage",
    )?;
    register(
        context.register_event_handler(OnHit, EventPriority::High, true),
        "hits",
    )?;
    register(
        context.register_event_handler(OnDeath, EventPriority::Normal, false),
        "deaths",
    )?;
    register(
        context.register_event_handler(OnMove, EventPriority::Normal, false),
        "moves",
    )?;
    register(
        context.register_event_handler(OnChat, EventPriority::High, true),
        "chat",
    )?;
    register(
        context.register_event_handler(OnHunger, EventPriority::Normal, true),
        "hunger",
    )?;
    scheduler::schedule_repeating_task(1, TASK_TICKS, run);
    Ok(())
}

fn run(server: Server) {
    let Some(mut runtime) = RUNTIME.lock().unwrap_or_else(|err| err.into_inner()).take() else {
        return;
    };
    runtime.m.server = Some(server);
    runtime.m.ticks += TASK_TICKS;
    runtime.step();
    runtime.m.server = None;
    *RUNTIME.lock().unwrap_or_else(|err| err.into_inner()) = Some(runtime);
}

impl Runtime {
    fn step(&mut self) {
        if !self.m.arena_tried {
            self.m.arena_tried = true;
            self.open_arena();
        }
        let later: Vec<Later> =
            std::mem::take(&mut *LATER.lock().unwrap_or_else(|err| err.into_inner()));
        for f in later {
            f(&mut self.m);
        }
        self.do_actions();
        self.handle_out();
        if self.m.ticks.is_multiple_of(TICKS_PER_SECOND) {
            self.second();
            self.handle_out();
            self.draw_sidebars();
        }
    }

    fn open_arena(&mut self) {
        let maps = arena::maps();
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() ^ u64::from(d.subsec_nanos()));
        let Some(map) = arena::pick(&maps, self.m.settings.map_pick, seed) else {
            tracing::info!("hallow-sdk: no maps; the match plays in the server's world");
            return;
        };
        let generator = self.m.settings.generator.unwrap_or(self.m.void_generator);
        match Arena::open(self.m.server(), &self.m.plugin, map, generator) {
            Ok(arena) => {
                tracing::info!("hallow-sdk: loaded map {} ({})", map.name, map.id);
                self.m.arena = Some(arena);
            }
            Err(err) => tracing::error!("hallow-sdk: can't load map {}: {err}", map.id),
        }
    }

    fn do_actions(&mut self) {
        let now = self.m.ticks;
        let (due, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.m.actions)
            .into_iter()
            .partition(|(at, _)| *at <= now);
        self.m.actions = waiting;
        for (_, action) in due {
            match action {
                Action::Prepare(id) => {
                    if let Some(player) = self.m.player(id)
                        && self.m.status(id) == Some(Status::Waiting)
                    {
                        if !matches!(
                            player.get_gamemode(),
                            GameMode::Creative | GameMode::Spectator
                        ) {
                            player.set_gamemode(GameMode::Adventure);
                        }
                        player.get_inventory().clear_all();
                    }
                }
                Action::Respawn(id) => {
                    if let Some(player) = self.m.player(id) {
                        player.respawn();
                        self.m.later(SETTLE_TICKS, Action::Spectate(id));
                    }
                }
                Action::Spectate(id) => {
                    if let Some(player) = self.m.player(id) {
                        player.get_inventory().clear_all();
                        player.set_gamemode(GameMode::Spectator);
                        if let Some((point, world)) = self.m.spectator_point() {
                            player.teleport_world(
                                world,
                                point.position(),
                                Some(point.yaw),
                                Some(point.pitch),
                            );
                        }
                    }
                }
            }
        }
    }

    /// Players put out since the last run: spectate, tell everyone, ask the
    /// game, and end the match if one side is left.
    fn handle_out(&mut self) {
        let out = std::mem::take(&mut self.m.out);
        for (id, cause) in out {
            let Some(member) = self.m.members.get_mut(&id) else {
                continue;
            };
            if member.status != Status::Playing {
                continue;
            }
            member.status = Status::Out;
            let name = member.name.clone();
            let killer = match &cause {
                Cause::Died { killer } | Cause::Void { killer } => *killer,
                _ => None,
            };
            let message = match (&cause, killer.and_then(|k| self.m.name(k))) {
                (Cause::Left, _) => format!("§7{name} left and is out."),
                (_, Some(killer)) => format!("§7{name} was taken out by {killer}."),
                _ => format!("§7{name} is out."),
            };
            self.m.broadcast(&message);
            match cause {
                Cause::Left => {}
                Cause::Died { .. } => self.m.later(1, Action::Respawn(id)),
                _ => self.m.later(0, Action::Spectate(id)),
            }
            if let Some(player) = self.m.player(id) {
                show_title(&player, "§cYou are out", "§7You're spectating now");
                player.send_system_message(text("§cYou are out. §7You're spectating now."), false);
            }
            self.game.eliminated(&mut self.m, id, &cause);
        }
        self.check_end();
    }

    fn check_end(&mut self) {
        if self.m.life.phase != Phase::InGame {
            return;
        }
        if let Some(outcome) = self.m.ending.take() {
            return self.end(outcome);
        }
        if !self.m.settings.last_standing_wins {
            return;
        }
        let alive = self.m.sides_alive().len();
        if alive == 0 || (self.m.sides_started >= 2 && alive <= 1) {
            let outcome = self.m.leader();
            self.end(outcome);
        }
    }

    fn second(&mut self) {
        let waiting = self
            .m
            .members
            .values()
            .filter(|m| m.online && m.status == Status::Waiting)
            .count();
        match self.m.life.second(waiting, &self.m.settings) {
            Step::Nothing => {}
            Step::Counting(left) | Step::Tick(left) => self.announce(left),
            Step::Cancelled => {
                self.m
                    .broadcast("§cNot enough players; waiting for more to start.");
            }
            Step::Start => self.start_match(),
            Step::TimeUp => {
                let outcome = self.game.time_up(&self.m);
                self.m.broadcast("§eTime's up!");
                self.end(outcome);
            }
            Step::Drain => {
                self.m.broadcast("§7Sending you back to the lobby...");
                network::set_state(State::Draining);
            }
        }
        if self.m.life.phase == Phase::InGame {
            self.game.tick(&mut self.m);
            self.check_end();
        }
    }

    fn announce(&self, left: u64) {
        if ANNOUNCE.contains(&left) || left == self.m.settings.countdown_secs {
            self.m
                .broadcast(&format!("§eThe match starts in §f{left}§e s."));
        }
        if left <= 5 {
            self.m.title(&format!("§e{left}"), "");
        }
    }

    fn start_match(&mut self) {
        let players: Vec<Uuid> = self
            .m
            .members
            .iter()
            .filter(|(_, m)| m.online && m.status == Status::Waiting)
            .map(|(id, _)| *id)
            .collect();
        let assigned = teams::balance(
            &players,
            &self.m.settings.teams,
            &self.m.preferences,
            self.m.settings.team_size,
        );
        let mut spawn_index: BTreeMap<Option<String>, usize> = BTreeMap::new();
        for id in &players {
            let team = assigned.get(id).cloned();
            if let Some(member) = self.m.members.get_mut(id) {
                member.status = Status::Playing;
                member.team.clone_from(&team);
            }
            let index = spawn_index.entry(team.clone()).or_default();
            let spawn = self.m.arena.as_ref().and_then(|arena| {
                Some((
                    arena.world(self.m.server())?,
                    arena.map.spawn_for(team.as_deref(), *index),
                ))
            });
            *index += 1;
            let kit = self.game.kit(&self.m, *id);
            let Some(player) = self.m.player(*id) else {
                continue;
            };
            if let Some((world, at)) = spawn {
                player.teleport_world(world, at.position(), Some(at.yaw), Some(at.pitch));
            }
            player.set_gamemode(self.m.settings.gamemode);
            player.set_invulnerable(false);
            match kit {
                Some(kit) => kit.give(&player),
                None => player.get_inventory().clear_all(),
            }
            self.m.add_stat(*id, stats::GAMES_PLAYED, 1);
        }
        self.m.sides_started = self.m.sides_alive().len();
        network::set_state(State::InGame);
        self.m.broadcast("§aThe match has started!");
        for id in &players {
            if let (Some(player), Some(team)) = (self.m.player(*id), self.m.team(*id)) {
                player.send_system_message(
                    text(&format!("§7You're on team {}§7.", team.colored_name())),
                    false,
                );
            }
        }
        self.game.started(&mut self.m);
    }

    fn end(&mut self, outcome: Outcome) {
        if self.m.life.phase != Phase::InGame {
            return;
        }
        self.m.life.end(&self.m.settings);
        let winners = self.m.winners(&outcome);
        for id in &winners {
            self.m.add_stat(*id, stats::WINS, 1);
        }
        self.m.broadcast(&self.m.describe(&outcome));
        for id in self.m.online() {
            if let Some(player) = self.m.player(id) {
                if winners.contains(&id) {
                    show_title(&player, "§6Victory!", &self.m.describe(&outcome));
                } else {
                    show_title(&player, "§cGame over", &self.m.describe(&outcome));
                }
            }
        }
        self.m.outcome = Some(outcome.clone());
        self.game.ended(&mut self.m, &outcome);
    }

    fn draw_sidebars(&mut self) {
        let teams: Vec<(&Team, Vec<String>)> = self
            .m
            .settings
            .teams
            .iter()
            .map(|team| {
                let names = self
                    .m
                    .members
                    .values()
                    .filter(|m| m.online && m.team.as_deref() == Some(&team.id))
                    .map(|m| m.name.clone())
                    .collect();
                (team, names)
            })
            .collect();
        let mut drawn = Vec::new();
        for id in self.m.online() {
            let Some(player) = self.m.player(id) else {
                continue;
            };
            let mut lines = self.lines(id);
            lines.extend(self.game.sidebar(&self.m, id));
            drawn.push((id, player, lines));
        }
        let title = self.m.settings.title.clone();
        let friendly_fire = self.m.settings.friendly_fire;
        for (id, player, lines) in drawn {
            let shown = self.m.shown.entry(id).or_default();
            sidebar::draw(&player, shown, &title, &lines, &teams, friendly_fire);
        }
    }

    /// The SDK's own sidebar lines for `player`.
    fn lines(&self, player: Uuid) -> Vec<String> {
        let m = &self.m;
        let s = &m.settings;
        let map = m
            .arena
            .as_ref()
            .map(|arena| format!("§fMap: §a{}", arena.map.name));
        let mut lines = vec![String::new()];
        match m.life.phase {
            Phase::Waiting | Phase::Countdown => {
                let waiting = m
                    .members
                    .values()
                    .filter(|m| m.online && m.status == Status::Waiting)
                    .count();
                lines.push(format!("§fPlayers: §a{waiting}/{}", s.max_players));
                lines.push(String::new());
                if m.life.phase == Phase::Countdown {
                    lines.push(format!("§fStarting in §a{}s", m.life.left));
                } else {
                    let more = s.min_players.saturating_sub(waiting);
                    lines.push(format!("§7Waiting for {more} more..."));
                }
            }
            Phase::InGame => {
                match m.time_left_secs() {
                    Some(left) => lines.push(format!("§fTime left: §a{}", sidebar::clock(left))),
                    None => lines.push(format!("§fTime: §a{}", sidebar::clock(m.life.played))),
                }
                if s.teams.is_empty() {
                    lines.push(format!("§fAlive: §a{}", m.alive().len()));
                } else {
                    lines.push(String::new());
                    for team in &s.teams {
                        let members = m.members(&team.id);
                        if members.is_empty() {
                            continue;
                        }
                        let alive = members.iter().filter(|id| m.is_alive(**id)).count();
                        let mark = if alive > 0 { "§a✔" } else { "§c✘" };
                        lines.push(format!("{} {mark} §7{alive}", team.colored_name()));
                    }
                }
                if let Some(team) = m.team(player) {
                    lines.push(String::new());
                    lines.push(format!("§fYour team: {}", team.colored_name()));
                }
                if !m.is_alive(player) {
                    lines.push("§7Spectating".to_owned());
                }
            }
            Phase::Ending | Phase::Over => {
                lines.push("§6Game over".to_owned());
                if let Some(outcome) = &m.outcome {
                    let winner = match outcome {
                        Outcome::Team(_) => m.team_of_outcome(outcome),
                        Outcome::Player(id) => m.name(*id).map(|n| format!("§f{n}")),
                        Outcome::Draw => Some("§7Draw".to_owned()),
                    };
                    if let Some(winner) = winner {
                        lines.push(format!("§fWinner: {winner}"));
                    }
                }
            }
        }
        if let Some(map) = map {
            lines.push(String::new());
            lines.push(map);
        }
        lines
    }
}

impl Match {
    fn team_of_outcome(&self, outcome: &Outcome) -> Option<String> {
        let Outcome::Team(id) = outcome else {
            return None;
        };
        self.settings
            .teams
            .iter()
            .find(|team| &team.id == id)
            .map(Team::colored_name)
    }

    fn by_entity(&self, entity: i32) -> Option<Uuid> {
        self.members
            .iter()
            .find(|(_, m)| m.online && m.entity == entity)
            .map(|(id, _)| *id)
    }
}

struct OnJoin;

impl EventHandler<PlayerJoinEvent> for OnJoin {
    fn handle(&self, _server: Server, event: PlayerJoinEventData) -> PlayerJoinEventData {
        let id = to_uuid(&event.player.get_id());
        let name = event.player.get_name();
        let entity = i32::try_from(event.player.as_entity().get_id()).unwrap_or(-1);
        with(|m| {
            let before = matches!(m.life.phase, Phase::Waiting | Phase::Countdown);
            let member = m.members.entry(id).or_insert_with(|| Member {
                name: name.clone(),
                entity,
                online: true,
                status: Status::Waiting,
                team: None,
                hit_by: None,
            });
            member.online = true;
            member.entity = entity;
            member.name = name;
            member.status = match member.status {
                _ if before => Status::Waiting,
                Status::Playing | Status::Out => Status::Out,
                _ => Status::Watching,
            };
            m.shown.remove(&id);
            if before {
                m.later(SETTLE_TICKS, Action::Prepare(id));
            } else {
                m.later(SETTLE_TICKS, Action::Spectate(id));
            }
        });
        event
    }
}

struct OnLeave;

impl EventHandler<PlayerLeaveEvent> for OnLeave {
    fn handle(&self, _server: Server, event: PlayerLeaveEventData) -> PlayerLeaveEventData {
        let id = to_uuid(&event.player.get_id());
        with(|m| {
            if m.life.phase == Phase::InGame && m.is_alive(id) {
                m.out.push((id, Cause::Left));
            }
            if let Some(member) = m.members.get_mut(&id) {
                member.online = false;
            }
            m.shown.remove(&id);
            m.preferences.remove(&id);
        });
        event
    }
}

/// Nobody is hurt outside the match, or while spectating.
struct OnDamage;

impl EventHandler<EntityDamageEvent> for OnDamage {
    fn handle(&self, _server: Server, mut event: EntityDamageEventData) -> EntityDamageEventData {
        let cancel = with(|m| {
            m.by_entity(event.entity_id)
                .is_some_and(|id| m.life.phase != Phase::InGame || !m.is_alive(id))
        });
        if cancel == Some(true) {
            event.cancelled = true;
        }
        event
    }
}

/// Friendly fire, spectators hitting, and who hit whom for kill credit.
struct OnHit;

impl EventHandler<EntityDamageByEntityEvent> for OnHit {
    fn handle(
        &self,
        _server: Server,
        mut event: EntityDamageByEntityEventData,
    ) -> EntityDamageByEntityEventData {
        let cancel = with(|m| {
            let attacker = m.by_entity(event.damager_id)?;
            if !m.is_alive(attacker) {
                return Some(true);
            }
            let victim = m.by_entity(event.entity_id)?;
            let same_team = m.team(attacker).is_some() && m.team(attacker) == m.team(victim);
            if same_team && !m.settings.friendly_fire {
                return Some(true);
            }
            let now = m.ticks;
            if let Some(member) = m.members.get_mut(&victim) {
                member.hit_by = Some((attacker, now));
            }
            Some(false)
        })
        .flatten();
        if cancel == Some(true) {
            event.cancelled = true;
        }
        event
    }
}

struct OnDeath;

impl EventHandler<PlayerDeathEvent> for OnDeath {
    fn handle(&self, _server: Server, event: PlayerDeathEventData) -> PlayerDeathEventData {
        let id = to_uuid(&event.player.get_id());
        with(|m| {
            if m.life.phase == Phase::InGame && m.is_alive(id) {
                let killer = m.last_attacker(id);
                m.out.push((id, Cause::Died { killer }));
            }
        });
        event
    }
}

struct OnMove;

impl EventHandler<PlayerMoveEvent> for OnMove {
    fn handle(&self, _server: Server, event: PlayerMoveEventData) -> PlayerMoveEventData {
        let y = event.to_position.1;
        // Cheap checks first: this runs for every move.
        let below = with(|m| {
            m.life.phase == Phase::InGame
                && m.arena.as_ref().is_some_and(|arena| y < arena.map.void_y)
        });
        if below == Some(true) {
            let id = to_uuid(&event.player.get_id());
            with(|m| {
                if m.is_alive(id) && !m.out.iter().any(|(out, _)| *out == id) {
                    let killer = m.last_attacker(id);
                    m.out.push((id, Cause::Void { killer }));
                }
            });
        }
        event
    }
}

/// Team chat, and spectators chatting among themselves.
struct OnChat;

impl EventHandler<PlayerChatEvent> for OnChat {
    fn handle(&self, server: Server, mut event: PlayerChatEventData) -> PlayerChatEventData {
        let id = to_uuid(&event.player.get_id());
        let message = event.message.clone();
        let Some(Some((line, to))) = with(|m| {
            if m.life.phase != Phase::InGame || !m.settings.team_chat {
                return None;
            }
            let name = m.name(id)?.to_owned();
            if !m.is_alive(id) {
                let to = m
                    .members
                    .iter()
                    .filter(|(_, o)| o.online && o.status != Status::Playing)
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>();
                return Some((format!("§7[Spectator] {name}: §f{message}"), to));
            }
            let team = m.team(id)?.clone();
            if message.starts_with('!') {
                return None;
            }
            let to = m
                .members(&team.id)
                .into_iter()
                .filter(|id| m.members.get(id).is_some_and(|o| o.online))
                .collect();
            Some((format!("{}[Team] §f{name}: {message}", team.code()), to))
        }) else {
            if let Some(shout) = message.strip_prefix('!') {
                event.message = shout.trim_start().to_owned();
            }
            return event;
        };
        event.cancelled = true;
        for to in to {
            if let Some(player) = server.get_player_by_uuid(to_wit(to)) {
                player.send_system_message(text(&line), false);
            }
        }
        event
    }
}

/// Nobody gets hungry outside the match.
struct OnHunger;

impl EventHandler<FoodLevelChangeEvent> for OnHunger {
    fn handle(
        &self,
        _server: Server,
        mut event: FoodLevelChangeEventData,
    ) -> FoodLevelChangeEventData {
        if with(|m| m.life.phase != Phase::InGame) == Some(true) {
            event.cancelled = true;
        }
        event
    }
}
