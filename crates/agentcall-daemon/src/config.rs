use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct LocalConfig {
    pub(crate) claude_workspace: Option<PathBuf>,
    pub(crate) acp_command: Option<Vec<String>>,
    pub(crate) acp_default_timeout_seconds: Option<u64>,
    pub(crate) acp_max_timeout_seconds: Option<u64>,
    pub(crate) acp_checkpoint_due_seconds: Option<u64>,
    pub(crate) acp_heartbeat_interval_seconds: Option<u64>,
    pub(crate) acp_max_active_invocations: Option<usize>,
}

impl LocalConfig {
    pub(crate) fn load(workspace: &Path) -> Result<Self, String> {
        let path = config_path(workspace);
        let text = std::fs::read_to_string(&path).map_err(|err| {
            format!(
                "missing local daemon config: {} ({err}). Copy config/agentcall.example.json to config/agentcall.local.json and set claude_workspace.",
                path.display()
            )
        })?;
        serde_json::from_str(&text)
            .map_err(|err| format!("invalid local daemon config: {} ({err})", path.display()))
    }
}

pub(crate) fn config_path(workspace: &Path) -> PathBuf {
    workspace.join("config").join("agentcall.local.json")
}
