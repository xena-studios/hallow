//! Joins a Hallow network through its proxy as a headless 26.3 client, then
//! switches servers with `/server` and checks it lands each time.
//!
//! ```text
//! netcheck [--proxy 127.0.0.1:25565] [--name netcheck] [--route game-1,lobby]
//! ```
//!
//! The proxy must allow offline players (`online_mode = false` in Vine).
//! Exits non-zero on the first step that fails.

use std::{net::SocketAddr, process::ExitCode, time::Duration};

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
                SAcknowledgeFinishConfig, SClientInformationConfig, SConfigPong, SKnownPacks,
                SKeepAlive as SConfigKeepAlive,
            },
            handshake::SHandShake,
            login::{SLoginAcknowledged, SLoginStart},
            play::{
                SChatCommand, SConfigurationAcknowledged, SConfirmTeleport, SKeepAlive,
                SPlayPong, SPlayerLoaded,
            },
        },
    },
    ser::NetworkWriteExt,
};
use tokio::{
    io::{BufReader, BufWriter},
    net::{
        TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    time::{Instant, timeout},
};
use uuid::Uuid;

const STEP_TIMEOUT: Duration = Duration::from_secs(30);
const COMPRESSION_LEVEL: u32 = 6;

type Reader = TCPNetworkDecoder<BufReader<OwnedReadHalf>>;
type Writer = TCPNetworkEncoder<BufWriter<OwnedWriteHalf>>;

struct Bot {
    name: String,
    reader: Reader,
    writer: Writer,
}

impl Bot {
    async fn send<P: ClientPacket + MultiVersionJavaPacket>(&mut self, packet: &P) -> Result<(), String> {
        let id = P::to_id(CURRENT_MC_VERSION);
        let mut buf = Vec::new();
        buf.write_var_int(&VarInt(id))
            .map_err(|err| format!("writing packet id {id}: {err:?}"))?;
        packet
            .write_packet_data(&mut buf, &CURRENT_MC_VERSION)
            .map_err(|err| format!("serializing packet {id}: {err:?}"))?;
        self.writer
            .write_packet(Bytes::from(buf))
            .await
            .map_err(|err| format!("writing packet {id}: {err}"))?;
        self.writer.flush().await.map_err(|err| format!("flushing: {err}"))
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
                let threshold = read_var_int(&packet.payload)
                    .ok_or("bad Set Compression packet")?;
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
                return Err(format!("kicked in configuration: {}", text_of(&packet.payload)));
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
        loop {
            let packet = self.next().await?;
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
        } else if packet.id == clientbound::play::SYSTEM_CHAT {
            println!("    {} sees: {}", self.name, text_of(&packet.payload));
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
        .map(|c| if c.is_control() || c == '\u{fffd}' { ' ' } else { c })
        .collect();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Args {
    proxy: SocketAddr,
    name: String,
    route: Vec<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        proxy: "127.0.0.1:25565".parse().expect("valid default"),
        name: "netcheck".to_string(),
        route: vec!["game-1".to_string(), "lobby".to_string()],
    };
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
            _ => return Err(format!("unknown flag {flag}")),
        }
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
    let mut bot = Bot::login(args.proxy, &args.name)
        .await
        .map_err(|err| format!("login: {err}"))?;
    bot.configure()
        .await
        .map_err(|err| format!("configuration: {err}"))?;
    bot.spawn().await.map_err(|err| format!("spawn: {err}"))?;
    println!("  ok  joined the default server ({} ms)", started.elapsed().as_millis());

    for server in &args.route {
        let step = Instant::now();
        bot.switch(server)
            .await
            .map_err(|err| format!("switch to {server}: {err}"))?;
        println!("  ok  switched to {server} ({} ms)", step.elapsed().as_millis());
    }
    println!("netcheck: passed in {} ms", started.elapsed().as_millis());
    Ok(())
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
    match run(&args).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("  FAIL {err}");
            ExitCode::FAILURE
        }
    }
}
