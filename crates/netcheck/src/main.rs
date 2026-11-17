//! Joins a Hallow network through its proxy as a headless 26.3 client, then
//! switches servers with `/server` and checks it lands each time.
//!
//! ```text
//! netcheck [--proxy 127.0.0.1:25565] [--name netcheck] [--route game-1,lobby] [--stay 0]
//! netcheck --route "/some-command arg,game-1"   # steps starting with / run a command
//! netcheck --route "~5,/msg someone hi"         # steps like ~5 wait that many seconds
//! netcheck --route ">hello there"               # steps starting with > send chat
//! netcheck --bots 100 [--ramp 10] [--stay 60] [--name load]   # load test; bots are load0000, load0001, ...
//! ```
//!
//! With `--control-plane 127.0.0.1:18090 --token-file <file>`, route steps
//! can also check and act through the control plane, for the never-dropped
//! checks (docs/ARCHITECTURE.md):
//!
//! | Step | Does |
//! |---|---|
//! | `@lobby` | Waits (up to 90 s, following moves) until the control plane says the bot is on a server whose name starts with `lobby`; with `*` or `?` in it, one the whole name matches (`@test-game-??????` is a game instance, not its lobby) |
//! | `?text` | Waits (up to 60 s) until the bot has been shown text containing `text` (chat, action bar or title) since the last `?` step |
//! | `!ban 1h Some reason` | Bans the bot (`perm` for permanent) through `POST /v1/punishments`, as the console |
//! | `!unban` | Lifts the bot's ban |
//! | `!kick Some reason` | Kicks the bot |
//! | `$docker compose stop lobby` | Runs a shell command, answering the server while it runs |
//! | `^` | Disconnects and joins again |
//!
//! Ban, unban and kick need nothing more than the console's view of the
//! bot's name; `@` asks `GET /v1/players?name=`.
//!
//! With `--bots`, that many bots join (`--ramp` per second), walk around spawn
//! like idle players for `--stay` seconds, and a summary reports how many
//! joined, their join times and how many were still connected at the end.
//!
//! The proxy must allow offline players (`online_mode = false` in Vine).
//! Exits non-zero on the first step that fails, or if any bot fails.

use std::{collections::BTreeMap, net::SocketAddr, process::ExitCode, time::Duration};

use bytes::Bytes;
use pumpkin_data::packet::{CURRENT_MC_VERSION, clientbound};
use pumpkin_protocol::{
    ClientPacket, ConnectionState, MultiVersionJavaPacket, RawPacket, ServerPacket,
    codec::var_int::VarInt,
    java::{
        client::{
            config::{CFinishConfig, CKnownPacks},
            login::CLoginSuccess,
            play::{CLogin, CPlayerPosition, CStartConfiguration},
        },
        packet_decoder::TCPNetworkDecoder,
        packet_encoder::TCPNetworkEncoder,
        server::{
            config::{
                SAcknowledgeFinishConfig, SClientInformationConfig, SConfigPong,
                SKeepAlive as SConfigKeepAlive, SKnownPacks,
            },
            handshake::SHandShake,
            login::{SLoginAcknowledged, SLoginStart},
            play::{
                SChatCommand, SChatMessage, SConfigurationAcknowledged, SConfirmTeleport,
                SKeepAlive, SPlayPong, SPlayerLoaded, SPlayerPosition,
            },
        },
    },
    ser::NetworkWriteExt,
};
use pumpkin_util::math::vector3::Vector3;
use tokio::{
    io::{BufReader, BufWriter},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::mpsc,
    time::{Instant, interval, sleep, timeout},
};
use uuid::Uuid;

const STEP_TIMEOUT: Duration = Duration::from_secs(30);
const SWITCH_TIMEOUT: Duration = Duration::from_secs(15);
/// What Vine says when it won't move a player.
const REFUSALS: [&str; 3] = ["Unknown server", "does not exist", "Failed to connect"];
const COMPRESSION_LEVEL: u32 = 6;

type Reader = TCPNetworkDecoder<BufReader<OwnedReadHalf>>;
type Writer = TCPNetworkEncoder<BufWriter<OwnedWriteHalf>>;

struct Bot {
    name: String,
    reader: Reader,
    writer: Writer,
    /// Where the server last put this player.
    position: Vector3<f64>,
    /// Print chat the player receives.
    verbose: bool,
    /// Text shown to the player since the last `?` step.
    seen: Vec<String>,
}

async fn send_to<P: ClientPacket + MultiVersionJavaPacket>(
    writer: &mut Writer,
    packet: &P,
) -> Result<(), String> {
    let id = P::to_id(CURRENT_MC_VERSION);
    let mut buf = Vec::new();
    buf.write_var_int(&VarInt(id))
        .map_err(|err| format!("writing packet id {id}: {err:?}"))?;
    packet
        .write_packet_data(&mut buf, &CURRENT_MC_VERSION)
        .map_err(|err| format!("serializing packet {id}: {err:?}"))?;
    writer
        .write_packet(Bytes::from(buf))
        .await
        .map_err(|err| format!("writing packet {id}: {err}"))?;
    writer
        .flush()
        .await
        .map_err(|err| format!("flushing: {err}"))
}

impl Bot {
    async fn send<P: ClientPacket + MultiVersionJavaPacket>(
        &mut self,
        packet: &P,
    ) -> Result<(), String> {
        send_to(&mut self.writer, packet).await
    }

    async fn next(&mut self) -> Result<RawPacket, String> {
        match timeout(STEP_TIMEOUT, self.reader.get_raw_packet()).await {
            Ok(Ok(packet)) => Ok(packet),
            Ok(Err(err)) => Err(format!("connection closed or bad packet: {err}")),
            Err(_) => Err(format!("no packet for {}s", STEP_TIMEOUT.as_secs())),
        }
    }

    /// Handshake and login, through to the configuration state.
    async fn login(addr: SocketAddr, name: &str) -> Result<Self, String> {
        let stream = TcpStream::connect(addr)
            .await
            .map_err(|err| format!("connecting to {addr}: {err}"))?;
        let _ = stream.set_nodelay(true);
        let (read, write) = stream.into_split();
        let mut bot = Self {
            name: name.to_string(),
            reader: TCPNetworkDecoder::new(BufReader::new(read)),
            writer: TCPNetworkEncoder::new(BufWriter::new(write)),
            position: Vector3::new(0.0, 0.0, 0.0),
            verbose: true,
            seen: Vec::new(),
        };

        bot.send(&SHandShake {
            protocol_version: VarInt(CURRENT_MC_VERSION.protocol_version()),
            server_address: addr.ip().to_string().into(),
            server_port: addr.port(),
            next_state: ConnectionState::Login,
        })
        .await?;
        bot.send(&SLoginStart {
            name: name.into(),
            uuid: Uuid::new_v4(),
        })
        .await?;

        let success = CLoginSuccess::to_id(CURRENT_MC_VERSION);
        loop {
            let packet = bot.next().await?;
            if packet.id == clientbound::login::LOGIN_COMPRESSION {
                let threshold =
                    read_var_int(&packet.payload).ok_or("bad Set Compression packet")?;
                if let Ok(threshold) = usize::try_from(threshold) {
                    bot.reader.set_compression(threshold);
                    bot.writer.set_compression((threshold, COMPRESSION_LEVEL));
                }
            } else if packet.id == clientbound::login::LOGIN_DISCONNECT {
                return Err(format!("kicked at login: {}", text_of(&packet.payload)));
            } else if packet.id == success {
                break;
            }
        }
        bot.send(&SLoginAcknowledged).await?;
        bot.send(&SClientInformationConfig {
            locale: "en_us",
            view_distance: 2,
            chat_mode: VarInt(0),
            chat_colors: true,
            skin_parts: 0x7F,
            main_hand: VarInt(1),
            text_filtering: false,
            server_listing: true,
        })
        .await?;
        Ok(bot)
    }

    /// Runs the configuration state until the server finishes it.
    async fn configure(&mut self) -> Result<(), String> {
        let known_packs = CKnownPacks::to_id(CURRENT_MC_VERSION);
        let finish = CFinishConfig::to_id(CURRENT_MC_VERSION);
        loop {
            let packet = self.next().await?;
            if packet.id == known_packs {
                self.send(&SKnownPacks {
                    known_packs: Vec::new(),
                })
                .await?;
            } else if packet.id == clientbound::config::KEEP_ALIVE {
                let keep_alive_id = read_i64(&packet.payload).ok_or("bad keep-alive")?;
                self.send(&SConfigKeepAlive { keep_alive_id }).await?;
            } else if packet.id == clientbound::config::PING {
                let id = read_i32(&packet.payload).ok_or("bad ping")?;
                self.send(&SConfigPong { id }).await?;
            } else if packet.id == clientbound::config::DISCONNECT {
                return Err(format!(
                    "kicked in configuration: {}",
                    text_of(&packet.payload)
                ));
            } else if packet.id == finish {
                self.send(&SAcknowledgeFinishConfig).await?;
                return Ok(());
            }
        }
    }

    /// Waits in play until the server has sent the join and the first position.
    async fn spawn(&mut self) -> Result<(), String> {
        let (mut joined, mut placed) = (false, false);
        while !(joined && placed) {
            let packet = self.next().await?;
            if self.answer_play(&packet).await? {
                continue;
            }
            if packet.id == CLogin::to_id(CURRENT_MC_VERSION) {
                joined = true;
            } else if packet.id == CPlayerPosition::to_id(CURRENT_MC_VERSION) {
                let mut slice = &packet.payload[..];
                let position = CPlayerPosition::read(&mut slice, &CURRENT_MC_VERSION)
                    .map_err(|err| format!("reading position: {err}"))?;
                self.send(&SConfirmTeleport {
                    teleport_id: position.teleport_id,
                    position: position.position,
                    yaw: position.yaw,
                    pitch: position.pitch,
                })
                .await?;
                self.send(&SPlayerLoaded).await?;
                self.position = position.position;
                placed = true;
            }
        }
        Ok(())
    }

    /// Asks the proxy to move this player, then follows the switch through a
    /// new configuration phase into the new server.
    async fn switch(&mut self, server: &str) -> Result<(), String> {
        self.send(&SChatCommand {
            command: &format!("server {server}"),
        })
        .await?;
        let start_config = CStartConfiguration::to_id(CURRENT_MC_VERSION);
        // Keep-alives keep the connection busy, so a refused switch needs its own limit.
        let deadline = Instant::now() + SWITCH_TIMEOUT;
        loop {
            if Instant::now() >= deadline {
                return Err(format!("no switch within {}s", SWITCH_TIMEOUT.as_secs()));
            }
            let packet = self.next().await?;
            if packet.id == clientbound::play::SYSTEM_CHAT {
                let text = text_of(&packet.payload);
                if REFUSALS.iter().any(|refusal| text.contains(refusal)) {
                    return Err(format!("refused: {text}"));
                }
            }
            if self.answer_play(&packet).await? {
                continue;
            }
            if packet.id == start_config {
                self.send(&SConfigurationAcknowledged).await?;
                break;
            }
        }
        self.configure().await?;
        self.spawn().await
    }

    /// Stays in play for `seconds`, answering keep-alives, confirming any
    /// teleport the server sends and following switches the proxy starts, so
    /// plugins can act on the player.
    async fn stay(&mut self, seconds: u64) -> Result<(), String> {
        let until = Instant::now() + Duration::from_secs(seconds);
        let position = CPlayerPosition::to_id(CURRENT_MC_VERSION);
        let start_config = CStartConfiguration::to_id(CURRENT_MC_VERSION);
        while Instant::now() < until {
            let remaining = until - Instant::now();
            let packet = match timeout(remaining, self.reader.get_raw_packet()).await {
                Ok(Ok(packet)) => packet,
                Ok(Err(err)) => return Err(format!("connection closed or bad packet: {err}")),
                Err(_) => break,
            };
            if self.answer_play(&packet).await? {
                continue;
            }
            // The proxy moved this player (a party warp, the matchmaker):
            // follow it into the new server.
            if packet.id == start_config {
                self.send(&SConfigurationAcknowledged).await?;
                self.configure().await?;
                self.spawn().await?;
                if self.verbose {
                    println!("    {} was moved to another server by the proxy", self.name);
                }
                continue;
            }
            if packet.id == position {
                let mut slice = &packet.payload[..];
                let teleport = CPlayerPosition::read(&mut slice, &CURRENT_MC_VERSION)
                    .map_err(|err| format!("reading position: {err}"))?;
                self.send(&SConfirmTeleport {
                    teleport_id: teleport.teleport_id,
                    position: teleport.position,
                    yaw: teleport.yaw,
                    pitch: teleport.pitch,
                })
                .await?;
            }
        }
        Ok(())
    }

    /// Walks a 4-block circle around where it spawned for `seconds`, sending a
    /// position four times a second like an idle player, while answering the
    /// server. Packets are read on their own task: a read cut off halfway
    /// would desync the stream.
    async fn wander(self, seconds: u64) -> Result<(), String> {
        let Self {
            reader,
            mut writer,
            position: home,
            ..
        } = self;
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

        let until = Instant::now() + Duration::from_secs(seconds);
        let mut steps = interval(Duration::from_millis(250));
        let mut angle: f64 = 0.0;
        let teleport = CPlayerPosition::to_id(CURRENT_MC_VERSION);
        let result = loop {
            if Instant::now() >= until {
                break Ok(());
            }
            tokio::select! {
                _ = steps.tick() => {
                    angle += 0.25;
                    let at = Vector3::new(home.x + 4.0 * angle.cos(), home.y, home.z + 4.0 * angle.sin());
                    if let Err(err) = send_to(&mut writer, &SPlayerPosition { position: at, collision: 1 }).await {
                        break Err(err);
                    }
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
                            Ok(moved) => send_to(&mut writer, &SConfirmTeleport {
                                teleport_id: moved.teleport_id,
                                position: moved.position,
                                yaw: moved.yaw,
                                pitch: moved.pitch,
                            }).await,
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

    /// Answers keep-alives and pings, reports chat and kicks. Returns true
    /// when the packet was handled here.
    async fn answer_play(&mut self, packet: &RawPacket) -> Result<bool, String> {
        if packet.id == clientbound::play::KEEP_ALIVE {
            let keep_alive_id = read_i64(&packet.payload).ok_or("bad keep-alive")?;
            self.send(&SKeepAlive { keep_alive_id }).await?;
            Ok(true)
        } else if packet.id == clientbound::play::PING {
            let id = read_i32(&packet.payload).ok_or("bad ping")?;
            self.send(&SPlayPong { id }).await?;
            Ok(true)
        } else if packet.id == clientbound::play::SYSTEM_CHAT
            || packet.id == clientbound::play::SET_TITLE_TEXT
            || packet.id == clientbound::play::SET_SUBTITLE_TEXT
            || packet.id == clientbound::play::SET_ACTION_BAR_TEXT
        {
            let text = text_of(&packet.payload);
            if self.verbose && self.seen.last() != Some(&text) {
                println!("    {} sees: {text}", self.name);
            }
            self.seen.push(text);
            if self.seen.len() > 500 {
                self.seen.drain(..250);
            }
            Ok(true)
        } else if packet.id == clientbound::play::DISCONNECT {
            Err(format!("kicked: {}", text_of(&packet.payload)))
        } else {
            Ok(false)
        }
    }
}

fn read_var_int(bytes: &[u8]) -> Option<i32> {
    let mut value: i32 = 0;
    for (i, byte) in bytes.iter().take(5).enumerate() {
        value |= i32::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

fn read_i64(bytes: &[u8]) -> Option<i64> {
    Some(i64::from_be_bytes(bytes.get(..8)?.try_into().ok()?))
}

fn read_i32(bytes: &[u8]) -> Option<i32> {
    Some(i32::from_be_bytes(bytes.get(..4)?.try_into().ok()?))
}

/// The readable text inside a chat component, good enough for a log line.
fn text_of(payload: &[u8]) -> String {
    let text: String = String::from_utf8_lossy(payload)
        .chars()
        .map(|c| {
            if c.is_control() || c == '\u{fffd}' {
                ' '
            } else {
                c
            }
        })
        .collect();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Args {
    proxy: SocketAddr,
    name: String,
    route: Vec<String>,
    stay: u64,
    bots: u32,
    ramp: u32,
    control_plane: Option<ControlPlane>,
}

/// The control plane's API, for `@`, `!` steps.
struct ControlPlane {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl ControlPlane {
    /// The server the control plane has `name` on, if they're online.
    async fn server_of(&self, name: &str) -> Result<Option<String>, String> {
        let res = self
            .http
            .get(format!(
                "{}{}",
                self.base,
                hallow_proto::routes::player_by_name(name)
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|err| format!("asking the control plane: {err}"))?;
        if res.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let location: hallow_proto::presence::PlayerLocation = res
            .error_for_status()
            .map_err(|err| format!("asking the control plane: {err}"))?
            .json()
            .await
            .map_err(|err| format!("reading the control plane's answer: {err}"))?;
        Ok(location.server)
    }

    /// Issues or lifts a punishment and returns the control plane's reply.
    async fn punish<B: serde::Serialize>(&self, path: &str, body: &B) -> Result<String, String> {
        let reply: hallow_proto::punishment::PunishmentReply = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .json(body)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|err| format!("calling the control plane: {err}"))?
            .json()
            .await
            .map_err(|err| format!("reading the control plane's answer: {err}"))?;
        if reply.ok {
            Ok(text_of(reply.message.as_bytes()))
        } else {
            Err(format!("refused: {}", reply.message))
        }
    }
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        proxy: "127.0.0.1:25565".parse().expect("valid default"),
        name: "netcheck".to_string(),
        route: vec!["game-1".to_string(), "lobby".to_string()],
        stay: 0,
        bots: 0,
        ramp: 10,
        control_plane: None,
    };
    let (mut cp_addr, mut token_file) = (None, None);
    let mut it = std::env::args().skip(1);
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--proxy" => {
                args.proxy = value
                    .parse()
                    .map_err(|err| format!("--proxy {value}: {err}"))?;
            }
            "--name" => args.name = value,
            "--route" => {
                args.route = value
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            "--stay" => {
                args.stay = value
                    .parse()
                    .map_err(|err| format!("--stay {value}: {err}"))?;
            }
            "--bots" => {
                args.bots = value
                    .parse()
                    .map_err(|err| format!("--bots {value}: {err}"))?;
            }
            "--control-plane" => cp_addr = Some(value),
            "--token-file" => token_file = Some(value),
            "--ramp" => {
                args.ramp = value
                    .parse::<u32>()
                    .map_err(|err| format!("--ramp {value}: {err}"))?
                    .max(1);
            }
            _ => return Err(format!("unknown flag {flag}")),
        }
    }
    if let Some(addr) = cp_addr {
        let file = token_file.ok_or("--control-plane needs --token-file")?;
        let token = std::fs::read_to_string(&file)
            .map_err(|err| format!("--token-file {file}: {err}"))?
            .trim()
            .to_owned();
        args.control_plane = Some(ControlPlane {
            base: format!("http://{addr}"),
            token,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(|err| format!("HTTP client: {err}"))?,
        });
    }
    Ok(args)
}

async fn run(args: &Args) -> Result<(), String> {
    let started = Instant::now();
    println!(
        "netcheck: joining {} as {} (Java {})",
        args.proxy,
        args.name,
        CURRENT_MC_VERSION.protocol_version()
    );
    let mut bot = rejoin(args).await?;
    println!(
        "  ok  joined the default server ({} ms)",
        started.elapsed().as_millis()
    );

    for server in &args.route {
        let step = Instant::now();
        if control_step(&mut bot, args, server)
            .await
            .map_err(|err| format!("step {server}: {err}"))?
        {
            println!("  ok  {server} ({} ms)", step.elapsed().as_millis());
            continue;
        }
        if server == "^" {
            bot = rejoin(args)
                .await
                .map_err(|err| format!("rejoining: {err}"))?;
            println!("  ok  joined again ({} ms)", step.elapsed().as_millis());
            continue;
        }
        // `~N` waits N seconds (still answering keep-alives and printing chat),
        // a step starting with `/` runs that command, and anything else is a
        // server to switch to.
        if let Some(secs) = server.strip_prefix('~') {
            let secs: u64 = secs
                .parse()
                .map_err(|_| format!("bad wait step {server}"))?;
            bot.stay(secs)
                .await
                .map_err(|err| format!("waiting {secs}s: {err}"))?;
            continue;
        }
        if let Some(message) = server.strip_prefix('>') {
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0));
            bot.send(&SChatMessage {
                message,
                timestamp,
                salt: 0,
                signature: None,
                message_count: VarInt(0),
                acknowledged: &[0; 3],
                checksum: 0,
            })
            .await
            .map_err(|err| format!("say {message}: {err}"))?;
            bot.stay(1)
                .await
                .map_err(|err| format!("after saying {message}: {err}"))?;
            println!("  ok  said {message}");
            continue;
        }
        if let Some(command) = server.strip_prefix('/') {
            bot.send(&SChatCommand { command })
                .await
                .map_err(|err| format!("run /{command}: {err}"))?;
            bot.stay(1)
                .await
                .map_err(|err| format!("after /{command}: {err}"))?;
            println!("  ok  ran /{command}");
            continue;
        }
        bot.switch(server)
            .await
            .map_err(|err| format!("switch to {server}: {err}"))?;
        println!(
            "  ok  switched to {server} ({} ms)",
            step.elapsed().as_millis()
        );
    }
    if args.stay > 0 {
        bot.stay(args.stay)
            .await
            .map_err(|err| format!("staying {}s: {err}", args.stay))?;
        println!("  ok  stayed {}s", args.stay);
    }
    println!("netcheck: passed in {} ms", started.elapsed().as_millis());
    Ok(())
}

/// Logs in, configures and spawns.
async fn rejoin(args: &Args) -> Result<Bot, String> {
    let mut bot = Bot::login(args.proxy, &args.name)
        .await
        .map_err(|err| format!("login: {err}"))?;
    bot.configure()
        .await
        .map_err(|err| format!("configuration: {err}"))?;
    bot.spawn().await.map_err(|err| format!("spawn: {err}"))?;
    Ok(bot)
}

/// How long `@` waits for the bot to land, and `?` for text.
const LAND_TIMEOUT: Duration = Duration::from_secs(90);
const TEXT_TIMEOUT: Duration = Duration::from_secs(60);

/// Runs a step that checks or acts through the control plane or the shell
/// (`@`, `?`, `!`, `$`). Returns false if `step` isn't one.
async fn control_step(bot: &mut Bot, args: &Args, step: &str) -> Result<bool, String> {
    let cp = || {
        args.control_plane
            .as_ref()
            .ok_or_else(|| "this step needs --control-plane and --token-file".to_owned())
    };
    if let Some(prefix) = step.strip_prefix('@') {
        let cp = cp()?;
        let deadline = Instant::now() + LAND_TIMEOUT;
        loop {
            let server = cp.server_of(&args.name).await?;
            if server.as_deref().is_some_and(|s| server_matches(prefix, s)) {
                println!("    {} is on {}", args.name, server.unwrap_or_default());
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "still on {} after {}s",
                    server.as_deref().unwrap_or("no server"),
                    LAND_TIMEOUT.as_secs()
                ));
            }
            bot.stay(1).await?;
        }
    }
    if let Some(text) = step.strip_prefix('?') {
        let deadline = Instant::now() + TEXT_TIMEOUT;
        loop {
            if bot.seen.iter().any(|seen| seen.contains(text)) {
                bot.seen.clear();
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Err(format!("not shown within {}s", TEXT_TIMEOUT.as_secs()));
            }
            bot.stay(1).await?;
        }
    }
    if let Some(action) = step.strip_prefix('!') {
        let cp = cp()?;
        let mut words = action.split_whitespace();
        let verb = words.next().unwrap_or_default();
        let rest: Vec<&str> = words.collect();
        use hallow_proto::punishment::{IssuePunishment, PunishmentKind, RevokePunishment};
        let issue = |kind, duration_seconds, reason: &[&str]| IssuePunishment {
            kind,
            target: args.name.clone(),
            issuer: None,
            reason: if reason.is_empty() {
                "netcheck".into()
            } else {
                reason.join(" ")
            },
            duration_seconds,
            staff: None,
        };
        let routes = hallow_proto::routes::PUNISHMENTS;
        let answer = match (verb, rest.as_slice()) {
            ("ban", [length, reason @ ..]) => {
                let duration = if *length == "perm" {
                    None
                } else {
                    Some(parse_length(length).ok_or_else(|| format!("bad length {length}"))?)
                };
                cp.punish(routes, &issue(PunishmentKind::Ban, duration, reason))
                    .await?
            }
            ("kick", reason) => {
                cp.punish(routes, &issue(PunishmentKind::Kick, None, reason))
                    .await?
            }
            ("unban", reason) => {
                let revoke = RevokePunishment {
                    kind: PunishmentKind::Ban,
                    target: args.name.clone(),
                    issuer: None,
                    reason: (!reason.is_empty()).then(|| reason.join(" ")),
                    staff: None,
                };
                cp.punish(hallow_proto::routes::PUNISHMENTS_REVOKE, &revoke)
                    .await?
            }
            _ => return Err("use !ban <length|perm> <reason>, !unban or !kick <reason>".into()),
        };
        println!("    control plane: {answer}");
        return Ok(true);
    }
    if let Some(command) = step.strip_prefix('$') {
        let mut child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .spawn()
            .map_err(|err| format!("starting it: {err}"))?;
        // Keep answering the server while it runs.
        loop {
            if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
                if !status.success() {
                    return Err(format!("exited with {status}"));
                }
                return Ok(true);
            }
            bot.stay(1).await?;
        }
    }
    Ok(false)
}

/// An `@` step's pattern: a prefix, or a glob (`*`, `?`) over the whole name.
fn server_matches(pattern: &str, name: &str) -> bool {
    if !pattern.contains(['*', '?']) {
        return name.starts_with(pattern);
    }
    fn glob(p: &[u8], n: &[u8]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some(b'*'), _) => glob(&p[1..], n) || (!n.is_empty() && glob(p, &n[1..])),
            (Some(b'?'), Some(_)) => glob(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a == b => glob(&p[1..], &n[1..]),
            _ => false,
        }
    }
    glob(pattern.as_bytes(), name.as_bytes())
}

/// `30s`, `5m`, `2h`, `7d` in seconds.
fn parse_length(text: &str) -> Option<u64> {
    let (number, unit) = text.split_at(text.len().checked_sub(1)?);
    let n: u64 = number.parse().ok()?;
    Some(
        n * match unit {
            "s" => 1,
            "m" => 60,
            "h" => 3600,
            "d" => 86_400,
            _ => return None,
        },
    )
}

/// One load-test bot: join, then wander. Returns the join time.
async fn load_bot(
    proxy: SocketAddr,
    name: String,
    stay: u64,
) -> (String, Result<Duration, (&'static str, String)>) {
    let started = Instant::now();
    let joined = async {
        let mut bot = Bot::login(proxy, &name).await?;
        bot.verbose = false;
        bot.configure().await?;
        bot.spawn().await?;
        Ok::<Bot, String>(bot)
    }
    .await;
    let bot = match joined {
        Ok(bot) => bot,
        Err(err) => return (name, Err(("join", err))),
    };
    let join_time = started.elapsed();
    match bot.wander(stay).await {
        Ok(()) => (name, Ok(join_time)),
        Err(err) => (name, Err(("while playing", err))),
    }
}

fn percentile(sorted: &[Duration], p: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[rank].as_millis()
}

async fn load_test(args: &Args) -> Result<(), String> {
    println!(
        "netcheck: {} bots joining {} at {}/s, staying {}s (Java {})",
        args.bots,
        args.proxy,
        args.ramp,
        args.stay,
        CURRENT_MC_VERSION.protocol_version()
    );
    let started = Instant::now();
    let mut tasks = Vec::new();
    for i in 0..args.bots {
        if i > 0 && i % args.ramp == 0 {
            sleep(Duration::from_secs(1)).await;
        }
        // `--name` sets the prefix, so several runs can be online at once.
        let prefix = if args.name == "netcheck" {
            "load"
        } else {
            args.name.as_str()
        };
        let name = format!("{prefix}{i:04}");
        tasks.push(tokio::spawn(load_bot(args.proxy, name, args.stay)));
    }

    let mut join_times = Vec::new();
    let mut failures: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for task in tasks {
        match task.await {
            Ok((_, Ok(join))) => join_times.push(join),
            Ok((name, Err((when, err)))) => failures
                .entry(format!("{when}: {err}"))
                .or_default()
                .push(name),
            Err(err) => failures
                .entry(format!("task: {err}"))
                .or_default()
                .push("?".into()),
        }
    }
    join_times.sort();
    let failed: usize = failures.values().map(Vec::len).sum();
    println!(
        "  stayed connected: {}/{}  join p50 {} ms  p95 {} ms  max {} ms  ({} s total)",
        join_times.len(),
        args.bots,
        percentile(&join_times, 0.5),
        percentile(&join_times, 0.95),
        percentile(&join_times, 1.0),
        started.elapsed().as_secs()
    );
    for (reason, names) in &failures {
        println!("  FAIL {} bots, {reason} (e.g. {})", names.len(), names[0]);
    }
    if failed == 0 {
        Ok(())
    } else {
        Err(format!("{failed} of {} bots failed", args.bots))
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("netcheck: {err}");
            return ExitCode::from(2);
        }
    };
    let outcome = if args.bots > 0 {
        load_test(&args).await
    } else {
        run(&args).await
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("  FAIL {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_patterns() {
        assert!(server_matches("lobby", "lobby"));
        assert!(server_matches("test-game-lobby", "test-game-lobby-a1b2c3"));
        assert!(server_matches("test-game-??????", "test-game-a1b2c3"));
        assert!(!server_matches(
            "test-game-??????",
            "test-game-lobby-a1b2c3"
        ));
        assert!(server_matches("test-*-lobby-*", "test-game-lobby-a1b2c3"));
        assert_eq!(parse_length("2h"), Some(7200));
        assert_eq!(parse_length("2x"), None);
    }
}
