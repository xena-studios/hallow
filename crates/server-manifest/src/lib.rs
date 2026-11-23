//! A server repo's `hallow.toml`: what the server is, how it scales, and what
//! the base image should do with it. The format is documented in
//! [docs/SERVERS.md](../../../docs/SERVERS.md#hallowtoml); a short example:
//!
//! ```toml
//! name = "skywars"            # the server kind: image name and control-plane kind
//! role = "game"               # "hub", "lobby" or "game"
//! lobby = "skywars-lobby"     # where players go when a game ends
//! base = "^0.2"               # base image versions this repo works with
//! world = "world"             # Pumpkin's world folder, relative to this file
//!
//! [players]
//! min = 2
//! max = 12
//!
//! [scaling]
//! warm = 1
//! max_instances = 200
//!
//! [[plugins]]
//! file = "plugins/skywars.wasm"
//! permissions = ["fs.read.data"]
//!
//! [pumpkin]                   # merged over the base image's pumpkin.toml
//! default_gamemode = "adventure"
//!
//! [env]
//! SKYWARS_MODE = "solo"
//! ```
//!
//! [`Manifest::parse`] checks everything that can be checked from the text;
//! [`Manifest::check_files`] checks the files it names, and
//! [`Manifest::check_base`] a base image version against `base`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};

use hallow_proto::server::ServerRole;
use semver::{Version, VersionReq};
use serde::Deserialize;

/// The manifest's file name at the root of a server repo, and in the image
/// (`/server/hallow.toml`).
pub const FILE_NAME: &str = "hallow.toml";

/// Plugins the base image ships; a server repo can't replace or configure them.
pub const BASE_PLUGINS: [&str; 1] = ["hallow_server"];

/// Plugins a server repo may not load, and why.
pub const REFUSED_PLUGINS: [(&str, &str); 1] = [(
    "pumpkin_java_multiversion",
    "old clients are translated by the proxy; servers only see the current version",
)];

/// `pumpkin.toml` keys a manifest may not set under `[pumpkin]`, and why.
/// Some would cut the server off the network, the rest have their own field.
const RESERVED_PUMPKIN_KEYS: [(&str, &str); 8] = [
    ("networking.proxy", "the base image sets up Vine forwarding"),
    (
        "networking.java.address",
        "the base image listens on 0.0.0.0, port HALLOW_SERVER_PORT (25565)",
    ),
    (
        "networking.java.online_mode",
        "the proxy authenticates players",
    ),
    ("networking.java.max_players", "set players.max instead"),
    ("default_level_name", "set world instead"),
    (
        "plugins.overrides.hallow_server",
        "the base image configures Hallow's plugin",
    ),
    (
        "telemetry",
        "telemetry stays off on Hallow networks (Decisions 24)",
    ),
    (
        "update_checker",
        "update checks stay off on Hallow networks (Decisions 24)",
    ),
];

/// What a server kind is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The lobby every player lands in after login. A network has one hub
    /// kind; the control plane finds it by name (`HALLOW_HUB_KIND`, `hub`).
    Hub,
    /// A lobby players move to, such as a game's own lobby with its queues.
    Lobby,
    /// One match per instance.
    Game,
}

impl Role {
    /// The role the server registers with: a hub is a lobby to the control
    /// plane, which tells it apart by kind.
    pub fn server_role(self) -> ServerRole {
        match self {
            Self::Hub | Self::Lobby => ServerRole::Lobby,
            Self::Game => ServerRole::Game,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hub => "hub",
            Self::Lobby => "lobby",
            Self::Game => "game",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a game picks its map.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MapPick {
    #[default]
    Random,
    Vote,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Players {
    /// Players needed to start a match; always 1 for hubs and lobbies.
    pub min: u32,
    /// Players per instance.
    pub max: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scaling {
    /// Idle instances kept ready while players are online.
    pub warm: u32,
    /// The most instances of this kind at once; `None` leaves it to the fleet.
    pub max_instances: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Maps {
    /// Folder of map worlds, relative to the manifest. `apply` copies it to
    /// `plugins/data/<first plugin>/maps`, where `hallow-sdk` reads it.
    pub dir: Option<PathBuf>,
    pub pick: MapPick,
}

/// A WASM plugin the repo ships.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plugin {
    /// Relative to the manifest.
    pub file: PathBuf,
    /// The plugin's name in Pumpkin, which keys its permissions; the file
    /// stem unless set.
    pub name: String,
    /// Pre-approved, so Pumpkin doesn't ask on the console.
    pub permissions: Vec<String>,
}

/// A parsed and validated `hallow.toml`.
#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    /// The server kind, such as `skywars`; also the image name.
    pub name: String,
    pub role: Role,
    /// The kind players return to when a game ends. A hub returns to itself.
    pub lobby: Option<String>,
    /// Base image versions this repo works with.
    pub base: VersionReq,
    /// Pumpkin's world folder, relative to the manifest. Pumpkin generates
    /// one when unset.
    pub world: Option<PathBuf>,
    /// Game kinds a lobby queues players for.
    pub queues: Vec<String>,
    pub players: Players,
    pub scaling: Scaling,
    /// Games only.
    pub maps: Option<Maps>,
    pub plugins: Vec<Plugin>,
    /// Merged over the base image's `pumpkin.toml`.
    pub pumpkin: toml::Table,
    /// Defaults for the server's environment; the container's own
    /// environment wins.
    pub env: BTreeMap<String, String>,
}

/// Why a manifest was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// Not valid TOML, or a field of the wrong type or name.
    Parse(String),
    /// Every problem found, each starting with its field.
    Invalid(Vec<String>),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "{}", message.trim_end()),
            Self::Invalid(problems) => {
                let s = if problems.len() == 1 { "" } else { "s" };
                write!(f, "{} problem{s}:", problems.len())?;
                for problem in problems {
                    write!(f, "\n  - {problem}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for Error {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    name: String,
    role: Role,
    lobby: Option<String>,
    base: String,
    world: Option<String>,
    #[serde(default)]
    queues: Vec<String>,
    players: RawPlayers,
    #[serde(default)]
    scaling: RawScaling,
    maps: Option<RawMaps>,
    #[serde(default)]
    plugins: Vec<RawPlugin>,
    #[serde(default)]
    pumpkin: toml::Table,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlayers {
    min: Option<u32>,
    max: u32,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawScaling {
    warm: Option<u32>,
    max_instances: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMaps {
    dir: Option<String>,
    #[serde(default)]
    pick: MapPick,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlugin {
    file: String,
    name: Option<String>,
    #[serde(default)]
    permissions: Vec<String>,
}

impl Manifest {
    /// Parses and validates a manifest's text. Paths aren't checked against
    /// the disk; see [`Manifest::check_files`].
    pub fn parse(text: &str) -> Result<Self, Error> {
        let raw: Raw = toml::from_str(text).map_err(|err| Error::Parse(err.to_string()))?;
        let mut problems = Vec::new();
        let mut problem =
            |field: &str, message: String| problems.push(format!("{field}: {message}"));

        if let Err(err) = check_kind_name(&raw.name) {
            problem("name", err);
        }

        let lobby = match (raw.role, raw.lobby) {
            (Role::Hub, None) => Some(raw.name.clone()),
            (Role::Hub, Some(lobby)) if lobby == raw.name => Some(lobby),
            (Role::Hub, Some(lobby)) => {
                problem(
                    "lobby",
                    format!("a hub returns players to itself; drop lobby = \"{lobby}\""),
                );
                None
            }
            (Role::Game, None) => {
                problem(
                    "lobby",
                    "a game must name the lobby players return to when it ends".into(),
                );
                None
            }
            (_, Some(lobby)) => {
                if let Err(err) = check_kind_name(&lobby) {
                    problem("lobby", err);
                } else if raw.role == Role::Game && lobby == raw.name {
                    problem("lobby", "a game can't return players to itself".into());
                }
                Some(lobby)
            }
            (Role::Lobby, None) => None,
        };

        let base = match VersionReq::parse(&raw.base) {
            Ok(base) => Some(base),
            Err(err) => {
                problem(
                    "base",
                    format!(
                        "\"{}\" isn't a version range like \"^0.1\": {err}",
                        raw.base
                    ),
                );
                None
            }
        };

        let world = raw.world.and_then(|world| match relative_path(&world) {
            Ok(path) => Some(path),
            Err(err) => {
                problem("world", err);
                None
            }
        });

        if raw.role == Role::Game && !raw.queues.is_empty() {
            problem("queues", "only hubs and lobbies queue players".into());
        }
        let mut seen = BTreeSet::new();
        for queue in &raw.queues {
            if let Err(err) = check_kind_name(queue) {
                problem("queues", err);
            } else if *queue == raw.name {
                problem("queues", format!("\"{queue}\" is this server itself"));
            } else if !seen.insert(queue) {
                problem("queues", format!("\"{queue}\" is listed twice"));
            }
        }

        let max = raw.players.max;
        if max == 0 {
            problem("players.max", "must be at least 1".into());
        }
        let min = match (raw.role, raw.players.min) {
            (Role::Game, Some(min)) => {
                if min == 0 {
                    problem("players.min", "must be at least 1".into());
                } else if min > max {
                    problem(
                        "players.min",
                        format!("{min} is more than players.max ({max})"),
                    );
                }
                min
            }
            (Role::Game, None) => 1,
            (role, Some(_)) => {
                problem(
                    "players.min",
                    format!("only games have a minimum, not a {role}"),
                );
                1
            }
            (_, None) => 1,
        };

        // A hub is the network's floor (Decisions 19): keep one up by default.
        let warm = raw
            .scaling
            .warm
            .unwrap_or(if raw.role == Role::Hub { 1 } else { 0 });
        if raw.role == Role::Hub && warm == 0 {
            problem(
                "scaling.warm",
                "a hub needs at least one instance ready for players to land in".into(),
            );
        }
        if let Some(max_instances) = raw.scaling.max_instances {
            if max_instances == 0 {
                problem("scaling.max_instances", "must be at least 1".into());
            } else if warm > max_instances {
                problem(
                    "scaling.warm",
                    format!("{warm} is more than scaling.max_instances ({max_instances})"),
                );
            }
        }

        let maps = match raw.maps {
            Some(_) if raw.role != Role::Game => {
                problem("maps", format!("only games pick maps, not a {}", raw.role));
                None
            }
            Some(maps) => {
                let dir = maps.dir.and_then(|dir| match relative_path(&dir) {
                    Ok(path) => Some(path),
                    Err(err) => {
                        problem("maps.dir", err);
                        None
                    }
                });
                Some(Maps {
                    dir,
                    pick: maps.pick,
                })
            }
            None => None,
        };

        let mut plugins = Vec::new();
        let mut names = BTreeSet::new();
        for (i, plugin) in raw.plugins.into_iter().enumerate() {
            let field = format!("plugins[{i}]");
            let file = match relative_path(&plugin.file) {
                Ok(file) if file.extension().is_some_and(|ext| ext == "wasm") => file,
                Ok(_) => {
                    problem(
                        &format!("{field}.file"),
                        format!("\"{}\" isn't a .wasm file", plugin.file),
                    );
                    continue;
                }
                Err(err) => {
                    problem(&format!("{field}.file"), err);
                    continue;
                }
            };
            let name = match plugin.name {
                Some(name) => name,
                None => file
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            };
            if let Err(err) = check_plugin_name(&name) {
                problem(&format!("{field}.name"), err);
            } else if BASE_PLUGINS.contains(&name.as_str()) {
                problem(
                    &format!("{field}.name"),
                    format!("{name} comes with the base image"),
                );
            } else if let Some((_, why)) = REFUSED_PLUGINS.iter().find(|(n, _)| *n == name) {
                problem(&format!("{field}.name"), format!("{name}: {why}"));
            } else if !names.insert(name.clone()) {
                problem(&format!("{field}.name"), format!("{name} is listed twice"));
            }
            for permission in &plugin.permissions {
                if permission.trim().is_empty() || permission.contains(char::is_whitespace) {
                    problem(
                        &format!("{field}.permissions"),
                        format!("\"{permission}\" isn't a permission name"),
                    );
                }
            }
            plugins.push(Plugin {
                file,
                name,
                permissions: plugin.permissions,
            });
        }
        // hallow-sdk loads a map as a world one folder up from Pumpkin's world
        // folder (`../plugins/data/...`), so that folder must be one deep.
        if maps.as_ref().is_some_and(|maps| maps.dir.is_some())
            && world
                .as_ref()
                .is_some_and(|world: &PathBuf| world.components().count() > 1)
        {
            problem(
                "world",
                "must be a single folder in a game with maps.dir (hallow-sdk loads maps next to it)"
                    .to_owned(),
            );
        }
        // The maps are copied into the first plugin's data folder, the only
        // place a plugin can read files.
        if maps.as_ref().is_some_and(|maps| maps.dir.is_some()) && plugins.is_empty() {
            problem(
                "maps.dir",
                "needs a [[plugins]] entry: the maps are copied into the first plugin's data folder"
                    .to_owned(),
            );
        }

        for (key, why) in RESERVED_PUMPKIN_KEYS {
            if has_key(&raw.pumpkin, key) {
                problem(&format!("pumpkin.{key}"), format!("can't be set: {why}"));
            }
        }
        for plugin in &plugins {
            let key = format!("plugins.overrides.{}.allowed_permissions", plugin.name);
            if !plugin.permissions.is_empty() && has_key(&raw.pumpkin, &key) {
                problem(
                    &format!("pumpkin.{key}"),
                    "set the plugin's permissions in [[plugins]] instead".into(),
                );
            }
        }

        for key in raw.env.keys() {
            if key.starts_with("HALLOW_") {
                problem(
                    &format!("env.{key}"),
                    "HALLOW_ variables come from hallow.toml's own fields or from the instance"
                        .into(),
                );
            } else if !is_env_name(key) {
                problem(
                    &format!("env.{key}"),
                    "not a variable name (letters, digits and _, not starting with a digit)".into(),
                );
            }
        }

        match base {
            Some(base) if problems.is_empty() => Ok(Self {
                name: raw.name,
                role: raw.role,
                lobby,
                base,
                world,
                queues: raw.queues,
                players: Players { min, max },
                scaling: Scaling {
                    warm,
                    max_instances: raw.scaling.max_instances,
                },
                maps,
                plugins,
                pumpkin: raw.pumpkin,
                env: raw.env,
            }),
            _ => Err(Error::Invalid(problems)),
        }
    }

    /// Reads and parses a manifest file.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = std::fs::read_to_string(path)
            .map_err(|err| Error::Parse(format!("can't read {}: {err}", path.display())))?;
        Self::parse(&text)
    }

    /// Checks that the files and folders it names exist under `dir`, the
    /// folder the manifest is in.
    pub fn check_files(&self, dir: &Path) -> Result<(), Error> {
        let mut problems = Vec::new();
        for (i, plugin) in self.plugins.iter().enumerate() {
            if !dir.join(&plugin.file).is_file() {
                problems.push(format!(
                    "plugins[{i}].file: {} doesn't exist",
                    plugin.file.display()
                ));
            }
        }
        if let Some(world) = &self.world
            && !dir.join(world).is_dir()
        {
            problems.push(format!("world: folder {} doesn't exist", world.display()));
        }
        if let Some(maps_dir) = self.maps.as_ref().and_then(|maps| maps.dir.as_ref())
            && !dir.join(maps_dir).is_dir()
        {
            problems.push(format!(
                "maps.dir: folder {} doesn't exist",
                maps_dir.display()
            ));
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(Error::Invalid(problems))
        }
    }

    /// Checks a base image version against `base`.
    pub fn check_base(&self, version: &Version) -> Result<(), String> {
        if self.base.matches(version) {
            Ok(())
        } else {
            Err(format!(
                "base: this repo works with base image {}, not {version}",
                self.base
            ))
        }
    }

    /// The base image's `pumpkin.toml` with this manifest applied: `[pumpkin]`
    /// merged over it, then `players.max`, `world` and the plugins'
    /// permissions.
    pub fn pumpkin_config(&self, mut base: toml::Table) -> toml::Table {
        merge(&mut base, self.pumpkin.clone());
        set(
            &mut base,
            &["networking", "java", "max_players"],
            toml::Value::Integer(self.players.max.into()),
        );
        if let Some(world) = &self.world {
            set(
                &mut base,
                &["default_level_name"],
                toml::Value::String(world.to_string_lossy().into_owned()),
            );
        }
        for plugin in &self.plugins {
            if plugin.permissions.is_empty() {
                continue;
            }
            let permissions = plugin
                .permissions
                .iter()
                .cloned()
                .map(toml::Value::String)
                .collect();
            set(
                &mut base,
                &["plugins", "overrides", &plugin.name, "allowed_permissions"],
                toml::Value::Array(permissions),
            );
        }
        base
    }

    /// Environment the server starts with, read by Hallow's server plugin,
    /// then `[env]`. The instance's own environment wins over all of it.
    pub fn env(&self) -> Vec<(String, String)> {
        let mut env = vec![
            ("HALLOW_SERVER_KIND".to_owned(), self.name.clone()),
            (
                "HALLOW_SERVER_ROLE".to_owned(),
                match self.role.server_role() {
                    ServerRole::Lobby => "lobby",
                    ServerRole::Game => "game",
                }
                .to_owned(),
            ),
            (
                "HALLOW_MAX_PLAYERS".to_owned(),
                self.players.max.to_string(),
            ),
        ];
        if let Some(lobby) = &self.lobby {
            env.push(("HALLOW_RETURN_LOBBY".to_owned(), lobby.clone()));
        }
        // For hallow-sdk: when a match can start, and how its map is picked.
        if self.role == Role::Game {
            env.push((
                "HALLOW_MIN_PLAYERS".to_owned(),
                self.players.min.to_string(),
            ));
            if let Some(maps) = &self.maps {
                let pick = match maps.pick {
                    MapPick::Random => "random",
                    MapPick::Vote => "vote",
                };
                env.push(("HALLOW_MAPS_PICK".to_owned(), pick.to_owned()));
            }
        }
        env.extend(self.env.iter().map(|(k, v)| (k.clone(), v.clone())));
        env
    }
}

/// A server kind name: it names an image on GHCR and a kind in the control
/// plane, so lowercase letters, digits and single dashes.
fn check_kind_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 40
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && !name.ends_with('-')
        && !name.contains("--")
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "\"{name}\" isn't a server kind name: up to 40 lowercase letters, digits and \
             single dashes, starting with a letter"
        ))
    }
}

fn check_plugin_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "\"{name}\" isn't a plugin name: letters, digits, _ and -, starting with a letter"
        ))
    }
}

fn is_env_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A path inside the repo: relative, without `..`.
fn relative_path(path: &str) -> Result<PathBuf, String> {
    let parsed = PathBuf::from(path);
    if path.trim().is_empty() {
        return Err("is empty".into());
    }
    if parsed
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(format!(
            "\"{path}\" must be a path inside the repo, relative to hallow.toml"
        ));
    }
    Ok(parsed)
}

/// Whether a dotted key is set in a table.
fn has_key(table: &toml::Table, dotted: &str) -> bool {
    let mut table = table;
    let mut parts = dotted.split('.').peekable();
    while let Some(part) = parts.next() {
        match table.get(part) {
            None => return false,
            Some(_) if parts.peek().is_none() => return true,
            Some(toml::Value::Table(inner)) => table = inner,
            Some(_) => return false,
        }
    }
    false
}

/// Deep-merges `over` into `base`: tables merge, everything else replaces.
fn merge(base: &mut toml::Table, over: toml::Table) {
    for (key, value) in over {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(base)), toml::Value::Table(over)) => merge(base, over),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// Sets a nested key, creating (or replacing non-table values with) tables
/// on the way.
fn set(table: &mut toml::Table, path: &[&str], value: toml::Value) {
    let (last, parents) = path.split_last().expect("a non-empty path");
    let mut table = table;
    for part in parents {
        let entry = table
            .entry(part.to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !entry.is_table() {
            *entry = toml::Value::Table(toml::Table::new());
        }
        table = entry.as_table_mut().expect("just made a table");
    }
    table.insert(last.to_string(), value);
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAME: &str = r#"
        name = "skywars"
        role = "game"
        lobby = "skywars-lobby"
        base = "^0.1"
        world = "world"

        [players]
        min = 2
        max = 12

        [scaling]
        warm = 1
        max_instances = 200

        [maps]
        dir = "maps"
        pick = "vote"

        [[plugins]]
        file = "plugins/skywars.wasm"
        permissions = ["fs.read.data"]

        [pumpkin]
        default_gamemode = "adventure"

        [pumpkin.networking.java]
        view_distance = 6

        [env]
        SKYWARS_MODE = "solo"
    "#;

    fn problems(text: &str) -> Vec<String> {
        match Manifest::parse(text) {
            Err(Error::Invalid(problems)) => problems,
            other => panic!("expected problems, got {other:?}"),
        }
    }

    fn assert_problem(text: &str, expect: &str) {
        let problems = problems(text);
        assert!(
            problems.iter().any(|p| p.contains(expect)),
            "no problem containing {expect:?} in {problems:#?}"
        );
    }

    /// A minimal valid manifest with `extra` lines added at the top level.
    fn hub(extra: &str) -> String {
        format!("name = \"hub\"\nrole = \"hub\"\nbase = \"^0.1\"\n{extra}\n[players]\nmax = 100\n")
    }

    #[test]
    fn full_game() {
        let manifest = Manifest::parse(GAME).unwrap();
        assert_eq!(manifest.name, "skywars");
        assert_eq!(manifest.role, Role::Game);
        assert_eq!(manifest.role.server_role(), ServerRole::Game);
        assert_eq!(manifest.lobby.as_deref(), Some("skywars-lobby"));
        assert!(manifest.base.matches(&Version::new(0, 1, 4)));
        assert_eq!(manifest.world, Some(PathBuf::from("world")));
        assert_eq!(manifest.players, Players { min: 2, max: 12 });
        assert_eq!(
            manifest.scaling,
            Scaling {
                warm: 1,
                max_instances: Some(200)
            }
        );
        assert_eq!(
            manifest.maps,
            Some(Maps {
                dir: Some(PathBuf::from("maps")),
                pick: MapPick::Vote
            })
        );
        assert_eq!(
            manifest.plugins,
            vec![Plugin {
                file: PathBuf::from("plugins/skywars.wasm"),
                name: "skywars".into(),
                permissions: vec!["fs.read.data".into()],
            }]
        );
        assert_eq!(manifest.env["SKYWARS_MODE"], "solo");
        let env = manifest.env();
        assert!(env.contains(&("HALLOW_MIN_PLAYERS".into(), "2".into())));
        assert!(env.contains(&("HALLOW_MAPS_PICK".into(), "vote".into())));
    }

    #[test]
    fn maps_need_a_plugin() {
        assert_problem(
            "name = \"g\"\nrole = \"game\"\nbase = \"^0.1\"\n[players]\nmax = 4\n[maps]\ndir = \"maps\"\n",
            "needs a [[plugins]] entry",
        );
    }

    #[test]
    fn hub_defaults() {
        let manifest = Manifest::parse(&hub("")).unwrap();
        assert_eq!(manifest.role.server_role(), ServerRole::Lobby);
        assert_eq!(manifest.lobby.as_deref(), Some("hub"));
        assert_eq!(manifest.players, Players { min: 1, max: 100 });
        assert_eq!(
            manifest.scaling,
            Scaling {
                warm: 1,
                max_instances: None
            }
        );
        assert!(manifest.plugins.is_empty() && manifest.maps.is_none());
        assert_eq!(
            manifest.env(),
            vec![
                ("HALLOW_SERVER_KIND".into(), "hub".into()),
                ("HALLOW_SERVER_ROLE".into(), "lobby".into()),
                ("HALLOW_MAX_PLAYERS".into(), "100".into()),
                ("HALLOW_RETURN_LOBBY".into(), "hub".into()),
            ]
        );
    }

    #[test]
    fn lobby_with_queues() {
        let manifest = Manifest::parse(
            r#"
            name = "skywars-lobby"
            role = "lobby"
            base = ">=0.1, <0.3"
            queues = ["skywars"]
            [players]
            max = 80
            "#,
        )
        .unwrap();
        assert_eq!(manifest.queues, ["skywars"]);
        assert_eq!(manifest.lobby, None);
        assert_eq!(manifest.scaling.warm, 0);
        assert!(
            !manifest
                .env()
                .iter()
                .any(|(k, _)| k == "HALLOW_RETURN_LOBBY")
        );
    }

    #[test]
    fn parse_errors_name_the_line() {
        let err = Manifest::parse("name = \"hub\"\nrole = \"hub\"\nbase = \"^0.1\"\n[players]\nmax = 100\nmin_players = 2\n")
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("unknown field `min_players`"), "{message}");
        assert!(message.contains("line 6"), "{message}");

        let err =
            Manifest::parse(&hub("").replace("\"hub\"\nbase", "\"proxy\"\nbase")).unwrap_err();
        assert!(err.to_string().contains("unknown variant `proxy`"), "{err}");

        let err = Manifest::parse("role = \"hub\"").unwrap_err();
        assert!(err.to_string().contains("missing field `name`"), "{err}");
    }

    #[test]
    fn collects_every_problem() {
        let problems = problems(
            r#"
            name = "SkyWars"
            role = "game"
            base = "latest"
            [players]
            min = 20
            max = 12
            "#,
        );
        assert_eq!(problems.len(), 4, "{problems:#?}");
        assert!(problems[0].starts_with("name: "));
        assert!(problems[1].starts_with("lobby: "));
        assert!(problems[2].starts_with("base: "));
        assert!(problems[3].starts_with("players.min: "));
        let message = Error::Invalid(problems).to_string();
        assert!(message.starts_with("4 problems:\n  - name: "), "{message}");
    }

    #[test]
    fn role_rules() {
        assert_problem(
            &hub("lobby = \"elsewhere\""),
            "a hub returns players to itself",
        );
        assert!(Manifest::parse(&hub("lobby = \"hub\"")).is_ok());
        assert_problem(&hub("queues = [\"hub\"]"), "is this server itself");
        assert_problem(&hub("queues = [\"a\", \"a\"]"), "listed twice");
        assert_problem(&hub("[maps]\npick = \"vote\""), "only games pick maps");
        assert_problem(&hub("[scaling]\nwarm = 0"), "a hub needs at least one");
        assert_problem(
            &GAME.replace("lobby = \"skywars-lobby\"", "lobby = \"skywars\""),
            "can't return players to itself",
        );
        assert_problem(
            &GAME.replace("world = \"world\"", "queues = [\"x\"]"),
            "only hubs and lobbies queue",
        );
        assert_problem(
            &hub("").replace("max = 100", "max = 100\nmin = 2"),
            "only games have a minimum",
        );
    }

    #[test]
    fn limits() {
        assert_problem(
            &hub("").replace("max = 100", "max = 0"),
            "players.max: must be at least 1",
        );
        assert_problem(
            &GAME.replace("min = 2", "min = 0"),
            "players.min: must be at least 1",
        );
        assert_problem(
            &GAME.replace("max_instances = 200", "max_instances = 0"),
            "scaling.max_instances: must be at least 1",
        );
        assert_problem(
            &GAME.replace("warm = 1", "warm = 300"),
            "more than scaling.max_instances",
        );
    }

    #[test]
    fn names() {
        for bad in ["", "Hub", "1hub", "hub-", "h--b", "hub_1", &"a".repeat(41)] {
            assert!(check_kind_name(bad).is_err(), "{bad:?}");
        }
        for good in ["hub", "skywars-lobby", "bedwars4", &"a".repeat(40)] {
            assert!(check_kind_name(good).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn paths_stay_in_the_repo() {
        assert_problem(
            &hub("world = \"../world\""),
            "must be a path inside the repo",
        );
        assert_problem(
            &hub("world = \"/data/world\""),
            "must be a path inside the repo",
        );
        assert_problem(&hub("world = \"\""), "world: is empty");
        let plugin = |file: &str| hub(&format!("[[plugins]]\nfile = \"{file}\""));
        assert_problem(&plugin("plugins/a.jar"), "isn't a .wasm file");
        assert_problem(&plugin("../a.wasm"), "must be a path inside the repo");
        assert!(Manifest::parse(&plugin("./plugins/a.wasm")).is_ok());
    }

    #[test]
    fn plugin_rules() {
        assert_problem(
            &hub("[[plugins]]\nfile = \"hallow_server.wasm\""),
            "comes with the base image",
        );
        assert_problem(
            &hub("[[plugins]]\nfile = \"plugins/pumpkin_java_multiversion.wasm\""),
            "translated by the proxy",
        );
        assert_problem(
            &hub("[[plugins]]\nfile = \"a/x.wasm\"\n[[plugins]]\nfile = \"b/x.wasm\""),
            "x is listed twice",
        );
        assert_problem(
            &hub("[[plugins]]\nfile = \"a.wasm\"\nname = \"my plugin\""),
            "isn't a plugin name",
        );
        assert_problem(
            &hub("[[plugins]]\nfile = \"a.wasm\"\npermissions = [\"\"]"),
            "isn't a permission name",
        );
    }

    #[test]
    fn reserved_pumpkin_keys() {
        for (table, key, expect) in [
            (
                "[pumpkin.networking.proxy.vine]",
                "secret = \"x\"",
                "pumpkin.networking.proxy",
            ),
            (
                "[pumpkin.networking.java]",
                "max_players = 5",
                "set players.max instead",
            ),
            (
                "[pumpkin]",
                "default_level_name = \"w\"",
                "set world instead",
            ),
            ("[pumpkin.telemetry]", "enabled = true", "Decisions 24"),
            (
                "[pumpkin.plugins.overrides.hallow_server]",
                "enabled = false",
                "configures Hallow's plugin",
            ),
        ] {
            assert_problem(&hub(&format!("{table}\n{key}")), expect);
        }
        assert_problem(
            &hub(
                "[[plugins]]\nfile = \"a.wasm\"\npermissions = [\"fs.read.data\"]\n\
                 [pumpkin.plugins.overrides.a]\nallowed_permissions = []",
            ),
            "in [[plugins]] instead",
        );
        // Other keys under the same tables are fine.
        assert!(Manifest::parse(&hub("[pumpkin.networking.java]\nview_distance = 8")).is_ok());
    }

    #[test]
    fn env_rules() {
        assert_problem(
            &hub("[env]\nHALLOW_SERVER_NAME = \"x\""),
            "HALLOW_ variables",
        );
        assert_problem(&hub("[env]\n\"9LIVES\" = \"x\""), "not a variable name");
        assert_problem(&hub("[env]\n\"A-B\" = \"x\""), "not a variable name");
        let err = Manifest::parse(&hub("[env]\nCOUNT = 3")).unwrap_err();
        assert!(err.to_string().contains("expected a string"), "{err}");
    }

    #[test]
    fn base_versions() {
        let manifest = Manifest::parse(GAME).unwrap();
        assert!(manifest.check_base(&Version::new(0, 1, 0)).is_ok());
        assert!(manifest.check_base(&Version::new(0, 1, 9)).is_ok());
        let err = manifest.check_base(&Version::new(0, 2, 0)).unwrap_err();
        assert_eq!(err, "base: this repo works with base image ^0.1, not 0.2.0");
    }

    #[test]
    fn renders_pumpkin_config() {
        let base: toml::Table = toml::from_str(
            r#"
            [networking.java]
            address = "0.0.0.0:25565"
            online_mode = false
            [networking.proxy.vine]
            enabled = true
            [plugins.overrides.hallow_server]
            allowed_permissions = ["http.outbound"]
            "#,
        )
        .unwrap();
        let config = Manifest::parse(GAME).unwrap().pumpkin_config(base);
        let java = config["networking"]["java"].as_table().unwrap();
        assert_eq!(java["address"].as_str(), Some("0.0.0.0:25565"));
        assert_eq!(java["online_mode"].as_bool(), Some(false));
        assert_eq!(java["view_distance"].as_integer(), Some(6));
        assert_eq!(java["max_players"].as_integer(), Some(12));
        assert_eq!(
            config["networking"]["proxy"]["vine"]["enabled"].as_bool(),
            Some(true)
        );
        assert_eq!(config["default_gamemode"].as_str(), Some("adventure"));
        assert_eq!(config["default_level_name"].as_str(), Some("world"));
        let overrides = config["plugins"]["overrides"].as_table().unwrap();
        assert_eq!(
            overrides["hallow_server"]["allowed_permissions"][0].as_str(),
            Some("http.outbound")
        );
        assert_eq!(
            overrides["skywars"]["allowed_permissions"][0].as_str(),
            Some("fs.read.data")
        );
    }

    #[test]
    fn checks_files() {
        let dir = std::env::temp_dir().join(format!("hallow-manifest-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("plugins")).unwrap();
        let manifest = Manifest::parse(GAME).unwrap();
        let err = manifest.check_files(&dir).unwrap_err();
        assert_eq!(
            err,
            Error::Invalid(vec![
                "plugins[0].file: plugins/skywars.wasm doesn't exist".into(),
                "world: folder world doesn't exist".into(),
                "maps.dir: folder maps doesn't exist".into(),
            ])
        );
        std::fs::write(dir.join("plugins/skywars.wasm"), b"\0asm").unwrap();
        std::fs::create_dir_all(dir.join("world")).unwrap();
        std::fs::create_dir_all(dir.join("maps")).unwrap();
        assert_eq!(manifest.check_files(&dir), Ok(()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The in-tree servers and the template must stay valid. A plugin that
    /// isn't built yet must be the one the server's own `plugin/` crate
    /// builds (`servers/build-plugins.sh` builds it before the images).
    #[test]
    fn repo_manifests_are_valid() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut kinds = std::collections::BTreeMap::new();
        for dir in [
            "servers/hub",
            "servers/test-game",
            "servers/test-game-lobby",
            "templates/server-repo",
        ] {
            let dir = root.join(dir);
            let manifest = Manifest::load(&dir.join(FILE_NAME))
                .unwrap_or_else(|err| panic!("{}: {err}", dir.display()));
            for plugin in &manifest.plugins {
                let built = dir.join(&plugin.file);
                if built.is_file() {
                    continue;
                }
                let crate_toml = std::fs::read_to_string(dir.join("plugin/Cargo.toml"))
                    .unwrap_or_else(|_| {
                        panic!(
                            "{}: {} isn't built and there's no plugin/ crate",
                            dir.display(),
                            plugin.file.display()
                        )
                    });
                let package: toml::Table = toml::from_str(&crate_toml).unwrap();
                let name = package["package"]["name"]
                    .as_str()
                    .unwrap()
                    .replace('-', "_");
                assert_eq!(
                    plugin.file,
                    Path::new("plugins").join(format!("{name}.wasm")),
                    "{}: plugin/ builds {name}.wasm",
                    dir.display()
                );
            }
            kinds.insert(manifest.name.clone(), manifest);
        }
        // The dev network's kinds point at each other.
        assert_eq!(kinds["hub"].role, Role::Hub);
        assert_eq!(kinds["test-game"].lobby.as_deref(), Some("test-game-lobby"));
        let lobby = &kinds["test-game-lobby"];
        assert_eq!(lobby.role, Role::Lobby);
        assert_eq!(lobby.queues, ["test-game"]);
        assert_eq!(lobby.lobby.as_deref(), Some("hub"));
    }
}
