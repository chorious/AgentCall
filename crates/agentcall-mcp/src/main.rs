mod bootstrap;
mod config;
mod daemon_client;
mod protocol;
mod tools;

use crate::config::Config;
use crate::protocol::serve;
use std::env;

fn main() {
    let config = match Config::from_args(env::args().skip(1).collect()) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(2);
        }
    };
    if let Err(err) = serve(config) {
        eprintln!("agentcall-mcp: {err}");
        std::process::exit(1);
    }
}
