use crate::events::EventEnvelopeV1;
use crate::session::{Session, SessionInfo};
use crate::state::AppState;
use crate::store::BoardQuery;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SessionProjectionV1 {
    pub(crate) schema_version: u16,
    pub(crate) projection_version: u64,
    pub(crate) projection_last_global_seq: u64,
    pub(crate) projection_last_session_seq: u64,
    pub(crate) projection_last_updated_at: String,
    pub(crate) projection_stale: bool,
    pub(crate) session_id: String,
    pub(crate) run_id: Option<String>,
    pub(crate) owner: Option<String>,
    pub(crate) workspace: String,
    pub(crate) claude_cwd: String,
    pub(crate) runtime: String,
    pub(crate) liveness_status: String,
    pub(crate) turn_status: String,
    pub(crate) attention_status: String,
    pub(crate) needs_attention: bool,
    pub(crate) current_task: String,
    pub(crate) pending_interaction: Value,
    pub(crate) last_progress_age_seconds: u64,
    pub(crate) last_progress_brief: Option<String>,
    pub(crate) patience_status: String,
    pub(crate) suggested_wait_seconds: u64,
    pub(crate) next_recommended_action: String,
    pub(crate) files_written_count: usize,
    pub(crate) report_ready: bool,
    pub(crate) last_error_brief: Option<String>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectionUpdate {
    pub(crate) projection: SessionProjectionV1,
    pub(crate) changed: bool,
    pub(crate) reason: String,
}

#[allow(dead_code)]
pub(crate) fn default_session_projection(session: &SessionInfo) -> SessionProjectionV1 {
    SessionProjectionV1 {
        schema_version: 1,
        projection_version: 1,
        projection_last_global_seq: 0,
        projection_last_session_seq: 0,
        projection_last_updated_at: chrono::Utc::now().to_rfc3339(),
        projection_stale: false,
        session_id: session.name.clone(),
        run_id: None,
        owner: None,
        workspace: session.cwd.clone(),
        claude_cwd: session.cwd.clone(),
        runtime: "pty".to_string(),
        liveness_status: session.status.clone(),
        turn_status: "unknown".to_string(),
        attention_status: "none".to_string(),
        needs_attention: false,
        current_task: String::new(),
        pending_interaction: serde_json::json!(null),
        last_progress_age_seconds: 0,
        last_progress_brief: None,
        patience_status: "unknown".to_string(),
        suggested_wait_seconds: 60,
        next_recommended_action: "inspect_summary".to_string(),
        files_written_count: 0,
        report_ready: false,
        last_error_brief: None,
        warnings: vec![],
    }
}

pub(crate) fn stale_projection_for_session_name(session_name: &str) -> SessionProjectionV1 {
    SessionProjectionV1 {
        schema_version: 1,
        projection_version: 0,
        projection_last_global_seq: 0,
        projection_last_session_seq: 0,
        projection_last_updated_at: chrono::Utc::now().to_rfc3339(),
        projection_stale: true,
        session_id: session_name.to_string(),
        run_id: None,
        owner: None,
        workspace: String::new(),
        claude_cwd: String::new(),
        runtime: "unknown".to_string(),
        liveness_status: "unknown".to_string(),
        turn_status: "unknown".to_string(),
        attention_status: "low_confidence".to_string(),
        needs_attention: true,
        current_task: String::new(),
        pending_interaction: serde_json::json!(null),
        last_progress_age_seconds: 0,
        last_progress_brief: None,
        patience_status: "unknown".to_string(),
        suggested_wait_seconds: 0,
        next_recommended_action: "inspect_session_debug".to_string(),
        files_written_count: 0,
        report_ready: false,
        last_error_brief: None,
        warnings: vec!["projection missing; default path did not scan cold logs".to_string()],
    }
}

#[allow(dead_code)]
pub(crate) fn bootstrap_stale_projection_from_existing_session(
    _state: &AppState,
    session: &Arc<Session>,
) -> SessionProjectionV1 {
    let mut projection = stale_projection_for_session_name(&session.name);
    projection.workspace = session.cwd.display().to_string();
    projection.claude_cwd = session.cwd.display().to_string();
    projection.runtime = "pty".to_string();
    projection.liveness_status = session.status.lock().unwrap().clone();
    projection
}

pub(crate) fn read_session_projection(
    state: &AppState,
    session_name: &str,
) -> Option<SessionProjectionV1> {
    if let Some(projection) = state.projections.lock().unwrap().get(session_name).cloned() {
        return Some(projection);
    }
    state
        .store
        .get_session_projection(session_name)
        .ok()
        .flatten()
}

pub(crate) fn session_projection_summary(state: &AppState, session_name: &str) -> Value {
    let projection = read_session_projection(state, session_name)
        .unwrap_or_else(|| stale_projection_for_session_name(session_name));
    let mut value = serde_json::json!(projection);
    value["projection_only"] = serde_json::json!(true);
    value["session"] = value
        .get("session_id")
        .cloned()
        .unwrap_or_else(|| serde_json::json!(session_name));
    value
}

pub(crate) fn apply_event_to_projection(
    previous: Option<SessionProjectionV1>,
    event: &EventEnvelopeV1,
) -> ProjectionUpdate {
    let Some(session_id) = event.session_id.clone() else {
        return ProjectionUpdate {
            projection: stale_projection_for_session_name("unbound"),
            changed: false,
            reason: "event has no session_id".to_string(),
        };
    };
    let mut projection = previous.unwrap_or_else(|| stale_projection_for_session_name(&session_id));
    projection.projection_stale = false;
    projection.projection_version = projection.projection_version.saturating_add(1);
    projection.projection_last_global_seq = event.global_seq;
    projection.projection_last_session_seq = event.session_seq.unwrap_or(0);
    projection.projection_last_updated_at = event.ts.clone();
    projection.session_id = session_id;
    projection.run_id = event.run_id.clone();
    projection.owner = event.owner_id.clone();

    let mut reason = "event_reduced".to_string();
    match event.event_type.as_str() {
        "session.started" | "process.started" | "mcp.tool_called" | "pty.session_started" => {
            projection.liveness_status = "working".to_string();
            projection.attention_status = "none".to_string();
            projection.needs_attention = false;
            projection.last_progress_brief = Some(event.message.clone());
            if let Some(workspace) = event.payload.get("cwd").and_then(Value::as_str) {
                projection.workspace = workspace.to_string();
                projection.claude_cwd = workspace.to_string();
            }
        }
        "command.accepted" | "command.completed" | "pty.input_sent" => {
            projection.liveness_status = "working".to_string();
            projection.attention_status = "none".to_string();
            projection.needs_attention = false;
            projection.last_progress_brief = Some(event.message.clone());
        }
        "command.awaiting_observation" => {
            projection.liveness_status = "working".to_string();
            projection.turn_status = "awaiting_observation".to_string();
            projection.attention_status = "none".to_string();
            projection.needs_attention = false;
            projection.last_progress_brief = Some(event.message.clone());
        }
        "session.cleanup" | "process.exited" | "pty.session_ended" => {
            projection.liveness_status = "completed".to_string();
            projection.attention_status = "none".to_string();
            projection.needs_attention = false;
            projection.last_progress_brief = Some(event.message.clone());
        }
        "pty.stop_requested" => {
            projection.liveness_status = "stopping".to_string();
            projection.turn_status = "awaiting_observation".to_string();
            projection.attention_status = "none".to_string();
            projection.needs_attention = false;
            projection.last_progress_brief = Some(event.message.clone());
        }
        event_type if event_type.starts_with("hook.") => {
            reduce_hook_event(&mut projection, event);
            reason = "hook_event_reduced".to_string();
        }
        event_type if event_type.contains("policy") || event_type.contains("denial") => {
            projection.attention_status = "blocked_by_policy".to_string();
            projection.needs_attention = true;
            projection.last_error_brief = Some(event.message.clone());
        }
        _ => {
            projection.last_progress_brief = Some(event.message.clone());
        }
    }

    ProjectionUpdate {
        projection,
        changed: true,
        reason,
    }
}

pub(crate) fn board_attention_projection(state: &AppState, owner_id: Option<&str>) -> Value {
    let all = state
        .store
        .list_board_projection(BoardQuery {
            attention_only: false,
            owner_id: owner_id.map(str::to_string),
        })
        .unwrap_or_else(|_| serde_json::json!({"sessions": []}));
    let attention_projection = state
        .store
        .list_board_projection(BoardQuery {
            attention_only: true,
            owner_id: owner_id.map(str::to_string),
        })
        .unwrap_or_else(|_| serde_json::json!({"sessions": []}));
    let live_daemon_sessions = projection_items(all.get("sessions").and_then(Value::as_array));
    let attention = projection_items(
        attention_projection
            .get("sessions")
            .and_then(Value::as_array),
    );
    serde_json::json!({
        "workspace": state.workspace,
        "view": "compact",
        "filter": "attention",
        "owner_id": owner_id,
        "projection_only": true,
        "store_backend": state.store.backend_name(),
        "live_daemon_sessions": live_daemon_sessions,
        "attention": attention,
    })
}

fn projection_items(items: Option<&Vec<Value>>) -> Vec<Value> {
    items
        .into_iter()
        .flatten()
        .map(|projection| {
            serde_json::json!({
                "session": projection.get("session_id").cloned().unwrap_or(Value::Null),
                "owner": projection.get("owner").cloned().unwrap_or(Value::Null),
                "liveness_status": projection.get("liveness_status").cloned().unwrap_or(Value::Null),
                "attention_status": projection.get("attention_status").cloned().unwrap_or(Value::Null),
                "needs_attention": projection.get("needs_attention").cloned().unwrap_or(Value::Null),
                "projection_stale": projection.get("projection_stale").cloned().unwrap_or(Value::Null),
                "projection_last_global_seq": projection.get("projection_last_global_seq").cloned().unwrap_or(Value::Null),
                "projection_last_session_seq": projection.get("projection_last_session_seq").cloned().unwrap_or(Value::Null),
                "last_progress_brief": projection.get("last_progress_brief").cloned().unwrap_or(Value::Null),
                "next_recommended_action": projection.get("next_recommended_action").cloned().unwrap_or(Value::Null),
                "warnings": projection.get("warnings").cloned().unwrap_or_else(|| serde_json::json!([])),
            })
        })
        .collect()
}

fn reduce_hook_event(projection: &mut SessionProjectionV1, event: &EventEnvelopeV1) {
    let status = event
        .payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("working");
    projection.liveness_status = match status {
        "needs_permission" => "needs_permission",
        "waiting_input" => "waiting_input",
        "checkpoint_due" => "checkpoint_due",
        "idle" => "idle",
        _ => "working",
    }
    .to_string();
    projection.attention_status = match status {
        "needs_permission" => "needs_permission",
        "waiting_input" => "waiting_input",
        "checkpoint_due" => "checkpoint_due",
        _ => "none",
    }
    .to_string();
    projection.needs_attention = projection.attention_status != "none";
    projection.last_progress_brief = Some(event.message.clone());
    if let Some(workspace) = event.payload.get("workspace").and_then(Value::as_str) {
        projection.workspace = workspace.to_string();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_default_has_stale_false() {
        let projection = default_session_projection(&SessionInfo {
            name: "worker-a".to_string(),
            command: vec!["claude".to_string()],
            cwd: "D:\\guKimi".to_string(),
            status: "running".to_string(),
            child_pid: Some(1234),
            created_at: 1,
            updated_at: 1,
            replay_bytes: 0,
            decode_health: crate::terminal::DecodeHealth::default(),
        });
        assert!(!projection.projection_stale);
        assert_eq!(projection.session_id, "worker-a");
    }

    #[test]
    fn reducer_updates_projection_from_hook_event() {
        let event = EventEnvelopeV1 {
            schema_version: 1,
            event_id: "evt-1".to_string(),
            global_seq: 1,
            session_seq: Some(1),
            session_id: Some("worker-a".to_string()),
            run_id: None,
            owner_id: None,
            ts: "2026-06-09T00:00:00Z".to_string(),
            source: "hook".to_string(),
            event_type: "hook.Notification".to_string(),
            severity: "info".to_string(),
            command_id: None,
            idempotency_key: None,
            trace_id: None,
            message: "permission requested".to_string(),
            payload: serde_json::json!({"status": "needs_permission"}),
        };
        let update = apply_event_to_projection(None, &event);
        assert!(update.changed);
        assert_eq!(update.projection.session_id, "worker-a");
        assert_eq!(update.projection.liveness_status, "needs_permission");
        assert_eq!(update.projection.attention_status, "needs_permission");
        assert!(update.projection.needs_attention);
    }

    #[test]
    fn reducer_tracks_real_pty_lifecycle_events() {
        let started = apply_event_to_projection(
            None,
            &test_projection_event(1, "worker-a", "pty.session_started", "PTY session started."),
        );
        assert_eq!(started.projection.liveness_status, "working");
        assert_eq!(started.projection.attention_status, "none");
        assert!(!started.projection.projection_stale);

        let stopped = apply_event_to_projection(
            Some(started.projection),
            &test_projection_event(2, "worker-a", "pty.stop_requested", "PTY stop requested."),
        );
        assert_eq!(stopped.projection.liveness_status, "stopping");
        assert_eq!(stopped.projection.turn_status, "awaiting_observation");
        assert_eq!(stopped.projection.attention_status, "none");

        let ended = apply_event_to_projection(
            Some(stopped.projection),
            &test_projection_event(3, "worker-a", "pty.session_ended", "PTY session ended."),
        );
        assert_eq!(ended.projection.liveness_status, "completed");
        assert_eq!(ended.projection.attention_status, "none");
    }

    #[test]
    fn reducer_tracks_actor_command_events() {
        let accepted = apply_event_to_projection(
            None,
            &test_projection_event(
                1,
                "worker-a",
                "command.accepted",
                "Session actor accepted command.",
            ),
        );
        assert_eq!(accepted.projection.liveness_status, "working");
        assert_eq!(accepted.projection.attention_status, "none");

        let awaiting = apply_event_to_projection(
            Some(accepted.projection),
            &test_projection_event(
                2,
                "worker-a",
                "command.awaiting_observation",
                "Session actor dispatched command and is waiting for observed worker state.",
            ),
        );
        assert_eq!(awaiting.projection.liveness_status, "working");
        assert_eq!(awaiting.projection.turn_status, "awaiting_observation");
        assert_eq!(awaiting.projection.attention_status, "none");
    }

    #[test]
    fn missing_projection_summary_is_stale_without_cold_scan() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-projection-missing-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        let summary = session_projection_summary(&state, "worker-missing");
        assert_eq!(summary["projection_only"], true);
        assert_eq!(summary["projection_stale"], true);
        assert_eq!(summary["session"], "worker-missing");
        assert_eq!(summary["attention_status"], "low_confidence");
        let _ = std::fs::remove_dir_all(root);
    }

    fn test_projection_event(
        global_seq: u64,
        session_id: &str,
        event_type: &str,
        message: &str,
    ) -> EventEnvelopeV1 {
        EventEnvelopeV1 {
            schema_version: 1,
            event_id: format!("evt-{global_seq:06}"),
            global_seq,
            session_seq: Some(global_seq),
            session_id: Some(session_id.to_string()),
            run_id: None,
            owner_id: Some("codex".to_string()),
            ts: chrono::Utc::now().to_rfc3339(),
            source: "daemon".to_string(),
            event_type: event_type.to_string(),
            severity: "info".to_string(),
            command_id: None,
            idempotency_key: None,
            trace_id: None,
            message: message.to_string(),
            payload: serde_json::json!({"session_id": session_id, "cwd": "E:\\Project\\AgentCall"}),
        }
    }
}
