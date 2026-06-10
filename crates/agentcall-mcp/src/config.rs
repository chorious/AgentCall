use std::env;
use std::path::PathBuf;

#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) workspace: PathBuf,
    pub(crate) daemon_url: String,
    pub(crate) daemon_token: Option<String>,
}

impl Config {
    pub(crate) fn from_args(args: Vec<String>) -> Result<Self, String> {
        let mut workspace = env::current_dir().map_err(|err| err.to_string())?;
        let mut daemon_url = "http://127.0.0.1:3293".to_string();
        let mut daemon_token = env::var("AGENTCALL_DAEMON_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty());
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
                "--daemon-token" => {
                    index += 1;
                    daemon_token = Some(
                        args.get(index)
                            .ok_or("missing --daemon-token value")?
                            .to_string(),
                    );
                }
                "--help" | "-h" => {
                    return Err(
                        "usage: agentcall-mcp [--workspace PATH] [--python PYTHON] [--daemon-url URL] [--daemon-token TOKEN]".to_string(),
                    );
                }
                other => return Err(format!("unknown argument: {other}")),
            }
            index += 1;
        }
        if daemon_token.is_none() {
            daemon_token = read_local_daemon_token(&workspace);
        }
        Ok(Self {
            workspace,
            daemon_url,
            daemon_token,
        })
    }
}

fn read_local_daemon_token(workspace: &std::path::Path) -> Option<String> {
    let path = workspace.join("config").join("agentcall.local.json");
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("daemon_token")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
}
