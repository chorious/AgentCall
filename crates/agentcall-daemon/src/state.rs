use crate::config::LocalConfig;
use crate::session::Session;
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) struct AppState {
    pub(crate) workspace: PathBuf,
    pub(crate) config: LocalConfig,
    pub(crate) config_error: Option<String>,
    pub(crate) sessions: Mutex<HashMap<String, Arc<Session>>>,
    pub(crate) seq: AtomicU64,
    pub(crate) event_seq: AtomicU64,
    pub(crate) state_writer: Mutex<()>,
}

impl AppState {
    pub(crate) fn new(workspace: PathBuf, config: LocalConfig, config_error: Option<String>) -> Self {
        let next_event_seq = next_event_number_from_log(&workspace);
        Self {
            workspace,
            config,
            config_error,
            sessions: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(1),
            event_seq: AtomicU64::new(next_event_seq),
            state_writer: Mutex::new(()),
        }
    }

    #[cfg(test)]
    pub(crate) fn test(workspace: PathBuf) -> Self {
        Self::new(
            workspace.clone(),
            LocalConfig {
                claude_workspace: Some(workspace),
                ..LocalConfig::default()
            },
            None,
        )
    }

    pub(crate) fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn next_event_id(&self) -> String {
        let number = self.event_seq.fetch_add(1, Ordering::SeqCst);
        format!("evt-{number:06}")
    }
}

pub(crate) fn read_json_file(path: &Path, default: serde_json::Value) -> serde_json::Value {
    let Ok(text) = fs::read_to_string(path) else {
        return default;
    };
    serde_json::from_str(&text).unwrap_or(default)
}

pub(crate) fn write_json_file(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        std::process::id()
    ));
    let text = serde_json::to_string_pretty(value).map_err(|err| err.to_string())? + "\n";
    fs::write(&tmp, text).map_err(|err| err.to_string())?;
    fs::rename(&tmp, path).map_err(|err| err.to_string())
}

pub(crate) fn read_events(path: &Path) -> Vec<serde_json::Value> {
    let Ok(text) = fs::read_to_string(path) else {
        return vec![];
    };
    let mut events: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    if events.len() > 80 {
        events = events.split_off(events.len() - 80);
    }
    events
}

pub(crate) fn append_agent_event(
    state: &AppState,
    event_type: &str,
    message: &str,
    data: serde_json::Value,
) {
    let _guard = state.state_writer.lock().unwrap();
    if let Err(err) = append_agent_event_locked(
        state,
        &state.workspace.join(".agentcall"),
        event_type,
        message,
        data,
    ) {
        eprintln!("agentcall-daemon: failed to append event: {err}");
    }
}

pub(crate) fn append_agent_event_locked(
    state: &AppState,
    agent_dir: &Path,
    event_type: &str,
    message: &str,
    data: serde_json::Value,
) -> Result<(), String> {
    fs::create_dir_all(&agent_dir).map_err(|err| err.to_string())?;
    let path = agent_dir.join("events.ndjson");
    let event = serde_json::json!({
        "id": state.next_event_id(),
        "ts": chrono::Utc::now().to_rfc3339(),
        "type": event_type,
        "task_id": null,
        "run_id": null,
        "message": message,
        "data": data,
    });
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| err.to_string())?;
    let text = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
    writeln!(file, "{text}").map_err(|err| err.to_string())
}

pub(crate) fn next_event_number_from_log(workspace: &Path) -> u64 {
    let path = workspace.join(".agentcall").join("events.ndjson");
    let Ok(text) = fs::read_to_string(path) else {
        return 1;
    };
    let mut max_seen = 0u64;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(id) = value.get("id").and_then(|value| value.as_str()) else {
            continue;
        };
        let Some(number) = id
            .strip_prefix("evt-")
            .and_then(|value| value.parse::<u64>().ok())
        else {
            continue;
        };
        max_seen = max_seen.max(number);
    }
    max_seen + 1
}
