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
        let next_seq = next_runtime_seq_from_state(&workspace);
        Self {
            workspace,
            config,
            config_error,
            sessions: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(next_seq),
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
    let event_id = state.next_event_id();
    let data = sanitize_event_data(agent_dir, event_type, &event_id, data)?;
    let event = serde_json::json!({
        "id": event_id,
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
    writeln!(file, "{text}").map_err(|err| err.to_string())?;
    append_hook_index(agent_dir, event_type, &event)
}

const TOOL_OUTPUT_INLINE_LIMIT: usize = 4096;

fn sanitize_event_data(
    agent_dir: &Path,
    event_type: &str,
    event_id: &str,
    mut data: serde_json::Value,
) -> Result<serde_json::Value, String> {
    if event_type != "hook.PostToolUse" {
        return Ok(data);
    }
    truncate_tool_response_field(agent_dir, event_id, &mut data, "stdout")?;
    truncate_tool_response_field(agent_dir, event_id, &mut data, "stderr")?;
    Ok(data)
}

fn truncate_tool_response_field(
    agent_dir: &Path,
    event_id: &str,
    data: &mut serde_json::Value,
    field: &str,
) -> Result<(), String> {
    let Some(tool_response) = data
        .get_mut("raw")
        .and_then(|raw| raw.get_mut("tool_response"))
        .and_then(|value| value.as_object_mut())
    else {
        return Ok(());
    };
    let Some(original) = tool_response
        .get(field)
        .and_then(|value| value.as_str())
        .map(str::to_string)
    else {
        return Ok(());
    };
    let original_bytes = original.as_bytes().len();
    if original_bytes <= TOOL_OUTPUT_INLINE_LIMIT {
        return Ok(());
    }
    let artifact_dir = agent_dir
        .join("logs")
        .join("artifacts")
        .join("PostToolUse");
    fs::create_dir_all(&artifact_dir).map_err(|err| err.to_string())?;
    let artifact_path = artifact_dir.join(format!("{event_id}-{field}.txt"));
    fs::write(&artifact_path, &original).map_err(|err| err.to_string())?;
    let truncated = format!(
        "{}\n...[AgentCall truncated {} bytes; full output: {}]",
        safe_prefix(&original, TOOL_OUTPUT_INLINE_LIMIT),
        original_bytes,
        artifact_path.display()
    );
    tool_response.insert(field.to_string(), serde_json::json!(truncated));
    tool_response.insert(
        format!("{field}_artifact"),
        serde_json::json!({
            "path": artifact_path,
            "original_bytes": original_bytes,
            "hash": fnv1a_hex(original.as_bytes()),
            "truncated": true
        }),
    );
    Ok(())
}

fn safe_prefix(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn fnv1a_hex(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn append_hook_index(
    agent_dir: &Path,
    event_type: &str,
    event: &serde_json::Value,
) -> Result<(), String> {
    let Some(hook_name) = event_type.strip_prefix("hook.") else {
        return Ok(());
    };
    let log_dir = agent_dir.join("logs").join("hooks");
    fs::create_dir_all(&log_dir).map_err(|err| err.to_string())?;
    let path = log_dir.join(format!("{hook_name}.ndjson"));
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| err.to_string())?;
    let text = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
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

pub(crate) fn next_runtime_seq_from_state(workspace: &Path) -> u64 {
    let mut max_seen = 0u64;
    let routes_path = workspace
        .join(".agentcall")
        .join("state")
        .join("routes.json");
    if let Ok(text) = fs::read_to_string(routes_path) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            collect_route_numbers(&value, &mut max_seen);
        }
    }
    let events_path = workspace.join(".agentcall").join("events.ndjson");
    if let Ok(text) = fs::read_to_string(events_path) {
        for line in text.lines() {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                collect_route_numbers(&value, &mut max_seen);
            }
        }
    }
    max_seen + 1
}

fn collect_route_numbers(value: &serde_json::Value, max_seen: &mut u64) {
    match value {
        serde_json::Value::String(text) => {
            if let Some(number) = route_number(text) {
                *max_seen = (*max_seen).max(number);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                collect_route_numbers(item, max_seen);
            }
        }
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                if let Some(number) = route_number(key) {
                    *max_seen = (*max_seen).max(number);
                }
                collect_route_numbers(value, max_seen);
            }
        }
        _ => {}
    }
}

fn route_number(text: &str) -> Option<u64> {
    text.strip_prefix("route-")?.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn runtime_seq_recovers_from_routes_and_events() {
        let root = test_workspace("runtime-seq");
        let state_dir = root.join(".agentcall").join("state");
        fs::create_dir_all(&state_dir).unwrap();
        fs::write(
            state_dir.join("routes.json"),
            r#"{"route-9":{"route_id":"route-9"}}"#,
        )
        .unwrap();
        fs::write(
            root.join(".agentcall").join("events.ndjson"),
            r#"{"data":{"route_id":"route-12"}}"#,
        )
        .unwrap();
        assert_eq!(next_runtime_seq_from_state(&root), 13);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn posttooluse_output_is_truncated_and_indexed_by_hook_type() {
        let root = test_workspace("posttooluse-index");
        let agent_dir = root.join(".agentcall");
        let state = AppState::test(root.clone());
        fs::create_dir_all(agent_dir.join("state")).unwrap();
        let large_stdout = "x".repeat(TOOL_OUTPUT_INLINE_LIMIT + 128);

        append_agent_event_locked(
            &state,
            &agent_dir,
            "hook.PostToolUse",
            "post tool use",
            serde_json::json!({
                "raw": {
                    "tool_response": {
                        "stdout": large_stdout,
                        "stderr": "short"
                    }
                }
            }),
        )
        .unwrap();

        let events_text = fs::read_to_string(agent_dir.join("events.ndjson")).unwrap();
        let event: serde_json::Value = serde_json::from_str(events_text.lines().next().unwrap()).unwrap();
        let stdout = event["data"]["raw"]["tool_response"]["stdout"]
            .as_str()
            .unwrap();
        assert!(stdout.contains("AgentCall truncated"));
        let artifact = event["data"]["raw"]["tool_response"]["stdout_artifact"]["path"]
            .as_str()
            .unwrap();
        assert!(Path::new(artifact).exists());

        let hook_index = agent_dir.join("logs").join("hooks").join("PostToolUse.ndjson");
        assert!(hook_index.exists());
        let _ = fs::remove_dir_all(root);
    }

    fn test_workspace(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("agentcall-state-{name}-{nonce}"))
    }
}
