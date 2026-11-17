//! The chunk generator: builds a [`Layout`] into an empty world as Pumpkin
//! generates its chunks, so a match's map is made fresh in every instance
//! and nothing needs resetting.

use std::sync::Arc;

use pumpkin_plugin_api::{
    ChunkBuffer, ChunkGenerator, GeneratorManager, Server, World, world::resolve_block_state,
};

use crate::build::{Builder, Palette};
use crate::map::Layout;

struct Generator(Arc<Builder>);

impl ChunkGenerator for Generator {
    fn generate_noise(&self, chunk: &mut ChunkBuffer) {
        let (cx, cz, min_y) = (chunk.x(), chunk.z(), chunk.min_y());
        let height = i32::try_from(chunk.height()).unwrap_or(384);
        self.0
            .build_chunk(cx, cz, min_y, height, &mut |x, y, z, state| {
                chunk.set_block(x, y, z, state);
            });
    }
}

/// A block name's default state, with or without the `minecraft:` prefix.
#[must_use]
pub fn block_state(name: &str) -> Option<u16> {
    let bare = name.strip_prefix("minecraft:").unwrap_or(name);
    resolve_block_state(&format!("minecraft:{bare}"), &[])
        .or_else(|| resolve_block_state(bare, &[]))
}

/// Resolves every block the layout uses.
///
/// # Errors
/// A block name Pumpkin doesn't know.
pub fn palette(layout: &Layout) -> Result<Palette, String> {
    layout
        .block_names()
        .into_iter()
        .map(|name| {
            block_state(&name)
                .map(|state| (name.clone(), state))
                .ok_or_else(|| format!("unknown block {name} in map {}", layout.name))
        })
        .collect()
}

/// Builds the layout into `world` from now on: every chunk Pumpkin
/// generates from here comes from the map, and the world border is set
/// round it. Call it from `on_load`, before any player has loaded a chunk;
/// a fresh instance has none on disk.
///
/// # Errors
/// The layout uses an unknown block.
pub fn build_into(world: &World, layout: &Layout) -> Result<(), String> {
    let builder = Builder::new(layout, palette(layout)?)?;
    let id = GeneratorManager::register(Generator(Arc::new(builder)));
    world.set_chunk_generator(id);
    let border = world.get_world_border();
    border.set_center(0.0, 0.0);
    border.set_diameter(layout.border * 2.0, None);
    Ok(())
}

/// The server's overworld, where matches are played.
#[must_use]
pub fn overworld(server: &Server) -> Option<World> {
    let worlds = server.get_all_worlds();
    let index = worlds
        .iter()
        .position(|w| w.get_dimension().ends_with("overworld"))
        .unwrap_or(0);
    worlds.into_iter().nth(index)
}
