use std::env;
use std::path::PathBuf;

#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) workspace: PathBuf,
    pub(crate) daemon_url: String,
}

impl Config {
    pub(crate) fn from_args(args: Vec<String>) -> Result<Self, String> {
        let mut workspace = env::current_dir().map_err(|err| err.to_string())?;
        let mut daemon_url = "http://127.0.0.1:3293".to_string();
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--workspace" => {
                    index += 1;
                    workspace = PathBuf::from(
                        args.get(index)
                            .ok_or("missing --workspace value")?
                            .to_string(),
                    );
                }
                "--python" => {
                    index += 1;
                    let _ = args.get(index).ok_or("missing --python value")?;
                }
                "--daemon-url" => {
                    index += 1;
                    daemon_url = args
                        .get(index)
                        .ok_or("missing --daemon-url value")?
                        .to_string();
                }
                "--help" | "-h" => {
                    return Err(
                        "usage: agentcall-mcp [--workspace PATH] [--python PYTHON] [--daemon-url URL]".to_string(),
                    );
                }
                other => return Err(format!("unknown argument: {other}")),
            }
            index += 1;
        }
        Ok(Self {
            workspace,
            daemon_url,
        })
    }
}
