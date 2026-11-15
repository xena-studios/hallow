//! Settings come from environment variables, then from `config.toml` in the
//! plugin's data folder (`plugins/data/hallow_server/`). A variable wins over
//! the same key in the file. Pumpkin only passes a plugin the variables it
//! asks for (`sys.env.<NAME>` permissions), which are the ones below.
//!
//! | Variable | `config.toml` key | Default |
//! |---|---|---|
//! | `HALLOW_CONTROL_PLANE` | `control_plane` | none: the plugin stays off |
//! | `HALLOW_CONTROL_PLANE_TOKEN` | `token` | |
//! | `HALLOW_CONTROL_PLANE_TOKEN_FILE` | `token_file` | a file in the data folder |
//! | `HALLOW_SERVER_NAME` | `name` | required |
//! | `HALLOW_SERVER_KIND` | `kind` | the name |
//! | `HALLOW_SERVER_VERSION` | `version` | `dev` |
//! | `HALLOW_SERVER_ROLE` | `role` | `game` (or `lobby`) |
//! | `HALLOW_SERVER_ADDRESS` | `address` | `<name>:25565` |
//! | `HALLOW_MAX_PLAYERS` | `max_players` | `100` |
//!
//! ```toml
//! control_plane = "control-plane:8080"   # host:port, plain HTTP on the private network
//! token_file = "token"                   # or token = "..."
//! name = "lobby"                         # unique per running server
//! kind = "hub"                           # the server repo
//! version = "v1.0.0"                     # the repo's git tag
//! role = "lobby"                         # lobby or game
//! address = "lobby:25565"                # where proxies reach this server
//! max_players = 100
//! ```
//!
//! Without `control_plane` set anywhere the plugin loads and does nothing, so
//! the base image can carry it for servers that aren't on a network.

use hallow_proto::server::ServerRole;
use serde::Deserialize;

pub const FILE: &str = "config.toml";

/// Every variable the plugin reads; each needs a `sys.env.<NAME>` permission.
pub const ENV_VARS: [&str; 9] = [
    CONTROL_PLANE,
    TOKEN,
    TOKEN_FILE,
    NAME,
    KIND,
    VERSION,
    ROLE,
    ADDRESS,
    MAX_PLAYERS,
];

const CONTROL_PLANE: &str = "HALLOW_CONTROL_PLANE";
const TOKEN: &str = "HALLOW_CONTROL_PLANE_TOKEN";
const TOKEN_FILE: &str = "HALLOW_CONTROL_PLANE_TOKEN_FILE";
const NAME: &str = "HALLOW_SERVER_NAME";
const KIND: &str = "HALLOW_SERVER_KIND";
const VERSION: &str = "HALLOW_SERVER_VERSION";
const ROLE: &str = "HALLOW_SERVER_ROLE";
const ADDRESS: &str = "HALLOW_SERVER_ADDRESS";
const MAX_PLAYERS: &str = "HALLOW_MAX_PLAYERS";

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    control_plane: Option<String>,
    token: Option<String>,
    token_file: Option<String>,
    name: Option<String>,
    kind: Option<String>,
    version: Option<String>,
    role: Option<String>,
    address: Option<String>,
    max_players: Option<u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Config {
    /// `host:port` of the control plane.
    pub control_plane: String,
    pub token: String,
    pub name: String,
    pub kind: String,
    pub version: String,
    pub role: ServerRole,
    pub address: String,
    pub max_players: u32,
}

/// Where settings come from, so tests can stand in for the sandbox.
pub trait Source {
    fn var(&self, name: &str) -> Option<String>;
    /// A file in the data folder, by name.
    fn read(&self, file: &str) -> Result<Option<String>, String>;
}

/// The real environment and data folder.
pub struct Sandbox {
    pub data_folder: String,
}

impl Source for Sandbox {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    fn read(&self, file: &str) -> Result<Option<String>, String> {
        let path = format!("{}/{file}", self.data_folder);
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(text)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(format!("can't read {path}: {err}")),
        }
    }
}

impl Config {
    /// `Ok(None)` when no control plane is configured: the plugin stays off.
    pub fn load(source: &impl Source) -> Result<Option<Self>, String> {
        let file: File = match source.read(FILE)? {
            Some(text) => toml::from_str(&text).map_err(|err| format!("{FILE}: {err}"))?,
            None => File::default(),
        };
        // A set but empty variable counts as unset.
        let var = |name: &str| source.var(name).filter(|value| !value.trim().is_empty());
        let pick = |name: &str, from_file: Option<String>| var(name).or(from_file);

        let Some(control_plane) = pick(CONTROL_PLANE, file.control_plane) else {
            return Ok(None);
        };
        let name = pick(NAME, file.name).ok_or(format!("set {NAME} or name in {FILE}"))?;

        // The variables override the file as a pair, so a token in the
        // environment beats a token_file in config.toml.
        let (token, token_file) = if var(TOKEN).is_some() || var(TOKEN_FILE).is_some() {
            (var(TOKEN), var(TOKEN_FILE))
        } else {
            (file.token, file.token_file)
        };
        let token = match (token, token_file) {
            (Some(_), Some(_)) => return Err("set token or token_file, not both".into()),
            (Some(token), None) => token,
            (None, Some(file)) => source
                .read(&file)?
                .ok_or(format!("token file {file} isn't in the data folder"))?,
            (None, None) => return Err(format!("set {TOKEN} or {TOKEN_FILE}")),
        };
        let token = token.trim().to_owned();
        if token.is_empty() {
            return Err("the control-plane token is empty".into());
        }

        let role = match pick(ROLE, file.role).as_deref() {
            None | Some("game") => ServerRole::Game,
            Some("lobby") => ServerRole::Lobby,
            Some(other) => return Err(format!("role must be lobby or game, not {other}")),
        };
        let max_players = match var(MAX_PLAYERS) {
            Some(value) => value
                .trim()
                .parse()
                .map_err(|_| format!("{MAX_PLAYERS} must be a number, not {value}"))?,
            None => file.max_players.unwrap_or(100),
        };

        Ok(Some(Self {
            control_plane,
            token,
            kind: pick(KIND, file.kind).unwrap_or_else(|| name.clone()),
            version: pick(VERSION, file.version).unwrap_or_else(|| "dev".into()),
            role,
            address: pick(ADDRESS, file.address).unwrap_or_else(|| format!("{name}:25565")),
            max_players,
            name,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[derive(Default)]
    struct Fake {
        vars: HashMap<&'static str, &'static str>,
        files: HashMap<&'static str, &'static str>,
    }

    impl Source for Fake {
        fn var(&self, name: &str) -> Option<String> {
            self.vars.get(name).map(|value| (*value).to_owned())
        }

        fn read(&self, file: &str) -> Result<Option<String>, String> {
            Ok(self.files.get(file).map(|text| (*text).to_owned()))
        }
    }

    #[test]
    fn off_without_a_control_plane() {
        let source = Fake {
            vars: HashMap::from([(NAME, "lobby")]),
            ..Fake::default()
        };
        assert_eq!(Config::load(&source), Ok(None));
    }

    #[test]
    fn environment_only_with_defaults() {
        let source = Fake {
            vars: HashMap::from([
                (CONTROL_PLANE, "cp:8080"),
                (TOKEN, " t\n"),
                (NAME, "skywars-7f3a"),
            ]),
            ..Fake::default()
        };
        let config = Config::load(&source).unwrap().unwrap();
        assert_eq!(
            config,
            Config {
                control_plane: "cp:8080".into(),
                token: "t".into(),
                name: "skywars-7f3a".into(),
                kind: "skywars-7f3a".into(),
                version: "dev".into(),
                role: ServerRole::Game,
                address: "skywars-7f3a:25565".into(),
                max_players: 100,
            }
        );
    }

    #[test]
    fn file_with_environment_overrides() {
        let source = Fake {
            vars: HashMap::from([(NAME, "lobby-2"), (MAX_PLAYERS, "50"), (TOKEN_FILE, "")]),
            files: HashMap::from([
                (
                    FILE,
                    r#"
                    control_plane = "cp:8080"
                    token_file = "token"
                    name = "lobby"
                    kind = "hub"
                    role = "lobby"
                    max_players = 200
                    "#,
                ),
                ("token", "secret\n"),
            ]),
        };
        let config = Config::load(&source).unwrap().unwrap();
        assert_eq!(config.name, "lobby-2");
        assert_eq!(config.kind, "hub");
        assert_eq!(config.role, ServerRole::Lobby);
        assert_eq!(config.token, "secret");
        assert_eq!(config.max_players, 50);
        assert_eq!(config.address, "lobby-2:25565");
    }

    #[test]
    fn rejects_bad_values() {
        let base = [(CONTROL_PLANE, "cp:8080"), (TOKEN, "t"), (NAME, "a")];
        for (var, value, expect) in [
            (ROLE, "hub", "role must be"),
            (MAX_PLAYERS, "lots", "must be a number"),
            (TOKEN_FILE, "token", "not both"),
        ] {
            let mut vars = HashMap::from(base);
            vars.insert(var, value);
            let err = Config::load(&Fake {
                vars,
                ..Fake::default()
            })
            .unwrap_err();
            assert!(err.contains(expect), "{err}");
        }
        let err = Config::load(&Fake {
            vars: HashMap::from([(CONTROL_PLANE, "cp:8080"), (NAME, "a")]),
            ..Fake::default()
        })
        .unwrap_err();
        assert!(err.contains(TOKEN), "{err}");
    }
}
