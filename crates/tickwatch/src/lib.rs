//! Logs the server's tick time, TPS and player count once a second, as
//! `tickwatch: players=12 mspt=3.41 tps=20.00`, for load tests. A tick has 50 ms
//! before the server falls behind.

use pumpkin_plugin_api::{
    Context, Plugin, PluginMetadata, Server, register_plugin, scheduler::schedule_repeating_task,
};

struct TickWatch;

impl Plugin for TickWatch {
    fn new() -> Self {
        Self
    }

    fn metadata(&self) -> PluginMetadata {
        PluginMetadata {
            name: "tickwatch".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Xena Studios".into()],
            description: "Logs tick time, TPS and player count once a second.".into(),
            dependencies: vec![],
            permissions: vec![],
        }
    }

    fn on_load(&self, _context: Context) -> Result<(), String> {
        schedule_repeating_task(20, 20, |server: Server| {
            tracing::info!(
                "tickwatch: players={} mspt={:.2} tps={:.2}",
                server.get_player_count(),
                server.get_mspt(),
                server.get_tps()
            );
        });
        Ok(())
    }
}

register_plugin!(TickWatch);
