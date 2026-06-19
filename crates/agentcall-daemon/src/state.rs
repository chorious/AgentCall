use crate::actor::ActorHandle;
use crate::config::LocalConfig;
use crate::control::ControlTokenClaims;
use crate::events::{EventEnvelopeV1, build_event_envelope, event_session_key};
use crate::ownership::{OwnerLease, WorkspaceLease};
use crate::projection::{SessionProjectionV1, apply_event_to_projection, read_session_projection};
use crate::session::Session;
use crate::store::{CommandStatus, RuntimeStore, StoreWriterRuntimeStore};
use crate::store_json::JsonRuntimeStore;
use crate::store_sqlite::SqliteRuntimeStore;
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const ROTATING_LOG_MAX_BYTES: u64 = 1024 * 1024;
const READ_TAIL_BYTES: u64 = 2 * 1024 * 1024;
const RECENT_EVENT_LIMIT: usize = 80;
const TOOL_OUTPUT_INLINE_LIMIT: usize = 4096;

pub(crate) struct AppState {
    pub(crate) workspace: PathBuf,
    pub(crate) config: LocalConfig,
    pub(crate) config_error: Option<String>,
    pub(crate) process_started_at_ms: u64,
    pub(crate) sessions: Mutex<HashMap<String, Arc<Session>>>,
    pub(crate) actors: Mutex<HashMap<String, ActorHandle>>,
    pub(crate) owner_leases: Mutex<HashMap<String, OwnerLease>>,
    pub(crate) workspace_leases: Mutex<HashMap<String, WorkspaceLease>>,
    pub(crate) control_tokens: Mutex<HashMap<String, ControlTokenClaims>>,
    pub(crate) store: Arc<dyn RuntimeStore>,
    pub(crate) seq: AtomicU64,
    pub(crate) event_seq: AtomicU64,
    pub(crate) event_session_seq: Mutex<HashMap<String, u64>>,
    pub(crate) projections: Mutex<HashMap<String, SessionProjectionV1>>,
    pub(crate) state_writer: Mutex<()>,
    pub(crate) last_runtime_cleanup_ms: AtomicU64,
}

impl AppState {
    pub(crate) fn new(
        workspace: PathBuf,
        config: LocalConfig,
        config_error: Option<String>,
    ) -> Self {
        let log_next_event_seq = next_event_number_from_log(&workspace);
        let log_event_session_seq = next_session_event_numbers_from_log(&workspace);
        let next_seq = next_runtime_seq_from_state(&workspace);
        let store = configured_runtime_store(&workspace, &config);
        let next_event_seq = store
            .next_event_global_seq(log_next_event_seq)
            .unwrap_or_else(|err| {
                eprintln!(
                    "agentcall-daemon: failed to recover event seq from {} store: {err}",
                    store.backend_name()
                );
                log_next_event_seq
            });
        let event_session_seq = store
            .next_session_event_numbers(log_event_session_seq.clone())
            .unwrap_or_else(|err| {
                eprintln!(
                    "agentcall-daemon: failed to recover session event seq from {} store: {err}",
                    store.backend_name()
                );
                log_event_session_seq
            });
        Self {
            workspace,
            config,
            config_error,
            process_started_at_ms: crate::util::now_ms(),
            sessions: Mutex::new(HashMap::new()),
            actors: Mutex::new(HashMap::new()),
            owner_leases: Mutex::new(HashMap::new()),
            workspace_leases: Mutex::new(HashMap::new()),
            control_tokens: Mutex::new(HashMap::new()),
            store,
            seq: AtomicU64::new(next_seq),
            event_seq: AtomicU64::new(next_event_seq),
            event_session_seq: Mutex::new(event_session_seq),
            projections: Mutex::new(HashMap::new()),
            state_writer: Mutex::new(()),
            last_runtime_cleanup_ms: AtomicU64::new(0),
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

    pub(crate) fn next_event_global_seq(&self) -> u64 {
        self.event_seq.fetch_add(1, Ordering::SeqCst)
    }

    pub(crate) fn next_event_session_seq(&self, session_key: &str) -> u64 {
        let mut session_seq = self.event_session_seq.lock().unwrap();
        let next = session_seq.get(session_key).copied().unwrap_or(1);
        session_seq.insert(session_key.to_string(), next + 1);
        next
    }
}

fn configured_runtime_store(workspace: &Path, config: &LocalConfig) -> Arc<dyn RuntimeStore> {
    let requested_writer_threads = config
        .store_writer_threads
        .or(config.max_sessions)
        .unwrap_or(6)
        .clamp(1, 6);
    match config.store_backend.as_deref() {
        Some("sqlite") => {
            let inner: Arc<dyn RuntimeStore> = Arc::new(
                SqliteRuntimeStore::new(workspace.to_path_buf())
                    .expect("failed to initialize sqlite runtime store"),
            );
            Arc::new(StoreWriterRuntimeStore::new(
                inner,
                requested_writer_threads,
            ))
        }
        _ => {
            let inner: Arc<dyn RuntimeStore> =
                Arc::new(JsonRuntimeStore::new(workspace.to_path_buf()));
            Arc::new(StoreWriterRuntimeStore::new(
                inner,
                requested_writer_threads,
            ))
        }
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
    let event_path = recent_events_path_for(path).unwrap_or_else(|| path.to_path_buf());
    let Some(text) = read_tail_text(&event_path, READ_TAIL_BYTES) else {
        return vec![];
    };
    let mut events: Vec<serde_json::Value> = text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    if events.len() > RECENT_EVENT_LIMIT {
        events = events.split_off(events.len() - RECENT_EVENT_LIMIT);
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

pub(crate) fn complete_command_event(
    state: &AppState,
    command_id: &str,
    status: CommandStatus,
    event_type: &str,
    message: &str,
    data: serde_json::Value,
) -> Result<(), String> {
    let _guard = state.state_writer.lock().unwrap();
    let envelope = build_agent_event_envelope(state, event_type, message, data)?;
    let previous_projection = envelope
        .session_id
        .as_deref()
        .and_then(|session_id| read_session_projection(state, session_id));
    let projection_update = apply_event_to_projection(previous_projection, &envelope);
    let projection_changed = projection_update.changed;
    let projection = projection_update.projection.clone();
    state
        .store
        .complete_command_with_event(command_id, status, &envelope, projection_update)?;
    if projection_changed {
        state
            .projections
            .lock()
            .unwrap()
            .insert(projection.session_id.clone(), projection);
    }
    append_hook_index(
        &state.workspace.join(".agentcall"),
        event_type,
        &envelope.to_compat_json(),
    )
}

pub(crate) fn append_agent_event_locked(
    state: &AppState,
    agent_dir: &Path,
    event_type: &str,
    message: &str,
    data: serde_json::Value,
) -> Result<(), String> {
    fs::create_dir_all(&agent_dir).map_err(|err| err.to_string())?;
    let envelope =
        build_agent_event_envelope_with_dir(state, agent_dir, event_type, message, data)?;
    let event = envelope.to_compat_json();
    let previous_projection = envelope
        .session_id
        .as_deref()
        .and_then(|session_id| read_session_projection(state, session_id));
    let projection_update = apply_event_to_projection(previous_projection, &envelope);
    let _projection_reason = projection_update.reason.as_str();
    let projection_changed = projection_update.changed;
    let projection = projection_update.projection.clone();
    state
        .store
        .append_event_and_update_projection(&envelope, projection_update)?;
    if projection_changed {
        state
            .projections
            .lock()
            .unwrap()
            .insert(projection.session_id.clone(), projection);
    }
    append_hook_index(agent_dir, event_type, &event)
}

fn build_agent_event_envelope(
    state: &AppState,
    event_type: &str,
    message: &str,
    data: serde_json::Value,
) -> Result<EventEnvelopeV1, String> {
    build_agent_event_envelope_with_dir(
        state,
        &state.workspace.join(".agentcall"),
        event_type,
        message,
        data,
    )
}

fn build_agent_event_envelope_with_dir(
    state: &AppState,
    agent_dir: &Path,
    event_type: &str,
    message: &str,
    data: serde_json::Value,
) -> Result<EventEnvelopeV1, String> {
    fs::create_dir_all(agent_dir).map_err(|err| err.to_string())?;
    let global_seq = state.next_event_global_seq();
    let event_id = format!("evt-{global_seq:06}");
    let session_key = event_session_key(&data);
    let session_seq = session_key
        .as_deref()
        .map(|key| state.next_event_session_seq(key));
    let data = sanitize_event_data(agent_dir, event_type, &event_id, data)?;
    Ok(build_event_envelope(
        event_id,
        global_seq,
        session_seq,
        event_type,
        message,
        data,
    ))
}

fn sanitize_event_data(
    agent_dir: &Path,
    event_type: &str,
    event_id: &str,
    mut data: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let Some(hook_name) = event_type.strip_prefix("hook.") else {
        return Ok(data);
    };
    match event_type {
        "hook.PostToolUse" => sanitize_post_tool_use(agent_dir, hook_name, event_id, &mut data)?,
        "hook.PostToolBatch" => {
            sanitize_post_tool_batch(agent_dir, hook_name, event_id, &mut data)?
        }
        _ => {}
    }
    Ok(data)
}

fn sanitize_post_tool_use(
    agent_dir: &Path,
    hook_name: &str,
    event_id: &str,
    data: &mut serde_json::Value,
) -> Result<(), String> {
    let Some(raw) = data.get_mut("raw").and_then(|raw| raw.as_object_mut()) else {
        return Ok(());
    };
    sanitize_tool_response(agent_dir, hook_name, event_id, "tool-response", raw)
}

fn sanitize_post_tool_batch(
    agent_dir: &Path,
    hook_name: &str,
    event_id: &str,
    data: &mut serde_json::Value,
) -> Result<(), String> {
    let Some(tool_calls) = data
        .get_mut("raw")
        .and_then(|raw| raw.get_mut("tool_calls"))
        .and_then(|value| value.as_array_mut())
    else {
        return Ok(());
    };
    for (index, call) in tool_calls.iter_mut().enumerate() {
        if let Some(call_object) = call.as_object_mut() {
            let label = format!("batch-{index}");
            sanitize_tool_response(agent_dir, hook_name, event_id, &label, call_object)?;
        }
    }
    Ok(())
}

fn sanitize_tool_response(
    agent_dir: &Path,
    hook_name: &str,
    event_id: &str,
    label: &str,
    object: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let Some(response) = object.get_mut("tool_response") else {
        return Ok(());
    };
    if response.as_str().is_some() {
        let original = response.as_str().unwrap().to_string();
        if original.as_bytes().len() > TOOL_OUTPUT_INLINE_LIMIT {
            let artifact = write_text_artifact(agent_dir, hook_name, event_id, label, &original)?;
            *response = serde_json::json!(artifact_marker(&artifact));
            object.insert("tool_response_artifact".to_string(), artifact);
        }
        return Ok(());
    }
    let Some(response_object) = response.as_object_mut() else {
        return Ok(());
    };
    truncate_object_string_field(
        agent_dir,
        hook_name,
        event_id,
        label,
        response_object,
        "stdout",
    )?;
    truncate_object_string_field(
        agent_dir,
        hook_name,
        event_id,
        label,
        response_object,
        "stderr",
    )?;
    truncate_nested_file_content(agent_dir, hook_name, event_id, label, response_object)?;
    Ok(())
}

fn truncate_nested_file_content(
    agent_dir: &Path,
    hook_name: &str,
    event_id: &str,
    label: &str,
    object: &mut serde_json::Map<String, serde_json::Value>,
) -> Result<(), String> {
    let Some(file) = object
        .get_mut("file")
        .and_then(|value| value.as_object_mut())
    else {
        return Ok(());
    };
    truncate_object_string_field(agent_dir, hook_name, event_id, label, file, "content")
}

fn truncate_object_string_field(
    agent_dir: &Path,
    hook_name: &str,
    event_id: &str,
    label: &str,
    object: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<(), String> {
    let Some(original) = object.get(field).and_then(|value| value.as_str()) else {
        return Ok(());
    };
    let original = original.to_string();
    let original_bytes = original.as_bytes().len();
    if original_bytes <= TOOL_OUTPUT_INLINE_LIMIT {
        return Ok(());
    }
    let artifact = write_text_artifact(
        agent_dir,
        hook_name,
        event_id,
        &format!("{label}-{field}"),
        &original,
    )?;
    let truncated = format!(
        "{}\n...[AgentCall truncated {} bytes; full output: {}]",
        safe_prefix(&original, TOOL_OUTPUT_INLINE_LIMIT),
        original_bytes,
        artifact
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    );
    object.insert(field.to_string(), serde_json::json!(truncated));
    object.insert(format!("{field}_artifact"), artifact);
    Ok(())
}

fn write_text_artifact(
    agent_dir: &Path,
    hook_name: &str,
    event_id: &str,
    label: &str,
    text: &str,
) -> Result<serde_json::Value, String> {
    let artifact_dir = agent_dir
        .join("artifacts")
        .join("hooks")
        .join(safe_path_component(hook_name));
    fs::create_dir_all(&artifact_dir).map_err(|err| err.to_string())?;
    let artifact_path = artifact_dir.join(format!(
        "{}-{}.txt",
        safe_path_component(event_id),
        safe_path_component(label)
    ));
    fs::write(&artifact_path, text).map_err(|err| err.to_string())?;
    Ok(serde_json::json!({
        "path": artifact_path,
        "original_bytes": text.as_bytes().len(),
        "line_count": text.lines().count(),
        "hash": fnv1a_hex(text.as_bytes()),
        "truncated": true
    }))
}

fn artifact_marker(artifact: &serde_json::Value) -> String {
    format!(
        "[AgentCall artifact: {} bytes, {} lines, path={}]",
        artifact
            .get("original_bytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        artifact
            .get("line_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        artifact
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    )
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
    let log_dir = agent_dir.join("logs").join("hooks").join(hook_name);
    let text = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    append_rotating_ndjson(&log_dir, hook_name, &text, ROTATING_LOG_MAX_BYTES).map(|_| ())
}

fn append_rotating_ndjson(
    dir: &Path,
    stem: &str,
    line: &str,
    max_bytes: u64,
) -> Result<PathBuf, String> {
    fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    let path = dir.join("recent.ndjson");
    let line_bytes = line.as_bytes().len() as u64 + 1;
    if let Ok(metadata) = fs::metadata(&path) {
        if metadata.len() > 0 && metadata.len().saturating_add(line_bytes) > max_bytes {
            let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
            let archive_dir = dir.join("archive").join(date);
            fs::create_dir_all(&archive_dir).map_err(|err| err.to_string())?;
            let stamp = chrono::Utc::now().format("%H%M%S%.3f").to_string();
            let archive_path =
                archive_dir.join(format!("{}-{}.ndjson", safe_path_component(stem), stamp));
            fs::rename(&path, archive_path).map_err(|err| err.to_string())?;
        }
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| err.to_string())?;
    writeln!(file, "{line}").map_err(|err| err.to_string())?;
    Ok(path)
}

fn safe_path_component(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn recent_events_path_for(path: &Path) -> Option<PathBuf> {
    if path.file_name().and_then(|name| name.to_str()) != Some("events.ndjson") {
        return None;
    }
    let agent_dir = path.parent()?;
    let recent = agent_dir.join("events").join("recent.ndjson");
    if recent.exists() {
        Some(recent)
    } else if path.exists() {
        Some(path.to_path_buf())
    } else {
        None
    }
}

fn read_tail_text(path: &Path, max_bytes: u64) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    if start > 0 {
        if let Some(index) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=index);
        }
    }
    Some(String::from_utf8_lossy(&bytes).to_string())
}

pub(crate) fn next_event_number_from_log(workspace: &Path) -> u64 {
    let mut max_seen = 0u64;
    for path in event_log_candidates(workspace) {
        if let Some(text) = read_tail_text(&path, READ_TAIL_BYTES) {
            for line in text.lines() {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if let Some(number) = value.get("global_seq").and_then(serde_json::Value::as_u64) {
                    max_seen = max_seen.max(number);
                    continue;
                }
                if let Some(number) = value
                    .get("id")
                    .and_then(|value| value.as_str())
                    .and_then(|id| id.strip_prefix("evt-"))
                    .and_then(|value| value.parse::<u64>().ok())
                {
                    max_seen = max_seen.max(number);
                }
            }
        }
    }
    max_seen + 1
}

pub(crate) fn next_session_event_numbers_from_log(workspace: &Path) -> HashMap<String, u64> {
    let mut max_seen: HashMap<String, u64> = HashMap::new();
    for path in event_log_candidates(workspace) {
        let Some(text) = read_tail_text(&path, READ_TAIL_BYTES) else {
            continue;
        };
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let Some(session_key) = value
                .get("session_key")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
            else {
                continue;
            };
            let Some(session_seq) = value.get("session_seq").and_then(serde_json::Value::as_u64)
            else {
                continue;
            };
            let entry = max_seen.entry(session_key.to_string()).or_insert(0);
            *entry = (*entry).max(session_seq);
        }
    }
    max_seen
        .into_iter()
        .map(|(session_key, max_seq)| (session_key, max_seq + 1))
        .collect()
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
    for events_path in event_log_candidates(workspace) {
        let Some(text) = read_tail_text(&events_path, READ_TAIL_BYTES) else {
            continue;
        };
        for line in text.lines() {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                collect_route_numbers(&value, &mut max_seen);
            }
        }
    }
    max_seen + 1
}

fn event_log_candidates(workspace: &Path) -> Vec<PathBuf> {
    let agent_dir = workspace.join(".agentcall");
    [
        agent_dir.join("events").join("recent.ndjson"),
        agent_dir.join("events.ndjson"),
    ]
    .into_iter()
    .filter(|path| path.exists())
    .collect()
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

        let events_text =
            fs::read_to_string(agent_dir.join("events").join("recent.ndjson")).unwrap();
        let event: serde_json::Value =
            serde_json::from_str(events_text.lines().next().unwrap()).unwrap();
        let stdout = event["data"]["raw"]["tool_response"]["stdout"]
            .as_str()
            .unwrap();
        assert!(stdout.contains("AgentCall truncated"));
        let artifact = event["data"]["raw"]["tool_response"]["stdout_artifact"]["path"]
            .as_str()
            .unwrap();
        assert!(Path::new(artifact).exists());

        let hook_index = agent_dir
            .join("logs")
            .join("hooks")
            .join("PostToolUse")
            .join("recent.ndjson");
        assert!(hook_index.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn event_envelope_v1_tracks_global_and_session_sequence() {
        let root = test_workspace("event-envelope-v1");
        let agent_dir = root.join(".agentcall");
        let state = AppState::test(root.clone());

        append_agent_event_locked(
            &state,
            &agent_dir,
            "session.test",
            "first",
            serde_json::json!({"wrapper_session": "worker-a"}),
        )
        .unwrap();
        append_agent_event_locked(
            &state,
            &agent_dir,
            "session.test",
            "second",
            serde_json::json!({"wrapper_session": "worker-a"}),
        )
        .unwrap();
        append_agent_event_locked(
            &state,
            &agent_dir,
            "session.test",
            "third",
            serde_json::json!({"wrapper_session": "worker-b"}),
        )
        .unwrap();

        let events_text =
            fs::read_to_string(agent_dir.join("events").join("recent.ndjson")).unwrap();
        let events: Vec<serde_json::Value> = events_text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(events[0]["schema_version"], 1);
        assert_eq!(events[0]["event_id"], "evt-000001");
        assert_eq!(events[0]["global_seq"], 1);
        assert_eq!(events[1]["global_seq"], 2);
        assert_eq!(events[2]["global_seq"], 3);
        assert_eq!(events[0]["session_key"], "worker-a");
        assert_eq!(events[0]["session_seq"], 1);
        assert_eq!(events[1]["session_seq"], 2);
        assert_eq!(events[2]["session_key"], "worker-b");
        assert_eq!(events[2]["session_seq"], 1);
        assert_eq!(next_event_number_from_log(&root), 4);
        assert_eq!(
            next_session_event_numbers_from_log(&root)
                .get("worker-a")
                .copied(),
            Some(3)
        );
        let projection_path = agent_dir
            .join("state")
            .join("projections")
            .join("sessions")
            .join("worker-a.json");
        let projection: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(projection_path).unwrap()).unwrap();
        assert_eq!(projection["session_id"], "worker-a");
        assert_eq!(projection["projection_last_global_seq"], 2);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_config_can_select_sqlite_store_backend() {
        let root = test_workspace("sqlite-backend-config");
        let state = AppState::new(
            root.clone(),
            LocalConfig {
                claude_workspace: Some(root.clone()),
                store_backend: Some("sqlite".to_string()),
                ..LocalConfig::default()
            },
            None,
        );
        assert_eq!(state.store.backend_name(), "sqlite");
        assert!(
            root.join(".agentcall")
                .join("state")
                .join("runtime.db")
                .exists()
        );
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
