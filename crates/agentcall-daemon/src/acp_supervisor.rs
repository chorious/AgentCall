use crate::state::{AppState, append_agent_event_locked, read_json_file, write_json_file};
use crate::util::now_ms;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

pub(crate) const DEFAULT_ACP_TIMEOUT_SECONDS: u64 = 1800;
pub(crate) const DEFAULT_ACP_CHECKPOINT_DUE_SECONDS: u64 = 600;
pub(crate) const DEFAULT_ACP_HEARTBEAT_SECONDS: u64 = 60;
pub(crate) const DEFAULT_ACP_MAX_ACTIVE: usize = 2;

#[derive(Clone, Debug)]
pub(crate) struct AcpSupervisorConfig {
    pub(crate) default_timeout_seconds: u64,
    pub(crate) max_timeout_seconds: u64,
    pub(crate) checkpoint_due_seconds: u64,
    pub(crate) heartbeat_interval_seconds: u64,
    pub(crate) max_active_invocations: usize,
}

impl AcpSupervisorConfig {
    pub(crate) fn from_state(state: &AppState) -> Self {
        let max_timeout = state
            .config
            .acp_max_timeout_seconds
            .unwrap_or(DEFAULT_ACP_TIMEOUT_SECONDS)
            .max(1);
        let default_timeout = state
            .config
            .acp_default_timeout_seconds
            .unwrap_or(DEFAULT_ACP_TIMEOUT_SECONDS)
            .max(1)
            .min(max_timeout);
        Self {
            default_timeout_seconds: default_timeout,
            max_timeout_seconds: max_timeout,
            checkpoint_due_seconds: state
                .config
                .acp_checkpoint_due_seconds
                .unwrap_or(DEFAULT_ACP_CHECKPOINT_DUE_SECONDS)
                .max(1),
            heartbeat_interval_seconds: state
                .config
                .acp_heartbeat_interval_seconds
                .unwrap_or(DEFAULT_ACP_HEARTBEAT_SECONDS)
                .max(1),
            max_active_invocations: state
                .config
                .acp_max_active_invocations
                .unwrap_or(DEFAULT_ACP_MAX_ACTIVE)
                .max(1),
        }
    }
}

#[derive(Clone)]
pub(crate) struct AcpInvocationStart {
    pub(crate) route_id: String,
    pub(crate) invocation_id: String,
    pub(crate) task_id: String,
    pub(crate) call_id: String,
    pub(crate) workspace: PathBuf,
    pub(crate) cwd: PathBuf,
    pub(crate) template: String,
    pub(crate) report_path: String,
    pub(crate) command: Vec<String>,
    pub(crate) hard_timeout_seconds: u64,
    pub(crate) checkpoint_due_after_seconds: u64,
    pub(crate) heartbeat_interval_seconds: u64,
}

pub(crate) fn acp_invocations_state(state: &AppState) -> Value {
    read_json_file(&acp_invocations_path(state), json!({}))
}

pub(crate) fn active_invocation_count(state: &AppState) -> usize {
    acp_invocations_state(state)
        .as_object()
        .map(|items| {
            items
                .values()
                .filter(|item| is_active_status(item.get("status").and_then(Value::as_str)))
                .count()
        })
        .unwrap_or(0)
}

pub(crate) fn orphaned_invocation_count(state: &AppState) -> usize {
    acp_invocations_state(state)
        .as_object()
        .map(|items| {
            items
                .values()
                .filter(|item| {
                    item.get("status").and_then(Value::as_str)
                        == Some("orphaned_after_daemon_restart")
                })
                .count()
        })
        .unwrap_or(0)
}

pub(crate) fn mark_orphaned_on_start(state: &AppState) {
    let agent_dir = state.workspace.join(".agentcall");
    let _guard = state.state_writer.lock().unwrap();
    let path = agent_dir.join("state").join("acp_invocations.json");
    let mut invocations = read_json_file(&path, json!({}));
    let now = now_ms();
    let mut orphaned = Vec::new();
    if let Some(items) = invocations.as_object_mut() {
        for (invocation_id, item) in items.iter_mut() {
            if !is_active_status(item.get("status").and_then(Value::as_str)) {
                continue;
            }
            if let Some(object) = item.as_object_mut() {
                object.insert("status".to_string(), json!("orphaned_after_daemon_restart"));
                object.insert("sop_status".to_string(), json!("orphaned"));
                object.insert("updated_at".to_string(), json!(now));
                object.insert("orphaned_at".to_string(), json!(now));
                object.insert(
                    "rerun_required".to_string(),
                    json!("daemon lost ownership of the ACP child process"),
                );
            }
            orphaned.push(invocation_id.clone());
        }
    }
    if !orphaned.is_empty() {
        let _ = write_json_file(&path, &invocations);
        for invocation_id in orphaned {
            let _ = append_agent_event_locked(
                state,
                &agent_dir,
                "acp.invocation_orphaned",
                "ACP invocation marked orphaned after daemon restart.",
                json!({"invocation_id": invocation_id, "runtime": "acp"}),
            );
        }
    }
}

pub(crate) fn record_started(state: &AppState, start: AcpInvocationStart) -> Result<(), String> {
    let agent_dir = state.workspace.join(".agentcall");
    let _guard = state.state_writer.lock().unwrap();
    let path = agent_dir.join("state").join("acp_invocations.json");
    let mut invocations = read_json_file(&path, json!({}));
    let now = now_ms();
    if let Some(items) = invocations.as_object_mut() {
        items.insert(
            start.invocation_id.clone(),
            json!({
                "route_id": start.route_id,
                "invocation_id": start.invocation_id,
                "task_id": start.task_id,
                "call_id": start.call_id,
                "runtime": "acp",
                "status": "running",
                "sop_status": "running",
                "workspace": start.workspace,
                "cwd": start.cwd,
                "template": start.template,
                "report_path": start.report_path,
                "command": start.command,
                "pid": null,
                "started_at": now,
                "updated_at": now,
                "last_heartbeat_at": now,
                "last_progress_at": now,
                "heartbeat_count": 0,
                "progress_update_count": 0,
                "permission_denials": [],
                "report_contract_status": "pending",
                "checkpoint_due": false,
                "checkpoint_emitted": false,
                "hard_timeout_seconds": start.hard_timeout_seconds,
                "checkpoint_due_after_seconds": start.checkpoint_due_after_seconds,
                "heartbeat_interval_seconds": start.heartbeat_interval_seconds,
            }),
        );
    }
    write_json_file(&path, &invocations)
}

pub(crate) fn record_progress(state: &AppState, invocation_id: &str, update: Value) {
    let agent_dir = state.workspace.join(".agentcall");
    let _guard = state.state_writer.lock().unwrap();
    let path = agent_dir.join("state").join("acp_invocations.json");
    let mut invocations = read_json_file(&path, json!({}));
    let now = now_ms();
    if let Some(item) = invocations
        .get_mut(invocation_id)
        .and_then(Value::as_object_mut)
    {
        if let Some(pid) = update.get("pid").and_then(Value::as_u64) {
            item.insert("pid".to_string(), json!(pid));
        }
        if update.get("kind").and_then(Value::as_str) == Some("permission_denied") {
            append_array_item(
                item,
                "permission_denials",
                compact_permission_denial(&update),
            );
        } else if update.get("kind").and_then(Value::as_str) != Some("process_started") {
            let count = item
                .get("progress_update_count")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1;
            item.insert("progress_update_count".to_string(), json!(count));
            item.insert("last_progress_at".to_string(), json!(now));
            item.insert("checkpoint_due".to_string(), json!(false));
        }
        item.insert("updated_at".to_string(), json!(now));
    }
    let _ = write_json_file(&path, &invocations);
}

pub(crate) fn record_finished(
    state: &AppState,
    invocation_id: &str,
    status: &str,
    report_contract_status: &str,
    result_summary: Value,
) {
    let agent_dir = state.workspace.join(".agentcall");
    let _guard = state.state_writer.lock().unwrap();
    let path = agent_dir.join("state").join("acp_invocations.json");
    let mut invocations = read_json_file(&path, json!({}));
    let now = now_ms();
    if let Some(item) = invocations
        .get_mut(invocation_id)
        .and_then(Value::as_object_mut)
    {
        item.insert("status".to_string(), json!(status));
        item.insert("sop_status".to_string(), json!(status));
        item.insert(
            "report_contract_status".to_string(),
            json!(report_contract_status),
        );
        item.insert("checkpoint_due".to_string(), json!(false));
        item.insert("updated_at".to_string(), json!(now));
        item.insert("finished_at".to_string(), json!(now));
        item.insert("result_summary".to_string(), result_summary);
    }
    let _ = write_json_file(&path, &invocations);
}

pub(crate) fn start_heartbeat(
    state: Arc<AppState>,
    route_id: String,
    invocation_id: String,
    done: Arc<AtomicBool>,
    config: AcpSupervisorConfig,
) {
    thread::spawn(move || {
        let sleep = Duration::from_secs(config.heartbeat_interval_seconds);
        while !done.load(Ordering::SeqCst) {
            thread::sleep(sleep);
            if done.load(Ordering::SeqCst) {
                break;
            }
            heartbeat_once(&state, &route_id, &invocation_id, &config);
        }
    });
}

fn heartbeat_once(
    state: &AppState,
    route_id: &str,
    invocation_id: &str,
    config: &AcpSupervisorConfig,
) {
    let agent_dir = state.workspace.join(".agentcall");
    let _guard = state.state_writer.lock().unwrap();
    let path = agent_dir.join("state").join("acp_invocations.json");
    let mut invocations = read_json_file(&path, json!({}));
    let now = now_ms();
    let mut checkpoint_event = false;
    if let Some(item) = invocations
        .get_mut(invocation_id)
        .and_then(Value::as_object_mut)
    {
        if !is_active_status(item.get("status").and_then(Value::as_str)) {
            return;
        }
        let heartbeat_count = item
            .get("heartbeat_count")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            + 1;
        item.insert("heartbeat_count".to_string(), json!(heartbeat_count));
        item.insert("last_heartbeat_at".to_string(), json!(now));
        item.insert("updated_at".to_string(), json!(now));
        let last_progress = item
            .get("last_progress_at")
            .and_then(Value::as_u64)
            .unwrap_or(now);
        let elapsed = now.saturating_sub(last_progress);
        let checkpoint_ms = config.checkpoint_due_seconds.saturating_mul(1000);
        let checkpoint_emitted = item
            .get("checkpoint_emitted")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if elapsed >= checkpoint_ms && !checkpoint_emitted {
            item.insert("status".to_string(), json!("checkpoint_due"));
            item.insert("sop_status".to_string(), json!("checkpoint_due"));
            item.insert("checkpoint_due".to_string(), json!(true));
            item.insert("checkpoint_emitted".to_string(), json!(true));
            checkpoint_event = true;
        }
    }
    let _ = write_json_file(&path, &invocations);
    if checkpoint_event {
        let _ = append_agent_event_locked(
            state,
            &agent_dir,
            "acp.checkpoint_due",
            "ACP invocation has no recent progress; consider rerouting to PTY.",
            json!({
                "route_id": route_id,
                "invocation_id": invocation_id,
                "runtime": "acp",
                "checkpoint_due_after_seconds": config.checkpoint_due_seconds,
            }),
        );
    }
}

fn acp_invocations_path(state: &AppState) -> PathBuf {
    state
        .workspace
        .join(".agentcall")
        .join("state")
        .join("acp_invocations.json")
}

fn is_active_status(status: Option<&str>) -> bool {
    matches!(status, Some("running" | "started" | "checkpoint_due"))
}

fn append_array_item(object: &mut serde_json::Map<String, Value>, key: &str, value: Value) {
    let entry = object.entry(key.to_string()).or_insert_with(|| json!([]));
    if let Some(items) = entry.as_array_mut() {
        items.push(value);
    }
}

fn compact_permission_denial(update: &Value) -> Value {
    json!({
        "at": now_ms(),
        "tool": update.get("tool").and_then(Value::as_str),
        "paths": update.get("paths").cloned().unwrap_or_else(|| json!([])),
        "reason": update.get("reason").and_then(Value::as_str).unwrap_or("ACP SOP policy denied the permission request"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use std::fs;

    #[test]
    fn active_count_and_orphan_marking_use_invocation_state() {
        let root = test_workspace("acp-orphan");
        let state = AppState::test(root.clone());
        record_started(
            &state,
            AcpInvocationStart {
                route_id: "route-1".to_string(),
                invocation_id: "acp-1".to_string(),
                task_id: "task".to_string(),
                call_id: "call".to_string(),
                workspace: root.clone(),
                cwd: root.clone(),
                template: "read-and-report".to_string(),
                report_path: root.join("report.md").to_string_lossy().to_string(),
                command: vec!["fake-acp".to_string()],
                hard_timeout_seconds: 1800,
                checkpoint_due_after_seconds: 600,
                heartbeat_interval_seconds: 60,
            },
        )
        .unwrap();
        assert_eq!(active_invocation_count(&state), 1);
        mark_orphaned_on_start(&state);
        assert_eq!(active_invocation_count(&state), 0);
        assert_eq!(orphaned_invocation_count(&state), 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn heartbeat_marks_checkpoint_once_without_finishing() {
        let root = test_workspace("acp-checkpoint");
        let state = AppState::test(root.clone());
        record_started(
            &state,
            AcpInvocationStart {
                route_id: "route-2".to_string(),
                invocation_id: "acp-2".to_string(),
                task_id: "task".to_string(),
                call_id: "call".to_string(),
                workspace: root.clone(),
                cwd: root.clone(),
                template: "read-and-report".to_string(),
                report_path: root.join("report.md").to_string_lossy().to_string(),
                command: vec!["fake-acp".to_string()],
                hard_timeout_seconds: 1800,
                checkpoint_due_after_seconds: 1,
                heartbeat_interval_seconds: 1,
            },
        )
        .unwrap();
        let path = acp_invocations_path(&state);
        let mut state_json = read_json_file(&path, json!({}));
        state_json["acp-2"]["last_progress_at"] = json!(now_ms().saturating_sub(2000));
        write_json_file(&path, &state_json).unwrap();
        heartbeat_once(
            &state,
            "route-2",
            "acp-2",
            &AcpSupervisorConfig {
                default_timeout_seconds: 1800,
                max_timeout_seconds: 1800,
                checkpoint_due_seconds: 1,
                heartbeat_interval_seconds: 1,
                max_active_invocations: 2,
            },
        );
        let invocations = acp_invocations_state(&state);
        assert_eq!(invocations["acp-2"]["status"], "checkpoint_due");
        assert_eq!(invocations["acp-2"]["checkpoint_due"], true);
        let _ = fs::remove_dir_all(root);
    }

    fn test_workspace(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "agentcall-acp-supervisor-{name}-{}",
            std::process::id()
        ))
    }
}
