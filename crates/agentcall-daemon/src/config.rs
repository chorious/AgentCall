use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct LocalConfig {
    pub(crate) claude_workspace: Option<PathBuf>,
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
        serde_json::from_str(&text).map_err(|err| {
            format!(
                "invalid local daemon config: {} ({err})",
                path.display()
            )
        })
    }
}

pub(crate) fn config_path(workspace: &Path) -> PathBuf {
    workspace.join("config").join("agentcall.local.json")
}
