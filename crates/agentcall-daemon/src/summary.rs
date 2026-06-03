use crate::session::{Session, default_claude_workspace, list_sessions};
use crate::state::{AppState, read_events, read_json_file};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub(crate) fn board_state(state: &AppState) -> serde_json::Value {
    let agent_dir = state.workspace.join(".agentcall");
    let events = read_events(&agent_dir.join("events.ndjson"));
    let project_state = read_json_file(
        &agent_dir.join("state").join("project.json"),
        serde_json::json!({}),
    );
    let active_sessions = read_json_file(
        &agent_dir.join("state").join("active_sessions.json"),
        serde_json::json!({}),
    );
    let file_claims = read_json_file(
        &agent_dir.join("state").join("file_claims.json"),
        serde_json::json!({}),
    );
    let transcripts = read_json_file(
        &agent_dir.join("state").join("transcripts.json"),
        serde_json::json!({}),
    );
    let reports = read_reports(&agent_dir.join("tasks"));
    serde_json::json!({
        "workspace": state.workspace,
        "pty_sessions": list_sessions(state),
        "active_sessions": active_sessions,
        "file_claims": file_claims,
        "transcripts": transcripts,
        "reports": reports,
        "recent_events": events,
        "project_state": project_state
    })
}

pub(crate) fn runtime_health(state: &AppState) -> serde_json::Value {
    let sessions = list_sessions(state);
    let agent_dir = state.workspace.join(".agentcall");
    let stale_claims = stale_claim_count(&agent_dir.join("state").join("file_claims.json"));
    serde_json::json!({
        "runtime": "agentcall-daemon",
        "workspace": state.workspace,
        "state_writer": "daemon",
        "event_next": state.event_seq.load(Ordering::SeqCst),
        "active_pty_sessions": sessions.len(),
        "stale_claims": stale_claims,
        "status": "ok"
    })
}

pub(crate) fn projects_state(state: &AppState) -> serde_json::Value {
    serde_json::json!({
        "projects": [{
            "name": state.workspace.file_name().and_then(|name| name.to_str()).unwrap_or("workspace"),
            "workspace": state.workspace,
            "sessions": list_sessions(state),
        }]
    })
}

pub(crate) fn session_summary(state: &AppState, session: &Arc<Session>) -> serde_json::Value {
    let status = session.status.lock().unwrap().clone();
    let replay_text = {
        let bytes = session.replay.lock().unwrap().clone();
        String::from_utf8_lossy(&bytes).to_string()
    };
    let waiting_input = looks_like_waiting_for_input(&replay_text);
    let summary_status = if waiting_input {
        "waiting_input".to_string()
    } else if status.starts_with("exited") {
        "completed".to_string()
    } else if status.starts_with("error") {
        "failed".to_string()
    } else {
        "working".to_string()
    };
    let agent_dir = state.workspace.join(".agentcall");
    let claims = read_json_file(
        &agent_dir.join("state").join("file_claims.json"),
        serde_json::json!({}),
    );
    let claimed_files: Vec<String> = claims
        .as_object()
        .map(|items| {
            items
                .iter()
                .filter(|(_, claim)| {
                    claim.get("status").and_then(|value| value.as_str()) == Some("active")
                })
                .filter(|(_, claim)| {
                    claim.get("session_id").and_then(|value| value.as_str())
                        == Some(session.name.as_str())
                })
                .map(|(file, _)| file.clone())
                .collect()
        })
        .unwrap_or_default();
    serde_json::json!({
        "session": session.name,
        "project": state.workspace.file_name().and_then(|name| name.to_str()).unwrap_or("workspace"),
        "transport": "pty",
        "status": summary_status,
        "workspace": state.workspace,
        "cwd": session.cwd,
        "claude_workspace": default_claude_workspace(),
        "last_hook_at": null,
        "last_tool": null,
        "claimed_files": claimed_files,
        "files_written": [],
        "report": null,
        "needs_user_input": waiting_input,
        "warnings": [],
        "conflicts": []
    })
}

pub(crate) fn looks_like_waiting_for_input(text: &str) -> bool {
    let tail = text
        .lines()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    tail.contains("waiting for your input")
        || tail.trim_end().ends_with('>')
        || tail.contains("? for shortcuts")
}

pub(crate) fn stale_claim_count(path: &Path) -> usize {
    let claims = read_json_file(path, serde_json::json!({}));
    claims
        .as_object()
        .map(|items| {
            items
                .values()
                .filter(|claim| {
                    claim.get("status").and_then(|value| value.as_str()) == Some("stale")
                })
                .count()
        })
        .unwrap_or(0)
}

pub(crate) fn read_reports(tasks_dir: &Path) -> Vec<serde_json::Value> {
    let mut reports = vec![];
    let Ok(tasks) = fs::read_dir(tasks_dir) else {
        return reports;
    };
    for task in tasks.flatten() {
        let reports_dir = task.path().join("reports");
        let Ok(entries) = fs::read_dir(reports_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            reports.push(read_json_file(&entry.path(), serde_json::json!({})));
        }
    }
    reports
}
