use std::env;
use std::path::PathBuf;

#[derive(Clone)]
pub(crate) struct Config {
    pub(crate) workspace: PathBuf,
    pub(crate) daemon_url: String,
    pub(crate) daemon_token: Option<String>,
    pub(crate) owner_id: String,
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
        let owner_id = derive_owner_id();
        Ok(Self {
            workspace,
            daemon_url,
            daemon_token,
            owner_id,
        })
    }
}

fn derive_owner_id() -> String {
    derive_owner_id_from(
        env::var("AGENTCALL_OWNER_ID").ok(),
        env::var("CODEX_THREAD_ID").ok(),
        std::process::id(),
    )
}

fn derive_owner_id_from(
    explicit_owner: Option<String>,
    codex_thread_id: Option<String>,
    process_id: u32,
) -> String {
    explicit_owner
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            codex_thread_id
                .filter(|value| !value.trim().is_empty())
                .map(|value| format!("codex-thread-{value}"))
        })
        .or_else(|| Some(format!("codex-mcp-pid-{process_id}")))
        .map(|value| normalize_owner_id(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("codex-mcp-pid-{process_id}"))
}

fn normalize_owner_id(value: &str) -> String {
    let mut normalized = String::new();
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':') {
            normalized.push(ch);
        } else {
            normalized.push('-');
        }
        if normalized.len() >= 96 {
            break;
        }
    }
    normalized.trim_matches('-').to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_id_prefers_explicit_env() {
        assert_eq!(
            derive_owner_id_from(
                Some("codex-main".to_string()),
                Some("thread-a".to_string()),
                42
            ),
            "codex-main"
        );
    }

    #[test]
    fn owner_id_uses_thread_when_available() {
        assert_eq!(
            derive_owner_id_from(None, Some("019e:abc".to_string()), 42),
            "codex-thread-019e:abc"
        );
    }

    #[test]
    fn owner_id_falls_back_to_mcp_process_not_global_codex() {
        assert_eq!(derive_owner_id_from(None, None, 42), "codex-mcp-pid-42");
    }
}
