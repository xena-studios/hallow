//! Turns a [`Layout`] into blocks, one chunk at a time. Pure: block names
//! are resolved to state ids once ([`Palette`]), and the chunk generator
//! hands [`Builder::build_chunk`] a closure that writes into Pumpkin's
//! chunk buffer. The same build runs in the tests.
//!
//! Besides the map's shapes the builder adds the blocks the games rely on:
//! a pad of the tier's block under each generator, each team's egg, a
//! glass cage round every spawn of a map with `cage = true`, and a glass
//! box round the waiting spot. Chests aren't built here: they need block
//! entities, which only a placed block gets, so games place them once the
//! chunks are loaded.

use std::collections::BTreeMap;

use crate::map::{Layout, Pos, Shape, Tier};

/// Blocks the builder uses on its own.
pub const BUILDER_BLOCKS: &[&str] = &[
    "glass",
    "iron_block",
    "gold_block",
    "diamond_block",
    "dragon_egg",
];

/// Block name → state id, for every name a layout uses.
pub type Palette = BTreeMap<String, u16>;

/// A block the builder adds at one spot.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Point {
    at: Pos,
    block: String,
}

/// Builds a layout's chunks.
#[derive(Clone, Debug)]
pub struct Builder {
    seed: u32,
    /// With their bounds, to skip the ones a chunk doesn't touch.
    shapes: Vec<(Shape, Pos, Pos)>,
    points: Vec<Point>,
    palette: Palette,
}

/// The block a generator stands on.
#[must_use]
pub const fn pad(tier: Tier) -> &'static str {
    match tier {
        Tier::Iron => "iron_block",
        Tier::Gold => "gold_block",
        Tier::Diamond => "diamond_block",
    }
}

/// The glass shell round a spawn point: a 3×3 room two blocks high with a
/// floor and a roof, the player standing on the floor.
#[must_use]
pub fn cage(spawn: [f64; 3]) -> Vec<Pos> {
    let (x, y, z) = (
        spawn[0].floor() as i32,
        spawn[1].floor() as i32,
        spawn[2].floor() as i32,
    );
    let mut blocks = Vec::new();
    for dy in -1..=2 {
        for dx in -1..=1 {
            for dz in -1..=1 {
                let inside = dx == 0 && dz == 0 && (0..=1).contains(&dy);
                if !inside {
                    blocks.push([x + dx, y + dy, z + dz]);
                }
            }
        }
    }
    blocks
}

/// The waiting box: a 9×9 glass floor with walls three high, open above.
#[must_use]
pub fn waiting_box(spot: [f64; 3]) -> Vec<Pos> {
    let (x, y, z) = (
        spot[0].floor() as i32,
        spot[1].floor() as i32,
        spot[2].floor() as i32,
    );
    let mut blocks = Vec::new();
    for dx in -4..=4_i32 {
        for dz in -4..=4_i32 {
            blocks.push([x + dx, y - 1, z + dz]);
            if dx.abs() == 4 || dz.abs() == 4 {
                for dy in 0..3 {
                    blocks.push([x + dx, y + dy, z + dz]);
                }
            }
        }
    }
    blocks
}

impl Builder {
    /// # Errors
    /// The palette lacks a block the layout uses.
    pub fn new(layout: &Layout, palette: Palette) -> Result<Self, String> {
        let shapes = layout
            .shapes
            .iter()
            .map(|shape| {
                let (min, max) = shape.bounds();
                (shape.clone(), min, max)
            })
            .collect();
        let mut points = Vec::new();
        for generator in layout.all_generators() {
            points.push(Point {
                at: generator.at,
                block: pad(generator.tier).into(),
            });
        }
        for team in &layout.teams {
            if let Some(egg) = team.egg {
                points.push(Point {
                    at: egg,
                    block: "dragon_egg".into(),
                });
            }
            if team.cage {
                for spawn in &team.spawns {
                    points.extend(cage(*spawn).into_iter().map(|at| Point {
                        at,
                        block: "glass".into(),
                    }));
                }
            }
        }
        if let Some(spot) = layout.waiting {
            points.extend(waiting_box(spot).into_iter().map(|at| Point {
                at,
                block: "glass".into(),
            }));
        }
        let builder = Self {
            seed: layout.seed,
            shapes,
            points,
            palette,
        };
        for name in layout.block_names() {
            builder.id(&name)?;
        }
        Ok(builder)
    }

    fn id(&self, name: &str) -> Result<u16, String> {
        self.palette
            .get(name)
            .copied()
            .ok_or_else(|| format!("no state for block {name}"))
    }

    /// Calls `set(x, y, z, state)` for every block of chunk `(cx, cz)`, with
    /// `x` and `z` local to the chunk. Later shapes overwrite earlier ones;
    /// the builder's own blocks go last. Heights outside `min_y..min_y +
    /// height` are skipped.
    pub fn build_chunk(
        &self,
        cx: i32,
        cz: i32,
        min_y: i32,
        height: i32,
        set: &mut impl FnMut(u8, i32, u8, u16),
    ) {
        let (x0, z0) = (cx * 16, cz * 16);
        let (x1, z1) = (x0 + 15, z0 + 15);
        let max_y = min_y + height - 1;
        let mut put = |x: i32, y: i32, z: i32, state: u16| {
            if (x0..=x1).contains(&x) && (z0..=z1).contains(&z) && (min_y..=max_y).contains(&y) {
                set((x - x0) as u8, y, (z - z0) as u8, state);
            }
        };
        for (shape, min, max) in &self.shapes {
            if max[0] < x0 || min[0] > x1 || max[2] < z0 || min[2] > z1 {
                continue;
            }
            self.shape(shape, [x0, z0, x1, z1], &mut put);
        }
        for point in &self.points {
            if let Some(&state) = self.palette.get(&point.block) {
                put(point.at[0], point.at[1], point.at[2], state);
            }
        }
    }

    /// Writes the part of `shape` inside the columns `[x0, z0, x1, z1]`.
    fn shape(&self, shape: &Shape, [x0, z0, x1, z1]: [i32; 4], put: &mut impl FnMut(i32, i32, i32, u16)) {
        let (min, max) = shape.bounds();
        let xs = min[0].max(x0)..=max[0].min(x1);
        let zs = min[2].max(z0)..=max[2].min(z1);
        let id = |name: &str| self.palette.get(name).copied().unwrap_or(0);
        match shape {
            Shape::Island {
                at,
                radius,
                depth,
                top,
                soil,
                core,
                ore,
                ..
            } => {
                let (top, soil, core) = (id(top), id(soil), id(core));
                let ore = ore.as_deref().map(id);
                for x in xs {
                    for z in zs.clone() {
                        let Some(bottom) = self.island_column(*at, *radius, *depth, x, z) else {
                            continue;
                        };
                        let y = at[1];
                        put(x, y, z, top);
                        for d in 1..=bottom {
                            let state = if d <= 2 {
                                soil
                            } else {
                                match ore {
                                    Some(ore) if hash(self.seed, x, y - d, z) % 23 == 0 => ore,
                                    _ => core,
                                }
                            };
                            put(x, y - d, z, state);
                        }
                    }
                }
            }
            Shape::Box {
                from,
                to,
                block,
                hollow,
                ..
            } => {
                let state = id(block);
                let _ = (from, to);
                let (lo, hi) = (min, max);
                for x in xs {
                    for z in zs.clone() {
                        for y in lo[1]..=hi[1] {
                            let edge = x == lo[0]
                                || x == hi[0]
                                || y == lo[1]
                                || y == hi[1]
                                || z == lo[2]
                                || z == hi[2];
                            if !hollow || edge {
                                put(x, y, z, state);
                            }
                        }
                    }
                }
            }
            Shape::Cylinder {
                at,
                radius,
                height,
                block,
                ..
            } => {
                let state = id(block);
                for x in xs {
                    for z in zs.clone() {
                        if distance(*at, x, z) <= *radius {
                            for y in at[1]..at[1] + height {
                                put(x, y, z, state);
                            }
                        }
                    }
                }
            }
            Shape::Tree {
                at,
                log,
                leaves,
                height,
                ..
            } => {
                let (log, leaves) = (id(log), id(leaves));
                let crown = at[1] + height;
                for x in xs {
                    for z in zs.clone() {
                        let (dx, dz) = (x - at[0], z - at[2]);
                        for y in crown - 2..=crown + 1 {
                            let reach = if y > crown { 1 } else { 2 };
                            let corner = dx.abs() == reach && dz.abs() == reach;
                            let trunk = dx == 0 && dz == 0 && y <= crown;
                            if dx.abs() <= reach
                                && dz.abs() <= reach
                                && !trunk
                                && !(corner && hash(self.seed, x, y, z) % 2 == 0)
                            {
                                put(x, y, z, leaves);
                            }
                        }
                        if dx == 0 && dz == 0 {
                            for y in at[1] + 1..=crown {
                                put(x, y, z, log);
                            }
                        }
                    }
                }
            }
        }
    }

    /// How far below the top an island's column reaches, or `None` outside
    /// its edge. The edge wobbles with the angle, and the underside tapers
    /// to a rough point.
    fn island_column(&self, at: Pos, radius: f64, depth: i32, x: i32, z: i32) -> Option<i32> {
        let (dx, dz) = (
            f64::from(x) + 0.5 - (f64::from(at[0]) + 0.5),
            f64::from(z) + 0.5 - (f64::from(at[2]) + 0.5),
        );
        let d = dx.hypot(dz);
        let edge = radius * (0.84 + 0.16 * edge_noise(self.seed ^ hash_pos(at), dz.atan2(dx)));
        if d > edge.max(1.0) {
            return None;
        }
        let t = (d / edge.max(1.0)).clamp(0.0, 1.0);
        let jitter = 0.7 + 0.3 * f64::from(hash(self.seed, x, at[1], z) % 100) / 100.0;
        let bottom = (f64::from(depth) * (1.0 - t.powf(1.6)) * jitter).round() as i32;
        Some(bottom.max(1))
    }
}

fn distance(at: Pos, x: i32, z: i32) -> f64 {
    (f64::from(x - at[0])).hypot(f64::from(z - at[2]))
}

fn hash_pos(at: Pos) -> u32 {
    hash(0, at[0], at[1], at[2])
}

/// A small, stable integer hash.
#[must_use]
pub fn hash(seed: u32, x: i32, y: i32, z: i32) -> u32 {
    let mut h = u64::from(seed)
        ^ (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (z as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    h ^= h >> 31;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 29;
    (h & 0xFFFF_FFFF) as u32
}

/// Smooth noise round a circle, 0..1, from twelve hashed points.
fn edge_noise(seed: u32, angle: f64) -> f64 {
    const POINTS: i32 = 12;
    let at = (angle / std::f64::consts::TAU).rem_euclid(1.0) * f64::from(POINTS);
    let i = at.floor() as i32;
    let f = at - f64::from(i);
    let value = |k: i32| f64::from(hash(seed, k.rem_euclid(POINTS), 0, 0) % 1000) / 1000.0;
    let s = f * f * (3.0 - 2.0 * f);
    value(i) * (1.0 - s) + value(i + 1) * s
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::map::Map;

    fn palette(layout: &Layout) -> Palette {
        layout
            .block_names()
            .into_iter()
            .enumerate()
            .map(|(i, name)| (name, i as u16 + 1))
            .collect()
    }

    fn build(layout: &Layout) -> BTreeMap<Pos, String> {
        let palette = palette(layout);
        let names: BTreeMap<u16, String> = palette.iter().map(|(n, i)| (*i, n.clone())).collect();
        let builder = Builder::new(layout, palette).unwrap();
        let mut blocks = BTreeMap::new();
        for cx in -6..6 {
            for cz in -6..6 {
                builder.build_chunk(cx, cz, -64, 384, &mut |x, y, z, state| {
                    blocks.insert(
                        [cx * 16 + i32::from(x), y, cz * 16 + i32::from(z)],
                        names[&state].clone(),
                    );
                });
            }
        }
        blocks
    }

    const MAP: &str = r#"
        name = "Test"
        void_y = 20
        border = 60
        copies = 2
        spectate = [0.5, 80, 0.5]
        waiting = [0.5, 100, 0.5]

        [team]
        spawn = [0.5, 69, -29.5]
        cage = true
        egg = [0, 65, -34]
        [[team.generator]]
        tier = "gold"
        at = [2, 64, -30]

        [[shape]]
        kind = "island"
        at = [0, 64, -30]
        radius = 6
        depth = 5
        repeat = true

        [[shape]]
        kind = "tree"
        at = [0, 64, 0]

        [[shape]]
        kind = "box"
        from = [-1, 60, -1]
        to = [1, 64, 1]
        block = "oak_planks"
        hollow = true
    "#;

    #[test]
    fn islands_are_solid_and_mirrored() {
        let layout = Map::parse(MAP).unwrap().layout();
        let blocks = build(&layout);
        // Both islands have a top at y 64 round their centre.
        assert_eq!(blocks[&[0, 64, -30]], "grass_block");
        assert_eq!(blocks[&[-1, 64, 29]], "grass_block");
        assert_eq!(blocks[&[0, 63, -30]], "dirt");
        assert!(blocks.get(&[0, 64, -40]).is_none(), "outside the edge");
        // The builder's own blocks.
        assert_eq!(blocks[&[2, 64, -30]], "gold_block");
        assert_eq!(blocks[&[0, 65, -34]], "dragon_egg");
        assert_eq!(blocks[&[0, 68, -30]], "glass", "cage floor");
        assert!(blocks.get(&[0, 69, -30]).is_none(), "inside the cage");
        assert_eq!(blocks[&[4, 99, 4]], "glass", "waiting box");
    }

    #[test]
    fn chunks_agree_at_their_edges() {
        // Building twice gives the same blocks: the noise is stable.
        let layout = Map::parse(MAP).unwrap().layout();
        assert_eq!(build(&layout), build(&layout));
    }

    #[test]
    fn hollow_boxes_and_trees() {
        let layout = Map::parse(MAP).unwrap().layout();
        let blocks = build(&layout);
        assert_eq!(blocks[&[1, 62, 1]], "oak_planks");
        assert!(blocks.get(&[0, 62, 0]).is_none(), "hollow");
        assert_eq!(blocks[&[0, 67, 0]], "oak_log");
        assert_eq!(blocks[&[1, 68, 0]], "oak_leaves");
    }

    #[test]
    fn cages_have_room_for_one() {
        let shell = cage([0.5, 70.0, 0.5]);
        assert_eq!(shell.len(), 4 * 9 - 2);
        assert!(!shell.contains(&[0, 70, 0]));
        assert!(shell.contains(&[0, 69, 0]));
    }

    #[test]
    fn missing_blocks_are_reported() {
        let layout = Map::parse(MAP).unwrap().layout();
        let mut palette = palette(&layout);
        palette.remove("oak_planks");
        assert!(Builder::new(&layout, palette).unwrap_err().contains("oak_planks"));
    }
}
