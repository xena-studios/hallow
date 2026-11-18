//! The anticheat suite (`--anticheat <seconds>`): cheating bots and honest
//! bots play on one server at once, then the control plane's verdict service
//! is asked what it saw. Passes when every check reported a violation on its
//! cheat bot and no honest bot has any.
//!
//! | Bot | Plays | Expected |
//! |---|---|---|
//! | `Target` | stands still, ticking like a vanilla client | nothing |
//! | `Walker` | walks a circle at 4 blocks/s and clicks in uneven bursts | nothing |
//! | `Fighter` | walks up to `Target` and hits it from 1.7 blocks | nothing |
//! | `Speed` | runs back and forth at 20 blocks/s | `speed` |
//! | `Fly` | rises 6 blocks off the ground and hovers | `fly` |
//! | `Reach` | hits `Target` from 3.8 blocks (Pumpkin allows 4) | `reach` |
//! | `Clicker` | clicks 25 times a second, 40 ms apart | `click_rate` |
//! | `Timer` | ticks 30 times a second | `timer` |
//!
//! Every bot sends a tick-end packet each tick, as 26.3 clients do. Names are
//! the `--name` prefix, a random run id and the role, so runs don't see each
//! other's violations. Checks report once per 30 s window, so the bots play
//! for `--anticheat` seconds (at least 35), leave, and the suite waits for
//! the windows that cover them before judging.

use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, time::Duration};

use hallow_proto::{routes, violation::PlayerViolations};
use pumpkin_data::packet::{CURRENT_MC_VERSION, clientbound};
use pumpkin_protocol::{
    MultiVersionJavaPacket, RawPacket, ServerPacket,
    codec::var_int::VarInt,
    java::{
        client::play::CPlayerPosition,
        server::play::{
            SAttack, SClientTickEnd, SConfirmTeleport, SKeepAlive, SPlayPong, SPlayerPosition,
            SSwingArm,
        },
    },
};
use pumpkin_util::math::vector3::Vector3;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, watch},
    time::{Instant, MissedTickBehavior, interval, sleep, sleep_until},
};

use crate::{Bot, read_i32, read_i64, send_to, text_of};

/// How long after the bots leave every collector has sent the windows that
/// cover them (one 30 s window, and a little).
const SETTLE: Duration = Duration::from_secs(35);
/// How long to keep asking for a cheat bot's expected violation after that.
const PATIENCE: Duration = Duration::from_secs(30);
const TICK: Duration = Duration::from_millis(50);
/// Blocks per tick: 4 blocks/s, a vanilla walk.
const WALK: f64 = 0.2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Role {
    Target,
    Walker,
    Fighter,
    Speed,
    Fly,
    Reach,
    Clicker,
    Timer,
}

impl Role {
    const ALL: [Self; 8] = [
        Self::Target,
        Self::Walker,
        Self::Fighter,
        Self::Speed,
        Self::Fly,
        Self::Reach,
        Self::Clicker,
        Self::Timer,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Target => "Target",
            Self::Walker => "Walker",
            Self::Fighter => "Fighter",
            Self::Speed => "Speed",
            Self::Fly => "Fly",
            Self::Reach => "Reach",
            Self::Clicker => "Clicker",
            Self::Timer => "Timer",
        }
    }

    /// The check a cheat bot should trip; `None` for honest bots.
    fn cheat(self) -> Option<&'static str> {
        match self {
            Self::Target | Self::Walker | Self::Fighter => None,
            Self::Speed => Some("speed"),
            Self::Fly => Some("fly"),
            Self::Reach => Some("reach"),
            Self::Clicker => Some("click_rate"),
            Self::Timer => Some("timer"),
        }
    }

    fn tick(self) -> Duration {
        if self == Self::Timer {
            // 1.5 times the normal speed.
            Duration::from_micros(33_333)
        } else {
            TICK
        }
    }
}

/// Where `Target` is: its entity id and position, once it's on the server.
type TargetInfo = Option<(i32, Vector3<f64>)>;

pub struct Options {
    pub proxy: SocketAddr,
    pub prefix: String,
    pub server: String,
    pub seconds: u64,
    pub control_plane: SocketAddr,
    pub token_file: PathBuf,
}

/// A small random number generator (xorshift), enough for uneven clicks.
struct Rng(u64);

impl Rng {
    fn new() -> Self {
        let seed = uuid::Uuid::new_v4().as_u64_pair().0 | 1;
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// A whole number in `low..high`.
    fn between(&mut self, low: u64, high: u64) -> u64 {
        low + self.next() % (high - low).max(1)
    }
}

pub async fn run(options: &Options) -> Result<(), String> {
    let token = std::fs::read_to_string(&options.token_file)
        .map_err(|err| format!("reading {}: {err}", options.token_file.display()))?
        .trim()
        .to_owned();
    let run_id = &uuid::Uuid::new_v4().simple().to_string()[..4];
    let names: BTreeMap<_, _> = Role::ALL
        .iter()
        .map(|&role| (role, format!("{}{run_id}{}", options.prefix, role.name())))
        .collect();
    if let Some(long) = names.values().find(|name| name.len() > 16) {
        return Err(format!(
            "bot name {long} is over 16 characters; use a shorter --name"
        ));
    }
    let seconds = options.seconds.max(35);
    println!(
        "netcheck: anticheat suite on {} through {}, {seconds}s of play (Java {})",
        options.server,
        options.proxy,
        CURRENT_MC_VERSION.protocol_version()
    );

    let (target_tx, target_rx) = watch::channel::<TargetInfo>(None);
    let mut tasks = Vec::new();
    for (&role, name) in &names {
        let (proxy, server, name) = (options.proxy, options.server.clone(), name.clone());
        let target = (role == Role::Target).then(|| target_tx.clone());
        let target_rx = target_rx.clone();
        tasks.push(tokio::spawn(async move {
            let result = play(proxy, &name, &server, role, seconds, target, target_rx).await;
            (role, name, result)
        }));
        // Target first, so the others find it.
        if role == Role::Target {
            sleep(Duration::from_secs(2)).await;
        }
    }
    drop(target_tx);
    let mut failures = Vec::new();
    for task in tasks {
        let (role, name, result) = task.await.map_err(|err| format!("bot task: {err}"))?;
        match result {
            Ok(()) => println!("  ok  {name} ({}) played {seconds}s", role.name()),
            Err(err) => failures.push(format!("{name} ({}): {err}", role.name())),
        }
    }
    if !failures.is_empty() {
        return Err(format!("bots failed: {}", failures.join("; ")));
    }

    println!(
        "  ..  waiting {}s for the windows that cover the bots",
        SETTLE.as_secs()
    );
    sleep(SETTLE).await;
    let deadline = Instant::now() + PATIENCE;
    let mut problems = Vec::new();
    for (&role, name) in &names {
        let found = loop {
            let standing = violations(options.control_plane, &token, name).await?;
            let checks: Vec<String> = standing
                .as_ref()
                .map(|s| s.violations.iter().map(|v| v.check.clone()).collect())
                .unwrap_or_default();
            let waiting = role
                .cheat()
                .is_some_and(|check| !checks.iter().any(|c| c == check));
            if !waiting || Instant::now() >= deadline {
                break (standing, checks);
            }
            sleep(Duration::from_secs(2)).await;
        };
        let (standing, checks) = found;
        let summary = standing
            .as_ref()
            .map(|s| {
                s.violations
                    .iter()
                    .map(|v| format!("{}: {}", v.check, v.summary))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        match role.cheat() {
            Some(check) if checks.iter().any(|c| c == check) => {
                println!("  ok  {name} caught by {check}");
                for line in summary.iter().filter(|line| !line.starts_with(check)) {
                    println!("      also: {line}");
                }
            }
            Some(check) => {
                problems.push(format!("{name}: no {check} violation (saw {checks:?})"));
            }
            None if checks.is_empty() => println!("  ok  {name} (honest) has no violations"),
            None => problems.push(format!(
                "{name} (honest) was flagged: {}",
                summary.join("; ")
            )),
        }
    }
    if problems.is_empty() {
        println!("netcheck: anticheat suite passed");
        Ok(())
    } else {
        for problem in &problems {
            println!("  FAIL {problem}");
        }
        Err(format!("{} anticheat expectations failed", problems.len()))
    }
}

/// Joins, moves to `server`, then plays `role` for `seconds`.
async fn play(
    proxy: SocketAddr,
    name: &str,
    server: &str,
    role: Role,
    seconds: u64,
    target: Option<watch::Sender<TargetInfo>>,
    target_rx: watch::Receiver<TargetInfo>,
) -> Result<(), String> {
    let mut bot = Bot::login(proxy, name).await?;
    bot.verbose = false;
    bot.configure().await?;
    bot.spawn().await?;
    bot.switch(server)
        .await
        .map_err(|err| format!("switch to {server}: {err}"))?;
    if let Some(target) = &target {
        let _ = target.send(Some((bot.entity_id, bot.position)));
    }
    act(bot, role, seconds, target_rx).await
}

/// What a bot does each tick and when it clicks.
struct Moves {
    role: Role,
    rng: Rng,
    home: Vector3<f64>,
    at: Vector3<f64>,
    on_ground: bool,
    ticks: u64,
    /// Where a fighter stands to hit `Target` from.
    post: Option<Vector3<f64>>,
    attacks_left: u32,
}

impl Moves {
    /// The next position to send.
    fn step(&mut self, target: TargetInfo) -> Vector3<f64> {
        self.ticks += 1;
        let t = self.ticks as f64;
        match self.role {
            Role::Target | Role::Clicker => {}
            Role::Walker => {
                // Radius 3 at 4 blocks/s.
                let angle = t * WALK / 3.0;
                self.at = Vector3::new(
                    self.home.x + 3.0 * angle.cos(),
                    self.home.y,
                    self.home.z + 3.0 * angle.sin(),
                );
            }
            Role::Timer => {
                let angle = t * 0.1 / 3.0;
                self.at = Vector3::new(
                    self.home.x + 3.0 * angle.cos(),
                    self.home.y,
                    self.home.z + 3.0 * angle.sin(),
                );
            }
            Role::Speed => {
                // One block a tick, back and forth over 16 blocks.
                let phase = self.ticks % 32;
                let offset = if phase < 16 {
                    phase as f64
                } else {
                    32.0 - phase as f64
                };
                self.at = Vector3::new(self.home.x + offset - 8.0, self.home.y, self.home.z);
            }
            Role::Fly => {
                self.on_ground = false;
                let rise = (t * 0.1).min(6.0);
                self.at = Vector3::new(self.home.x, self.home.y + rise, self.home.z);
            }
            Role::Fighter | Role::Reach => {
                if let Some((_, target)) = target {
                    // Level with the target, east of it: the eyes sit inside its
                    // hitbox's height, so reach is the gap to its side.
                    let gap = if self.role == Role::Reach { 3.8 } else { 1.7 };
                    let post = *self.post.get_or_insert(Vector3::new(
                        target.x + gap + 0.3,
                        target.y,
                        target.z,
                    ));
                    self.at = walk_towards(self.at, post);
                }
            }
        }
        self.at
    }

    /// Whether the bot stands where it hits from.
    fn in_place(&self) -> bool {
        self.post.is_some_and(|post| distance(self.at, post) < 1e-6)
    }

    /// When to click next, after one now; `None` for bots that don't.
    fn next_click(&mut self, now: Instant) -> Option<Instant> {
        let wait = match self.role {
            Role::Clicker => 40,
            // Bursts of uneven clicks, 4 to 11 a second, with pauses.
            Role::Walker => {
                if self.rng.between(0, 10) == 0 {
                    self.rng.between(1_500, 3_000)
                } else {
                    self.rng.between(90, 260)
                }
            }
            // About one hit a second, a few of them: enough to measure and
            // not enough to kill the target.
            Role::Fighter | Role::Reach => self.rng.between(800, 1_300),
            _ => return None,
        };
        Some(now + Duration::from_millis(wait))
    }
}

fn distance(a: Vector3<f64>, b: Vector3<f64>) -> f64 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2) + (a.z - b.z).powi(2)).sqrt()
}

/// One walking step from `at` towards `to`.
fn walk_towards(at: Vector3<f64>, to: Vector3<f64>) -> Vector3<f64> {
    let gap = distance(at, to);
    if gap <= WALK {
        return to;
    }
    let k = WALK / gap;
    Vector3::new(
        at.x + (to.x - at.x) * k,
        at.y + (to.y - at.y) * k,
        at.z + (to.z - at.z) * k,
    )
}

/// Plays `role` for `seconds`: a movement packet and a tick-end packet each
/// tick, clicks and attacks on their own schedule, answering the server
/// meanwhile. Packets are read on their own task, like `wander`.
async fn act(
    bot: Bot,
    role: Role,
    seconds: u64,
    target_rx: watch::Receiver<TargetInfo>,
) -> Result<(), String> {
    let Bot {
        reader,
        mut writer,
        position,
        ..
    } = bot;
    let (tx, mut rx) = mpsc::channel::<Result<RawPacket, String>>(64);
    let reading = tokio::spawn(async move {
        let mut reader = reader;
        loop {
            let packet = reader
                .get_raw_packet()
                .await
                .map_err(|err| format!("connection closed or bad packet: {err}"));
            let failed = packet.is_err();
            if tx.send(packet).await.is_err() || failed {
                break;
            }
        }
    });

    let mut moves = Moves {
        role,
        rng: Rng::new(),
        home: position,
        at: position,
        on_ground: true,
        ticks: 0,
        post: None,
        attacks_left: 5,
    };
    let until = Instant::now() + Duration::from_secs(seconds);
    let mut ticks = interval(role.tick());
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut next_click = moves.next_click(Instant::now());
    let teleport = CPlayerPosition::to_id(CURRENT_MC_VERSION);
    let far_future = Instant::now() + Duration::from_secs(24 * 3600);

    let result = loop {
        if Instant::now() >= until {
            break Ok(());
        }
        tokio::select! {
            _ = ticks.tick() => {
                let target = *target_rx.borrow();
                let at = moves.step(target);
                let collision = u8::from(moves.on_ground);
                if let Err(err) = send_to(&mut writer, &SPlayerPosition { position: at, collision }).await {
                    break Err(err);
                }
                if let Err(err) = send_to(&mut writer, &SClientTickEnd).await {
                    break Err(err);
                }
            }
            () = sleep_until(next_click.unwrap_or(far_future)) => {
                let now = Instant::now();
                let target = *target_rx.borrow();
                let hit = match (role, target) {
                    (Role::Fighter | Role::Reach, Some((entity, _))) if moves.in_place() && moves.attacks_left > 0 => Some(entity),
                    (Role::Fighter | Role::Reach, _) => {
                        // Not in place yet: no swing either.
                        next_click = moves.next_click(now);
                        continue;
                    }
                    _ => None,
                };
                let mut sent = send_to(&mut writer, &SSwingArm { hand: VarInt(0) }).await;
                if let (Ok(()), Some(entity)) = (&sent, hit) {
                    moves.attacks_left -= 1;
                    sent = send_to(&mut writer, &SAttack { entity_id: VarInt(entity) }).await;
                }
                if let Err(err) = sent {
                    break Err(err);
                }
                next_click = moves.next_click(now);
            }
            packet = rx.recv() => {
                let packet = match packet {
                    Some(Ok(packet)) => packet,
                    Some(Err(err)) => break Err(err),
                    None => break Err("reader stopped".into()),
                };
                let answered = if packet.id == clientbound::play::KEEP_ALIVE {
                    match read_i64(&packet.payload) {
                        Some(keep_alive_id) => send_to(&mut writer, &SKeepAlive { keep_alive_id }).await,
                        None => Err("bad keep-alive".into()),
                    }
                } else if packet.id == clientbound::play::PING {
                    match read_i32(&packet.payload) {
                        Some(id) => send_to(&mut writer, &SPlayPong { id }).await,
                        None => Err("bad ping".into()),
                    }
                } else if packet.id == clientbound::play::DISCONNECT {
                    Err(format!("kicked: {}", text_of(&packet.payload)))
                } else if packet.id == teleport {
                    let mut slice = &packet.payload[..];
                    match CPlayerPosition::read(&mut slice, &CURRENT_MC_VERSION) {
                        Ok(moved) => {
                            // Go on from where the server put us.
                            moves.at = moved.position;
                            moves.home = moved.position;
                            send_to(&mut writer, &SConfirmTeleport {
                                teleport_id: moved.teleport_id,
                                position: moved.position,
                                yaw: moved.yaw,
                                pitch: moved.pitch,
                            }).await
                        }
                        Err(err) => Err(format!("reading position: {err}")),
                    }
                } else {
                    Ok(())
                };
                if let Err(err) = answered {
                    break Err(err);
                }
            }
        }
    };
    reading.abort();
    result
}

/// `GET /v1/violations?name=`; `None` if the control plane doesn't know the
/// player. A plain HTTP/1.1 request, to keep netcheck free of an HTTP client.
async fn violations(
    control_plane: SocketAddr,
    token: &str,
    name: &str,
) -> Result<Option<PlayerViolations>, String> {
    let path = routes::violations_of(name);
    let mut stream = TcpStream::connect(control_plane)
        .await
        .map_err(|err| format!("connecting to the control plane at {control_plane}: {err}"))?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {control_plane}\r\nAuthorization: Bearer {token}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|err| format!("asking the control plane: {err}"))?;
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut response))
        .await
        .map_err(|_| "the control plane didn't answer".to_string())?
        .map_err(|err| format!("reading the control plane's answer: {err}"))?;
    let split = response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("no HTTP header in the answer")?;
    let head = String::from_utf8_lossy(&response[..split]).to_ascii_lowercase();
    let mut body = response[split + 4..].to_vec();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or("no HTTP status")?;
    if head.contains("transfer-encoding: chunked") {
        body = dechunk(&body).ok_or("bad chunked body")?;
    }
    match status {
        200 => serde_json::from_slice(&body)
            .map(Some)
            .map_err(|err| format!("reading violations for {name}: {err}")),
        404 => Ok(None),
        status => Err(format!(
            "GET {path}: {status} {}",
            String::from_utf8_lossy(&body)
        )),
    }
}

fn dechunk(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line = body.windows(2).position(|w| w == b"\r\n")?;
        let size = std::str::from_utf8(&body[..line]).ok()?;
        let size = usize::from_str_radix(size.split(';').next()?.trim(), 16).ok()?;
        body = &body[line + 2..];
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(body.get(..size)?);
        body = body.get(size + 2..)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_fit_and_roles_expect_their_check() {
        for role in Role::ALL {
            assert!(format!("ac1234{}", role.name()).len() <= 16);
        }
        let cheats: Vec<_> = Role::ALL.iter().filter_map(|r| r.cheat()).collect();
        assert_eq!(cheats, ["speed", "fly", "reach", "click_rate", "timer"]);
    }

    #[test]
    fn walking_steps_and_arrives() {
        let mut at = Vector3::new(0.0, 64.0, 0.0);
        let to = Vector3::new(1.0, 64.0, 0.0);
        for _ in 0..4 {
            at = walk_towards(at, to);
        }
        assert!((at.x - 0.8).abs() < 1e-9);
        at = walk_towards(at, to);
        assert_eq!(at.x, 1.0);
    }

    #[test]
    fn speed_runs_twenty_blocks_a_second() {
        let mut moves = Moves {
            role: Role::Speed,
            rng: Rng(1),
            home: Vector3::new(0.0, 64.0, 0.0),
            at: Vector3::new(0.0, 64.0, 0.0),
            on_ground: true,
            ticks: 0,
            post: None,
            attacks_left: 0,
        };
        let mut last = moves.step(None);
        let mut path = 0.0;
        for _ in 0..20 {
            let next = moves.step(None);
            path += distance(last, next);
            last = next;
        }
        assert!((path - 20.0).abs() < 1e-9, "{path}");
    }

    #[test]
    fn dechunks() {
        assert_eq!(
            dechunk(b"4\r\nWiki\r\n5;x=y\r\npedia\r\n0\r\n\r\n").unwrap(),
            b"Wikipedia"
        );
        assert!(dechunk(b"9\r\nshort\r\n").is_none());
    }
}
