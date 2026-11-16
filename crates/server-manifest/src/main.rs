//! `hallow-manifest`: checks a server repo's `hallow.toml` (in CI and image
//! builds) and applies it when a server container starts.
//!
//! ```text
//! hallow-manifest check [PATH] [--base-version VERSION]
//! hallow-manifest apply PATH --pumpkin FILE --plugins DIR [--base-version VERSION]
//! ```
//!
//! `check` validates the manifest and the files it names; PATH is the file
//! or the repo folder (default `.`). `apply` is what the base image's
//! entrypoint runs: it merges the manifest into the rendered `pumpkin.toml`
//! in place, copies the repo's plugins into Pumpkin's plugin folder and
//! prints `export` lines for the server's environment, skipping variables the
//! container already sets. Both exit 1 on an invalid manifest and 2 on bad
//! arguments.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hallow_manifest::{FILE_NAME, Manifest};
use semver::Version;

const USAGE: &str = "usage:
  hallow-manifest check [PATH] [--base-version VERSION]
  hallow-manifest apply PATH --pumpkin FILE --plugins DIR [--base-version VERSION]";

#[derive(Debug, Default, PartialEq)]
struct Args {
    command: String,
    path: Option<PathBuf>,
    pumpkin: Option<PathBuf>,
    plugins: Option<PathBuf>,
    base_version: Option<String>,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut args = args.into_iter();
    let mut parsed = Args {
        command: args.next().ok_or("no command")?,
        ..Args::default()
    };
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().ok_or(format!("{flag} needs a value"));
        match arg.as_str() {
            "--pumpkin" => parsed.pumpkin = Some(value(&arg)?.into()),
            "--plugins" => parsed.plugins = Some(value(&arg)?.into()),
            "--base-version" => parsed.base_version = Some(value(&arg)?),
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            _ if parsed.path.is_none() => parsed.path = Some(arg.into()),
            _ => return Err(format!("unexpected argument {arg}")),
        }
    }
    Ok(parsed)
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) if args.command == "check" || args.command == "apply" => args,
        Ok(args) if args.command == "-h" || args.command == "--help" => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(args) => return usage(&format!("unknown command {}", args.command)),
        Err(err) => return usage(&err),
    };
    let base_version = match args.base_version.as_deref().map(Version::parse) {
        None => None,
        Some(Ok(version)) => Some(version),
        Some(Err(err)) => return usage(&format!("--base-version: {err}")),
    };

    let path = match args.path.clone() {
        None => PathBuf::from(FILE_NAME),
        Some(path) if path.is_dir() => path.join(FILE_NAME),
        Some(path) => path,
    };
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    let manifest = Manifest::load(&path)
        .and_then(|manifest| manifest.check_files(dir).map(|()| manifest))
        .map_err(|err| err.to_string())
        .and_then(|manifest| match &base_version {
            Some(version) => manifest.check_base(version).map(|()| manifest),
            None => Ok(manifest),
        });
    let manifest = match manifest {
        Ok(manifest) => manifest,
        Err(err) => {
            eprintln!("{}: {err}", path.display());
            return ExitCode::FAILURE;
        }
    };

    if args.command == "check" {
        println!(
            "{}: ok: {} ({}), up to {} players, base {}",
            path.display(),
            manifest.name,
            manifest.role,
            manifest.players.max,
            manifest.base
        );
        return ExitCode::SUCCESS;
    }

    let (Some(pumpkin), Some(plugins)) = (&args.pumpkin, &args.plugins) else {
        return usage("apply needs --pumpkin and --plugins");
    };
    match apply(&manifest, dir, pumpkin, plugins) {
        Ok(exports) => {
            print!("{exports}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("hallow-manifest apply: {err}");
            ExitCode::FAILURE
        }
    }
}

fn usage(err: &str) -> ExitCode {
    eprintln!("hallow-manifest: {err}\n{USAGE}");
    ExitCode::from(2)
}

/// Applies the manifest and returns the `export` lines for the environment.
fn apply(
    manifest: &Manifest,
    dir: &Path,
    pumpkin: &Path,
    plugins: &Path,
) -> Result<String, String> {
    let text = std::fs::read_to_string(pumpkin)
        .map_err(|err| format!("can't read {}: {err}", pumpkin.display()))?;
    let base: toml::Table =
        toml::from_str(&text).map_err(|err| format!("{}: {err}", pumpkin.display()))?;
    let rendered = toml::to_string(&manifest.pumpkin_config(base))
        .map_err(|err| format!("can't write pumpkin.toml: {err}"))?;
    std::fs::write(pumpkin, rendered)
        .map_err(|err| format!("can't write {}: {err}", pumpkin.display()))?;

    std::fs::create_dir_all(plugins)
        .map_err(|err| format!("can't create {}: {err}", plugins.display()))?;
    for plugin in &manifest.plugins {
        let from = dir.join(&plugin.file);
        let to = plugins.join(from.file_name().expect("a .wasm file name"));
        // A repo that keeps its plugins in plugins/ already has them in place.
        if same_file(&from, &to) {
            continue;
        }
        std::fs::copy(&from, &to)
            .map_err(|err| format!("can't copy {} to {}: {err}", from.display(), to.display()))?;
    }

    Ok(exports(&manifest.env(), |name| {
        std::env::var_os(name).is_some_and(|value| !value.is_empty())
    }))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// `export NAME='value'` for each variable the container doesn't already set.
fn exports(env: &[(String, String)], is_set: impl Fn(&str) -> bool) -> String {
    env.iter()
        .filter(|(name, _)| !is_set(name))
        .map(|(name, value)| format!("export {name}='{}'\n", value.replace('\'', r"'\''")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args, String> {
        parse_args(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_arguments() {
        assert_eq!(
            args(&["check"]).unwrap(),
            Args {
                command: "check".into(),
                ..Args::default()
            }
        );
        let parsed = args(&[
            "apply",
            "/server/hallow.toml",
            "--pumpkin",
            "p.toml",
            "--plugins",
            "plugins",
            "--base-version",
            "0.1.0",
        ])
        .unwrap();
        assert_eq!(parsed.path, Some(PathBuf::from("/server/hallow.toml")));
        assert_eq!(parsed.pumpkin, Some(PathBuf::from("p.toml")));
        assert_eq!(parsed.plugins, Some(PathBuf::from("plugins")));
        assert_eq!(parsed.base_version.as_deref(), Some("0.1.0"));
        assert!(
            args(&["check", "a", "b"])
                .unwrap_err()
                .contains("unexpected")
        );
        assert!(
            args(&["check", "--nope"])
                .unwrap_err()
                .contains("unknown option")
        );
        assert!(
            args(&["apply", "--pumpkin"])
                .unwrap_err()
                .contains("needs a value")
        );
        assert!(args(&[]).is_err());
    }

    #[test]
    fn exports_quote_and_skip_set_variables() {
        let env = vec![
            ("HALLOW_SERVER_KIND".to_owned(), "hub".to_owned()),
            ("GREETING".to_owned(), "it's here".to_owned()),
            ("HALLOW_MAX_PLAYERS".to_owned(), "100".to_owned()),
        ];
        let out = exports(&env, |name| name == "HALLOW_MAX_PLAYERS");
        assert_eq!(
            out,
            "export HALLOW_SERVER_KIND='hub'\nexport GREETING='it'\\''s here'\n"
        );
    }

    #[test]
    fn apply_writes_config_and_copies_plugins() {
        let dir =
            std::env::temp_dir().join(format!("hallow-manifest-apply-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("repo/build")).unwrap();
        std::fs::write(dir.join("repo/build/game.wasm"), b"\0asm").unwrap();
        std::fs::write(
            dir.join("pumpkin.toml"),
            "[networking.java]\naddress = \"0.0.0.0:25565\"\n",
        )
        .unwrap();
        let manifest = Manifest::parse(
            r#"
            name = "test-game"
            role = "game"
            lobby = "hub"
            base = "^0.1"
            [players]
            max = 8
            [[plugins]]
            file = "build/game.wasm"
            permissions = ["fs.read.data"]
            "#,
        )
        .unwrap();
        let out = apply(
            &manifest,
            &dir.join("repo"),
            &dir.join("pumpkin.toml"),
            &dir.join("plugins"),
        )
        .unwrap();
        assert!(out.contains("HALLOW_RETURN_LOBBY='hub'"), "{out}");
        assert!(dir.join("plugins/game.wasm").is_file());
        let config: toml::Table =
            toml::from_str(&std::fs::read_to_string(dir.join("pumpkin.toml")).unwrap()).unwrap();
        assert_eq!(
            config["networking"]["java"]["max_players"].as_integer(),
            Some(8)
        );
        assert_eq!(
            config["networking"]["java"]["address"].as_str(),
            Some("0.0.0.0:25565")
        );
        assert_eq!(
            config["plugins"]["overrides"]["game"]["allowed_permissions"][0].as_str(),
            Some("fs.read.data")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
