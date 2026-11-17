//! The map format: one TOML file per map, describing sky islands as shapes
//! plus the places a game needs (spawns, eggs, shops, generators, chests).
//!
//! A map is drawn for **one team**: its island, spawn and features go in
//! `[team]` and in entries marked `repeat = true`, and the map's symmetry
//! copies them for every other team. `copies` (1, 2 or 4) turns copies
//! about the centre by 360°/copies; `mirror` adds a mirror image of each
//! across the x axis, for up to 8 teams. Everything else (the middle, its
//! chests and generators) is placed once.
//!
//! Coordinates are world blocks. `[x, y, z]` for a block, `[x, y, z]` as
//! floats for where a player stands. The centre of the map is `x = 0,
//! z = 0`.
//!
//! [`Map::parse`] reads and checks a file; [`Map::layout`] expands the
//! symmetry into a [`Layout`] with every shape and feature in world
//! coordinates, which the generator builds and the games play on.

use std::collections::BTreeSet;

use serde::Deserialize;

/// A block position.
pub type Pos = [i32; 3];
/// Where a player stands.
pub type Spot = [f64; 3];

/// A map as written in its file.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Map {
    /// Shown to players: "Emberfall".
    pub name: String,
    /// One line about it.
    #[serde(default)]
    pub description: String,
    /// The modes the map is made for (`solo`, `teams`); empty means any.
    #[serde(default)]
    pub modes: Vec<String>,
    /// Feeds the island noise, so every build of the map is the same.
    #[serde(default)]
    pub seed: u32,
    /// Players below this height have fallen into the void.
    pub void_y: i32,
    /// Half the world border's width, in blocks, around the centre.
    pub border: f64,
    /// Turned copies of the team island: 1, 2 or 4.
    pub copies: u8,
    /// Also a mirror image of every copy, across the x axis.
    #[serde(default)]
    pub mirror: bool,
    /// Players per team. SkyWars solo maps say 1.
    #[serde(default = "one")]
    pub team_size: u8,
    /// Where players wait before the match (a glass box is built round it),
    /// for games that don't wait in cages.
    #[serde(default)]
    pub waiting: Option<Spot>,
    /// Where spectators start.
    pub spectate: Spot,
    /// The first team's island.
    pub team: TeamTemplate,
    #[serde(default, rename = "shape")]
    pub shapes: Vec<Shape>,
    #[serde(default, rename = "generator")]
    pub generators: Vec<Generator>,
    #[serde(default, rename = "chest")]
    pub chests: Vec<Chest>,
}

const fn one() -> u8 {
    1
}

/// What each team gets, drawn for the first team.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TeamTemplate {
    /// Where the team's players spawn (and respawn).
    pub spawn: Spot,
    /// More spawn points for a team of several (each gets its own cage);
    /// players past the list share `spawn`.
    #[serde(default)]
    pub extra_spawns: Vec<Spot>,
    /// SkyWars: build a glass cage round each spawn point.
    #[serde(default)]
    pub cage: bool,
    /// EggWars: the egg's block.
    #[serde(default)]
    pub egg: Option<Pos>,
    /// EggWars: where the shop keeper stands.
    #[serde(default)]
    pub shop: Option<Spot>,
    #[serde(default, rename = "generator")]
    pub generators: Vec<Generator>,
    #[serde(default, rename = "chest")]
    pub chests: Vec<Chest>,
}

/// A resource generator.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Generator {
    pub tier: Tier,
    /// The block it sits on; its resource appears just above.
    pub at: Pos,
    /// The level it starts at; 0 is off until upgraded.
    #[serde(default = "level_one")]
    pub level: u8,
    /// Copied for every team (outside `[team]`).
    #[serde(default)]
    pub repeat: bool,
}

const fn level_one() -> u8 {
    1
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Iron,
    Gold,
    Diamond,
}

/// A loot chest.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Chest {
    pub at: Pos,
    /// Which loot table fills it.
    #[serde(default)]
    pub loot: Loot,
    #[serde(default)]
    pub repeat: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Loot {
    /// A team island's chests.
    #[default]
    Island,
    /// The middle's, better.
    Middle,
}

/// Something built out of blocks.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Shape {
    /// A floating island: a flat top at `at.y` with a rough edge, soil
    /// under it, and a rocky underside tapering to a point `depth` below.
    Island {
        at: Pos,
        radius: f64,
        depth: i32,
        #[serde(default = "grass")]
        top: String,
        #[serde(default = "dirt")]
        soil: String,
        #[serde(default = "stone")]
        core: String,
        /// Sprinkled through the underside, one block in `ore_chance`.
        #[serde(default)]
        ore: Option<String>,
        #[serde(default)]
        repeat: bool,
    },
    /// A filled (or `hollow`) box between two corners, inclusive.
    Box {
        from: Pos,
        to: Pos,
        block: String,
        #[serde(default)]
        hollow: bool,
        #[serde(default)]
        repeat: bool,
    },
    /// An upright cylinder from `at.y` up `height` blocks.
    Cylinder {
        at: Pos,
        radius: f64,
        height: i32,
        block: String,
        #[serde(default)]
        repeat: bool,
    },
    /// A small tree standing on `at`.
    Tree {
        at: Pos,
        #[serde(default = "oak_log")]
        log: String,
        #[serde(default = "oak_leaves")]
        leaves: String,
        #[serde(default = "five")]
        height: i32,
        #[serde(default)]
        repeat: bool,
    },
}

fn grass() -> String {
    "grass_block".into()
}
fn dirt() -> String {
    "dirt".into()
}
fn stone() -> String {
    "stone".into()
}
fn oak_log() -> String {
    "oak_log".into()
}
fn oak_leaves() -> String {
    "oak_leaves".into()
}
const fn five() -> i32 {
    5
}

impl Shape {
    const fn repeat(&self) -> bool {
        match self {
            Self::Island { repeat, .. }
            | Self::Box { repeat, .. }
            | Self::Cylinder { repeat, .. }
            | Self::Tree { repeat, .. } => *repeat,
        }
    }

    /// The block names it uses.
    pub fn blocks(&self) -> Vec<&str> {
        match self {
            Self::Island {
                top,
                soil,
                core,
                ore,
                ..
            } => {
                let mut blocks = vec![top.as_str(), soil.as_str(), core.as_str()];
                blocks.extend(ore.as_deref());
                blocks
            }
            Self::Box { block, .. } | Self::Cylinder { block, .. } => vec![block.as_str()],
            Self::Tree { log, leaves, .. } => vec![log.as_str(), leaves.as_str()],
        }
    }

    /// Inclusive bounds: `(min, max)`.
    #[must_use]
    pub fn bounds(&self) -> (Pos, Pos) {
        match self {
            Self::Island {
                at, radius, depth, ..
            } => {
                let r = radius.ceil() as i32 + 1;
                (
                    [at[0] - r, at[1] - depth - 1, at[2] - r],
                    [at[0] + r, at[1], at[2] + r],
                )
            }
            Self::Box { from, to, .. } => (
                [from[0].min(to[0]), from[1].min(to[1]), from[2].min(to[2])],
                [from[0].max(to[0]), from[1].max(to[1]), from[2].max(to[2])],
            ),
            Self::Cylinder {
                at, radius, height, ..
            } => {
                let r = radius.ceil() as i32;
                (
                    [at[0] - r, at[1], at[2] - r],
                    [at[0] + r, at[1] + height - 1, at[2] + r],
                )
            }
            Self::Tree { at, height, .. } => (
                [at[0] - 2, at[1] + 1, at[2] - 2],
                [at[0] + 2, at[1] + height + 1, at[2] + 2],
            ),
        }
    }

    fn transformed(&self, t: Transform) -> Self {
        let mut shape = self.clone();
        match &mut shape {
            Self::Island { at, .. } | Self::Cylinder { at, .. } | Self::Tree { at, .. } => {
                *at = t.pos(*at);
            }
            Self::Box { from, to, .. } => {
                *from = t.pos(*from);
                *to = t.pos(*to);
            }
        }
        shape
    }
}

/// One copy of the team island: a turn of `quarter` quarter turns, then
/// a mirror across the x axis (x → −x) if `mirror`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transform {
    pub quarters: u8,
    pub mirror: bool,
}

impl Transform {
    pub const IDENTITY: Self = Self {
        quarters: 0,
        mirror: false,
    };

    /// A block: block `(x, z)` covers `[x, x + 1)`, so a quarter turn about
    /// the centre takes it to `(-z - 1, x)`.
    #[must_use]
    pub fn pos(self, [x, y, z]: Pos) -> Pos {
        let (mut x, mut z) = (x, z);
        for _ in 0..self.quarters % 4 {
            (x, z) = (-z - 1, x);
        }
        if self.mirror {
            x = -x - 1;
        }
        [x, y, z]
    }

    /// A point.
    #[must_use]
    pub fn spot(self, [x, y, z]: Spot) -> Spot {
        let (mut x, mut z) = (x, z);
        for _ in 0..self.quarters % 4 {
            (x, z) = (-z, x);
        }
        if self.mirror {
            x = -x;
        }
        [x, y, z]
    }
}

/// A team's copy of `[team]`, in world coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct TeamSite {
    /// Spawn points, the first is the team's main one. Players face the
    /// centre: `yaw` in Minecraft's degrees.
    pub spawns: Vec<Spot>,
    pub yaw: f32,
    pub cage: bool,
    pub egg: Option<Pos>,
    pub shop: Option<Spot>,
    pub generators: Vec<Generator>,
    pub chests: Vec<Chest>,
}

/// A map with its symmetry expanded: every shape and feature where it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    pub name: String,
    pub seed: u32,
    pub void_y: i32,
    pub border: f64,
    pub team_size: u8,
    pub waiting: Option<Spot>,
    pub spectate: Spot,
    pub teams: Vec<TeamSite>,
    pub shapes: Vec<Shape>,
    /// Shared generators (the middle's, and `repeat` ones).
    pub generators: Vec<Generator>,
    /// Shared chests.
    pub chests: Vec<Chest>,
}

impl Map {
    /// Reads a map file and checks it.
    ///
    /// # Errors
    /// The TOML is invalid, or the map doesn't hold together (see
    /// [`Map::check`]).
    pub fn parse(text: &str) -> Result<Self, String> {
        let map: Self = toml::from_str(text).map_err(|err| err.to_string())?;
        map.check()?;
        Ok(map)
    }

    /// How many teams the map has.
    #[must_use]
    pub fn team_count(&self) -> usize {
        usize::from(self.copies) * if self.mirror { 2 } else { 1 }
    }

    /// The copies, in team order.
    #[must_use]
    pub fn transforms(&self) -> Vec<Transform> {
        let step = 4 / self.copies.max(1);
        let mirrors: &[bool] = if self.mirror {
            &[false, true]
        } else {
            &[false]
        };
        mirrors
            .iter()
            .flat_map(|&mirror| {
                (0..self.copies).map(move |i| Transform {
                    quarters: i * step,
                    mirror,
                })
            })
            .collect()
    }

    /// Checks what serde can't: symmetry, sizes, and that teams don't sit
    /// on top of each other.
    ///
    /// # Errors
    /// The first problem found.
    pub fn check(&self) -> Result<(), String> {
        if !matches!(self.copies, 1 | 2 | 4) {
            return Err(format!("copies must be 1, 2 or 4, not {}", self.copies));
        }
        if self.team_size == 0 {
            return Err("team_size must be at least 1".into());
        }
        if self.border < 16.0 {
            return Err("border must be at least 16".into());
        }
        if self.team_count() < 2 {
            return Err("a map needs at least 2 teams (copies × mirror)".into());
        }
        let layout = self.layout();
        let spawns: Vec<Spot> = layout.teams.iter().map(|team| team.spawns[0]).collect();
        for (i, a) in spawns.iter().enumerate() {
            for b in &spawns[i + 1..] {
                let d = ((a[0] - b[0]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
                if d < 8.0 {
                    return Err(format!(
                        "two teams spawn {d:.1} blocks apart, at {a:?} and {b:?}; move [team] off the axis the copies share"
                    ));
                }
            }
        }
        for team in &layout.teams {
            for spot in &team.spawns {
                if spot[1] <= f64::from(self.void_y) {
                    return Err(format!("a spawn at {spot:?} is below void_y"));
                }
            }
        }
        let mut chests = BTreeSet::new();
        for chest in layout.all_chests() {
            if !chests.insert(chest.at) {
                return Err(format!("two chests at {:?}", chest.at));
            }
        }
        for shape in &self.shapes {
            if let Shape::Island { radius, depth, .. } = shape
                && (*radius < 1.0 || *depth < 1)
            {
                return Err("an island needs a radius and depth of at least 1".into());
            }
        }
        Ok(())
    }

    /// Expands the symmetry.
    #[must_use]
    pub fn layout(&self) -> Layout {
        let transforms = self.transforms();
        let teams = transforms
            .iter()
            .map(|&t| {
                let spawns: Vec<Spot> = std::iter::once(self.team.spawn)
                    .chain(self.team.extra_spawns.iter().copied())
                    .map(|spot| t.spot(spot))
                    .collect();
                TeamSite {
                    yaw: facing_centre(spawns[0]),
                    spawns,
                    cage: self.team.cage,
                    egg: self.team.egg.map(|egg| t.pos(egg)),
                    shop: self.team.shop.map(|shop| t.spot(shop)),
                    generators: self
                        .team
                        .generators
                        .iter()
                        .map(|g| Generator { at: t.pos(g.at), ..*g })
                        .collect(),
                    chests: self
                        .team
                        .chests
                        .iter()
                        .map(|c| Chest { at: t.pos(c.at), ..*c })
                        .collect(),
                }
            })
            .collect();
        let expand = |repeat: bool| -> Vec<Transform> {
            if repeat {
                transforms.clone()
            } else {
                vec![Transform::IDENTITY]
            }
        };
        let shapes = self
            .shapes
            .iter()
            .flat_map(|shape| {
                expand(shape.repeat())
                    .into_iter()
                    .map(move |t| shape.transformed(t))
            })
            .collect();
        let generators = self
            .generators
            .iter()
            .flat_map(|g| {
                expand(g.repeat)
                    .into_iter()
                    .map(move |t| Generator { at: t.pos(g.at), ..*g })
            })
            .collect();
        let chests = self
            .chests
            .iter()
            .flat_map(|c| {
                expand(c.repeat)
                    .into_iter()
                    .map(move |t| Chest { at: t.pos(c.at), ..*c })
            })
            .collect();
        Layout {
            name: self.name.clone(),
            seed: self.seed,
            void_y: self.void_y,
            border: self.border,
            team_size: self.team_size,
            waiting: self.waiting,
            spectate: self.spectate,
            teams,
            shapes,
            generators,
            chests,
        }
    }
}

/// The yaw that looks from `spot` towards the centre. Minecraft's yaw is 0
/// facing +z and 90 facing −x.
#[must_use]
pub fn facing_centre([x, _, z]: Spot) -> f32 {
    x.atan2(-z).to_degrees() as f32
}

impl Layout {
    /// Every chest: the teams' and the shared ones.
    pub fn all_chests(&self) -> impl Iterator<Item = &Chest> {
        self.teams
            .iter()
            .flat_map(|team| team.chests.iter())
            .chain(self.chests.iter())
    }

    /// Every generator: the teams' and the shared ones.
    pub fn all_generators(&self) -> impl Iterator<Item = &Generator> {
        self.teams
            .iter()
            .flat_map(|team| team.generators.iter())
            .chain(self.generators.iter())
    }

    /// Every block name the map's shapes use, plus the ones the builder
    /// adds (cages, the waiting box, generator pads).
    #[must_use]
    pub fn block_names(&self) -> BTreeSet<String> {
        let mut names: BTreeSet<String> = self
            .shapes
            .iter()
            .flat_map(Shape::blocks)
            .map(str::to_owned)
            .collect();
        for name in crate::build::BUILDER_BLOCKS {
            names.insert((*name).to_owned());
        }
        names
    }

    /// The team whose island is nearest `spot`.
    #[must_use]
    pub fn nearest_team(&self, spot: Spot) -> Option<usize> {
        self.teams
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let da = (a.spawns[0][0] - spot[0]).powi(2) + (a.spawns[0][2] - spot[2]).powi(2);
                let db = (b.spawns[0][0] - spot[0]).powi(2) + (b.spawns[0][2] - spot[2]).powi(2);
                da.total_cmp(&db)
            })
            .map(|(i, _)| i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL: &str = r#"
        name = "Test"
        void_y = 20
        border = 60
        copies = 4
        spectate = [0.5, 80, 0.5]

        [team]
        spawn = [3.5, 65, -30.5]
        egg = [3, 65, -36]
        [[team.generator]]
        tier = "iron"
        at = [5, 64, -33]

        [[shape]]
        kind = "island"
        at = [0, 64, 0]
        radius = 8
        depth = 6

        [[shape]]
        kind = "island"
        at = [3, 64, -32]
        radius = 6
        depth = 5
        repeat = true

        [[generator]]
        tier = "diamond"
        at = [0, 64, 0]
    "#;

    #[test]
    fn quarter_turns_come_back() {
        let t = Transform {
            quarters: 1,
            mirror: false,
        };
        let p = [5, 64, -33];
        let mut q = p;
        for _ in 0..4 {
            q = t.pos(q);
        }
        assert_eq!(q, p);
        assert_eq!(t.pos([0, 0, 0]), [-1, 0, 0]);
        assert_eq!(t.spot([1.0, 0.0, 0.0]), [0.0, 0.0, 1.0]);
        let m = Transform {
            quarters: 0,
            mirror: true,
        };
        assert_eq!(m.pos([2, 0, 3]), [-3, 0, 3]);
    }

    #[test]
    fn blocks_and_spots_turn_together() {
        // A spot in the middle of a block stays in the middle of its block.
        for quarters in 0..4 {
            for mirror in [false, true] {
                let t = Transform { quarters, mirror };
                let block = t.pos([7, 64, -20]);
                let spot = t.spot([7.5, 64.0, -19.5]);
                assert_eq!(
                    [spot[0].floor() as i32, spot[2].floor() as i32],
                    [block[0], block[2]]
                );
            }
        }
    }

    #[test]
    fn symmetry_makes_every_team() {
        let map = Map::parse(SMALL).unwrap();
        assert_eq!(map.team_count(), 4);
        let layout = map.layout();
        assert_eq!(layout.teams.len(), 4);
        assert_eq!(layout.shapes.len(), 1 + 4);
        assert_eq!(layout.generators.len(), 1);
        assert_eq!(layout.all_generators().count(), 5);
        let eggs: BTreeSet<Pos> = layout.teams.iter().filter_map(|t| t.egg).collect();
        assert_eq!(eggs.len(), 4);
        assert_eq!(layout.nearest_team([3.0, 65.0, -30.0]), Some(0));
    }

    #[test]
    fn spawns_face_the_middle() {
        assert!((facing_centre([0.0, 0.0, -30.0]) - 0.0).abs() < 0.01);
        assert!((facing_centre([30.0, 0.0, 0.0]) - 90.0).abs() < 0.01);
        assert!((facing_centre([0.0, 0.0, 30.0]).abs() - 180.0).abs() < 0.01);
    }

    #[test]
    fn bad_maps_are_refused() {
        let three = SMALL.replace("copies = 4", "copies = 3");
        assert!(Map::parse(&three).unwrap_err().contains("copies"));
        let on_axis = SMALL
            .replace("copies = 4", "copies = 2\nmirror = true")
            .replace("spawn = [3.5, 65, -30.5]", "spawn = [0, 65, -30]");
        assert!(Map::parse(&on_axis).unwrap_err().contains("apart"));
        assert!(Map::parse(&SMALL.replace("void_y = 20", "void_y = 70")).is_err());
        assert!(Map::parse(&SMALL.replace("name", "nmae")).is_err());
    }
}
