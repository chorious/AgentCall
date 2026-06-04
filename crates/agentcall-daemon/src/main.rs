mod acp;
mod hooks;
mod http;
mod mcp;
mod routes;
mod session;
mod state;
mod summary;
mod terminal;
mod util;

use crate::http::handle_connection;
use crate::session::default_claude_workspace;
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

    let state = Arc::new(AppState::new(normalize_path(workspace)?));
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    println!("AgentCall daemon: http://localhost:{port}");
    append_agent_event(
        &state,
        "daemon.started",
        "AgentCall daemon started.",
        serde_json::json!({"port": port, "workspace": state.workspace, "default_claude_workspace": default_claude_workspace()}),
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
