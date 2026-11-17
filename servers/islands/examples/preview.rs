//! Prints a map from above, one character per block, to check a map file
//! by eye: `cargo run -p hallow-islands --example preview -- <map.toml>`.
//! `#` ground, `T` tree, `I` `G` `D` generators, `E` egg, `S` shop,
//! `@` spawn, `C` chest, `o` cage.

use std::collections::BTreeMap;

use hallow_islands::build::{Builder, Palette};
use hallow_islands::map::Map;

fn main() {
    let path = std::env::args().nth(1).expect("a map file");
    let map = Map::parse(&std::fs::read_to_string(&path).expect("readable")).expect("valid");
    let layout = map.layout();
    let names: Vec<String> = layout.block_names().into_iter().collect();
    let palette: Palette = names
        .iter()
        .enumerate()
        .map(|(i, n)| (n.clone(), u16::try_from(i + 1).unwrap()))
        .collect();
    let builder = Builder::new(&layout, palette).expect("buildable");
    let r = layout.border as i32;
    let mut top: BTreeMap<(i32, i32), (i32, char)> = BTreeMap::new();
    let reach = r / 16 + 1;
    for cx in -reach..=reach {
        for cz in -reach..=reach {
            builder.build_chunk(cx, cz, -64, 384, &mut |x, y, z, state| {
                let name = &names[usize::from(state) - 1];
                if name == "air" {
                    return;
                }
                let c = match name.as_str() {
                    n if n.contains("leaves") || n.contains("log") => 'T',
                    "iron_block" => 'I',
                    "gold_block" => 'G',
                    "diamond_block" => 'D',
                    "dragon_egg" => 'E',
                    "glass" => 'o',
                    _ => '#',
                };
                let key = (cx * 16 + i32::from(x), cz * 16 + i32::from(z));
                if top.get(&key).is_none_or(|(ty, _)| y >= *ty) {
                    top.insert(key, (y, c));
                }
            });
        }
    }
    for team in &layout.teams {
        for spawn in &team.spawns {
            top.insert(
                (spawn[0].floor() as i32, spawn[2].floor() as i32),
                (999, '@'),
            );
        }
        if let Some(shop) = team.shop {
            top.insert((shop[0].floor() as i32, shop[2].floor() as i32), (999, 'S'));
        }
    }
    for chest in layout.all_chests() {
        top.insert((chest.at[0], chest.at[2]), (999, 'C'));
    }
    println!(
        "{} ({} teams of {})",
        layout.name,
        layout.teams.len(),
        layout.team_size
    );
    for z in -r..=r {
        let line: String = (-r..=r)
            .map(|x| top.get(&(x, z)).map_or(' ', |(_, c)| *c))
            .collect();
        println!("{}", line.trim_end());
    }
}
