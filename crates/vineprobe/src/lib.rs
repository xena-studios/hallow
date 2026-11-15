//! A Vine plugin for tests. It adds commands that drive Vine's runtime server
//! registry and drain mode, so `netcheck` can exercise them from in game:
//!
//! - `/probe-register <name> <address> <secret>`: register a server with Vine forwarding
//! - `/probe-unregister <name>`
//! - `/probe-drain on|off`
//! - `/probe-http <host:port> <path>`: plain-HTTP GET through `wasi:http`, to check the
//!   `http.outbound` permission
//!
//! Not for production: anyone who can run a command can reroute players.

// The bindings generated from Vine's WIT include calls with many arguments.
#![allow(clippy::too_many_arguments)]

wit_bindgen::generate!({
    path: "../../upstream/Vine/wit",
    world: "plugin",
});

use vine::plugin::{logging, proxy, types::ForwardingMode};

const REGISTER: u32 = 1;
const UNREGISTER: u32 = 2;
const DRAIN: u32 = 3;
const HTTP: u32 = 4;

struct Probe;

fn reply(sender: &CommandSender, message: &str) {
    match &sender.player_uuid {
        Some(uuid) => {
            proxy::send_message(uuid, message);
        }
        None => logging::log(logging::Level::Info, message),
    }
}

impl Guest for Probe {
    fn init_plugin() {}

    fn on_load(ctx: Context) -> Result<(), String> {
        ctx.register_command(
            REGISTER,
            "probe-register",
            "Register a backend server",
            &[],
            None,
        );
        ctx.register_command(
            UNREGISTER,
            "probe-unregister",
            "Unregister a backend server",
            &[],
            None,
        );
        ctx.register_command(
            DRAIN,
            "probe-drain",
            "Stop or resume accepting logins",
            &[],
            None,
        );
        ctx.register_command(HTTP, "probe-http", "GET a URL through wasi:http", &[], None);
        logging::log(logging::Level::Info, "vineprobe: loaded");
        Ok(())
    }

    fn on_unload(_ctx: Context) -> Result<(), String> {
        Ok(())
    }

    fn get_metadata() -> PluginMetadata {
        PluginMetadata {
            name: "vineprobe".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            authors: vec!["Xena Studios".into()],
            description: Some("Test commands for Vine's server registry and drain mode.".into()),
            dependencies: vec![],
        }
    }

    fn handle_event(_handler_id: u32, evt: Event) -> Event {
        evt
    }

    fn handle_command(
        command_id: u32,
        sender: CommandSender,
        args: Vec<String>,
    ) -> Result<i32, String> {
        match (command_id, args.as_slice()) {
            (REGISTER, [name, address, secret]) => {
                let ok = proxy::register_server(name, address, ForwardingMode::Vine, Some(secret));
                reply(&sender, &format!("vineprobe: register {name} at {address}: {ok}"));
                Ok(i32::from(ok))
            }
            (UNREGISTER, [name]) => {
                let ok = proxy::unregister_server(name);
                reply(&sender, &format!("vineprobe: unregister {name}: {ok}"));
                Ok(i32::from(ok))
            }
            (DRAIN, [state]) if state == "on" || state == "off" => {
                proxy::set_draining(state == "on");
                reply(&sender, &format!("vineprobe: draining {}", proxy::is_draining()));
                Ok(1)
            }
            (HTTP, [authority, path]) => {
                let outcome = match http_get(authority, path) {
                    Ok(status) => format!("status {status}"),
                    Err(err) => format!("error {err}"),
                };
                reply(&sender, &format!("vineprobe: http {authority}{path}: {outcome}"));
                Ok(1)
            }
            _ => Err("usage: /probe-register <name> <address> <secret> | /probe-unregister <name> | /probe-drain on|off | /probe-http <host:port> <path>".into()),
        }
    }

    fn handle_command_suggestion(
        _command_id: u32,
        _sender: CommandSender,
        _args: Vec<String>,
    ) -> Vec<String> {
        Vec::new()
    }

    fn handle_task(_handler_id: u32) {}
}

/// A plain-HTTP GET, returning the status code or the `wasi:http` error.
fn http_get(authority: &str, path: &str) -> Result<u16, String> {
    use wasip2::http::{outgoing_handler, types};

    let request = types::OutgoingRequest::new(types::Fields::new());
    let _ = request.set_scheme(Some(&types::Scheme::Http));
    let _ = request.set_authority(Some(authority));
    let _ = request.set_path_with_query(Some(path));
    let body = request.body().map_err(|()| "no body".to_string())?;
    types::OutgoingBody::finish(body, None).map_err(|err| format!("{err:?}"))?;

    let future = outgoing_handler::handle(request, None).map_err(|err| format!("{err:?}"))?;
    future.subscribe().block();
    let response = future
        .get()
        .ok_or("no response")?
        .map_err(|()| "already taken".to_string())?
        .map_err(|err| format!("{err:?}"))?;
    Ok(response.status())
}

export!(Probe);
