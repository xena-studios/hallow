//! Arenas: the map a match is played on, loaded fresh from disk.
//!
//! A server repo keeps its maps in `maps/`, one folder per map (`hallow.toml`'s
//! `[maps] dir`). Each folder is a Pumpkin world (`region/` and so on) plus a
//! `map.toml`:
//!
//! ```toml
//! name = "Sky Temple"
//! authors = ["Xena Studios"]
//! void_y = 40.0                        # below this a player is out (default -64)
//! spectators = { x = 0.5, y = 120.0, z = 0.5 }   # default: above the first spawn
//!
//! [[spawns]]
//! team = "red"                         # a team id; leave it out for any team
//! x = 10.5
//! y = 100.0
//! z = 0.5
//! yaw = 90.0
//! ```
//!
//! The base image copies `maps/` into the game plugin's data folder on every
//! start (`plugins/data/<plugin>/maps`), the only place a plugin can read
//! files. When the match server starts, the SDK picks a map
//! (`hallow.toml`'s `maps.pick`), copies its folder to `arenas/<map>` and
//! loads that copy as a world, so the map on disk is never changed and every
//! load starts from it. Chunks the map doesn't have are left empty, so a
//! map is an island in the void unless the game sets its own generator
//! ([`Settings::generator`](crate::Settings::generator)).
//!
//! A game with no maps plays in the server's own world, at its spawn.

use std::path::Path;

use pumpkin_plugin_api::{Dimension, Server, World, worldgen::ChunkGenerator};
use serde::Deserialize;

/// Where the game plugin's data folder is inside its sandbox.
const DATA: &str = "data";
/// The lowest block of the overworld.
const WORLD_BOTTOM: f64 = -64.0;

/// A point to put a player at.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    #[serde(default)]
    pub yaw: f32,
    #[serde(default)]
    pub pitch: f32,
}

impl Point {
    #[must_use]
    pub const fn position(&self) -> (f64, f64, f64) {
        (self.x, self.y, self.z)
    }
}

/// A spawn point, for one team or for anyone.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spawn {
    #[serde(default)]
    pub team: Option<String>,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    #[serde(default)]
    pub yaw: f32,
    #[serde(default)]
    pub pitch: f32,
}

impl Spawn {
    /// Where the spawn puts a player.
    #[must_use]
    pub const fn at(&self) -> Point {
        Point {
            x: self.x,
            y: self.y,
            z: self.z,
            yaw: self.yaw,
            pitch: self.pitch,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MapFile {
    name: String,
    #[serde(default)]
    authors: Vec<String>,
    void_y: Option<f64>,
    spectators: Option<Point>,
    #[serde(default)]
    spawns: Vec<Spawn>,
}

/// One map, from its folder's `map.toml`.
#[derive(Clone, Debug, PartialEq)]
pub struct Map {
    /// The folder's name.
    pub id: String,
    pub name: String,
    pub authors: Vec<String>,
    /// Below this height a player is out.
    pub void_y: f64,
    /// Where spectators go.
    pub spectators: Point,
    pub spawns: Vec<Spawn>,
}

impl Map {
    /// Reads a map from the text of its `map.toml`; `id` is its folder.
    pub fn parse(id: &str, text: &str) -> Result<Self, String> {
        let file: MapFile = toml::from_str(text).map_err(|err| format!("{id}/map.toml: {err}"))?;
        if file.spawns.is_empty() {
            return Err(format!("{id}/map.toml: no [[spawns]]"));
        }
        let first = file.spawns[0].at();
        Ok(Self {
            id: id.to_owned(),
            name: file.name,
            authors: file.authors,
            void_y: file.void_y.unwrap_or(WORLD_BOTTOM),
            spectators: file.spectators.unwrap_or(Point {
                y: first.y + 10.0,
                ..first
            }),
            spawns: file.spawns,
        })
    }

    /// The `index`th spawn for `team`: its own spawns, else the spawns for
    /// anyone, else any spawn. Spawns are reused round the list.
    #[must_use]
    pub fn spawn_for(&self, team: Option<&str>, index: usize) -> Point {
        let own: Vec<&Spawn> = self
            .spawns
            .iter()
            .filter(|spawn| team.is_some() && spawn.team.as_deref() == team)
            .collect();
        let open: Vec<&Spawn> = self.spawns.iter().filter(|s| s.team.is_none()).collect();
        let list = if !own.is_empty() {
            own
        } else if !open.is_empty() {
            open
        } else {
            self.spawns.iter().collect()
        };
        list[index % list.len()].at()
    }
}

/// How a map is picked (`hallow.toml`'s `maps.pick`, `HALLOW_MAPS_PICK`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pick {
    #[default]
    Random,
    /// Players vote in the lobby. Not built yet: picks at random.
    Vote,
}

impl Pick {
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text.trim() {
            "vote" => Self::Vote,
            _ => Self::Random,
        }
    }
}

/// Every map in `maps/` of the data folder, sorted by id. A folder whose
/// `map.toml` is missing or wrong is logged and skipped.
#[must_use]
pub fn maps() -> Vec<Map> {
    read_maps(&Path::new(DATA).join("maps"))
}

fn read_maps(dir: &Path) -> Vec<Map> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut maps: Vec<Map> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let id = entry.file_name().to_string_lossy().into_owned();
            let text = match std::fs::read_to_string(entry.path().join("map.toml")) {
                Ok(text) => text,
                Err(err) => {
                    tracing::warn!("hallow-sdk: skipping map {id}: no map.toml ({err})");
                    return None;
                }
            };
            Map::parse(&id, &text)
                .map_err(|err| tracing::warn!("hallow-sdk: skipping map {err}"))
                .ok()
        })
        .collect();
    maps.sort_by(|a, b| a.id.cmp(&b.id));
    maps
}

/// Picks one of `maps`; `seed` varies the random pick.
#[must_use]
pub fn pick(maps: &[Map], how: Pick, seed: u64) -> Option<&Map> {
    match how {
        Pick::Random | Pick::Vote => {
            let len = u64::try_from(maps.len()).ok().filter(|len| *len > 0)?;
            maps.get(usize::try_from(seed % len).ok()?)
        }
    }
}

/// A chunk generator that leaves everything empty: the void around a map.
pub struct Void;

impl ChunkGenerator for Void {}

/// The map a match is played on, loaded as its own world.
pub struct Arena {
    pub map: Map,
    /// The world's name in Pumpkin: the map's id.
    pub world_name: String,
}

impl Arena {
    /// Copies `map` from `maps/` to `arenas/` in the data folder of the
    /// plugin `plugin` and loads the copy as a world that generates with
    /// `generator` where the map has no chunks.
    pub fn open(server: &Server, plugin: &str, map: &Map, generator: u32) -> Result<Self, String> {
        let data = Path::new(DATA);
        let from = data.join("maps").join(&map.id);
        let to = data.join("arenas").join(&map.id);
        if to.exists() {
            std::fs::remove_dir_all(&to)
                .map_err(|err| format!("can't clear {}: {err}", to.display()))?;
        }
        copy_world(&from, &to)?;
        // Pumpkin loads a world from its world folder joined with the name,
        // and the plugin's data folder sits next to that folder
        // (hallow.toml's `world` is one folder deep).
        let world = server.create_world(
            &format!("../plugins/data/{plugin}/arenas/{}", map.id),
            Dimension::Overworld,
        );
        world.set_chunk_generator(generator);
        Ok(Self {
            map: map.clone(),
            world_name: world.get_name(),
        })
    }

    /// The arena's world. Pumpkin hands out a new handle each time.
    #[must_use]
    pub fn world(&self, server: &Server) -> Option<World> {
        server.get_world_by_name(&self.world_name)
    }
}

/// Copies a map's world folder, leaving out its `map.toml`.
fn copy_world(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|err| format!("can't create {}: {err}", to.display()))?;
    let entries =
        std::fs::read_dir(from).map_err(|err| format!("can't read {}: {err}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("can't read {}: {err}", from.display()))?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            copy_world(&source, &target)?;
        } else if entry.file_name() != "map.toml" {
            std::fs::copy(&source, &target).map_err(|err| {
                format!(
                    "can't copy {} to {}: {err}",
                    source.display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLE: &str = r#"
        name = "Temple"
        authors = ["Xena Studios"]
        void_y = 40.0

        [[spawns]]
        team = "red"
        x = 10.5
        y = 100.0
        z = 0.5
        yaw = 90.0

        [[spawns]]
        team = "blue"
        x = -9.5
        y = 100.0
        z = 0.5

        [[spawns]]
        x = 0.5
        y = 101.0
        z = 0.5
    "#;

    #[test]
    fn parses_map_toml() {
        let map = Map::parse("temple", TEMPLE).unwrap();
        assert_eq!(map.name, "Temple");
        assert_eq!(map.void_y, 40.0);
        assert_eq!(map.spawns.len(), 3);
        assert_eq!(map.spawns[0].team.as_deref(), Some("red"));
        assert_eq!(map.spawns[0].yaw, 90.0);
        // Spectators default to above the first spawn.
        assert_eq!(map.spectators.position(), (10.5, 110.0, 0.5));

        assert!(Map::parse("empty", "name = \"Empty\"").is_err());
        assert!(Map::parse("typo", &format!("{TEMPLE}\nvoid = 3")).is_err());
        let plain =
            Map::parse("plain", "name = \"P\"\n[[spawns]]\nx = 0\ny = 64\nz = 0\n").unwrap();
        assert_eq!(plain.void_y, WORLD_BOTTOM);
    }

    #[test]
    fn spawns_by_team() {
        let map = Map::parse("temple", TEMPLE).unwrap();
        assert_eq!(map.spawn_for(Some("red"), 0).x, 10.5);
        assert_eq!(map.spawn_for(Some("red"), 3).x, 10.5);
        assert_eq!(map.spawn_for(Some("blue"), 0).x, -9.5);
        // No spawns of its own: the open ones.
        assert_eq!(map.spawn_for(Some("green"), 0).x, 0.5);
        assert_eq!(map.spawn_for(None, 1).x, 0.5);
    }

    #[test]
    fn reads_and_picks_maps() {
        let dir = std::env::temp_dir().join(format!("hallow-sdk-maps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for id in ["b", "a"] {
            std::fs::create_dir_all(dir.join(id).join("region")).unwrap();
            std::fs::write(dir.join(id).join("map.toml"), TEMPLE).unwrap();
            std::fs::write(dir.join(id).join("region/r.0.0.mca"), b"x").unwrap();
        }
        std::fs::create_dir_all(dir.join("broken")).unwrap();
        let maps = read_maps(&dir);
        assert_eq!(
            maps.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(pick(&maps, Pick::Random, 3).unwrap().id, "b");
        assert!(pick(&[], Pick::Random, 3).is_none());
        assert_eq!(Pick::parse("vote"), Pick::Vote);

        let copy = dir.join("copy");
        copy_world(&dir.join("a"), &copy).unwrap();
        assert!(copy.join("region/r.0.0.mca").is_file());
        assert!(!copy.join("map.toml").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
