mod acp;
mod acp_supervisor;
mod config;
mod hooks;
mod http;
mod mcp;
mod routes;
mod session;
mod state;
mod summary;
mod terminal;
mod util;

use crate::config::LocalConfig;
use crate::http::handle_connection;
use crate::state::{AppState, append_agent_event};
use crate::util::normalize_path;
use std::env;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut port = 3293u16;
    let mut workspace = env::current_dir()?;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                port = args.next().ok_or("missing --port value")?.parse::<u16>()?;
            }
            "--workspace" => {
                workspace = PathBuf::from(args.next().ok_or("missing --workspace value")?);
            }
            _ => return Err(format!("unknown argument: {arg}").into()),
        }
    }

    let workspace = normalize_path(workspace)?;
    let (config, config_error) = match LocalConfig::load(&workspace) {
        Ok(config) => (config, None),
        Err(err) => (LocalConfig::default(), Some(err)),
    };
    let state = Arc::new(AppState::new(workspace, config, config_error));
    acp_supervisor::mark_orphaned_on_start(&state);
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    println!("AgentCall daemon: http://localhost:{port}");
    append_agent_event(
        &state,
        "daemon.started",
        "AgentCall daemon started.",
        serde_json::json!({
            "port": port,
            "workspace": state.workspace,
            "config_error": state.config_error,
            "claude_workspace": state.config.claude_workspace
        }),
    );
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let state = Arc::clone(&state);
                thread::spawn(move || {
                    if let Err(err) = handle_connection(stream, state) {
                        eprintln!("request error: {err}");
                    }
                });
            }
            Err(err) => eprintln!("accept error: {err}"),
        }
    }
    Ok(())
}
