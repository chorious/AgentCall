use crate::session::{StartRequest, start_session};
use crate::state::{AppState, append_agent_event_locked, read_json_file, write_json_file};
use crate::util::{now_ms, safe_name};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
pub(crate) struct RouteRequest {
    objective: String,
    workspace: Option<String>,
    mode: Option<String>,
    runtime: Option<String>,
    estimated_minutes: Option<u64>,
    estimated_files: Option<u64>,
    estimated_loc: Option<u64>,
    needs_continuity: Option<bool>,
    risk: Option<String>,
    session_name: Option<String>,
    command: Option<Vec<String>>,
    adapter_command: Option<Vec<String>>,
    timeout_seconds: Option<u64>,
    task_id: Option<String>,
    call_id: Option<String>,
    phase: Option<String>,
    role: Option<String>,
    allowed_paths: Option<Vec<String>>,
    acceptance_criteria: Option<Vec<String>>,
    persist_context: Option<bool>,
}

#[derive(Clone, Serialize)]
struct RouteRecord {
    route_id: String,
    invocation_id: Option<String>,
    objective: String,
    workspace: Option<String>,
    mode: String,
    runtime: String,
    recommended_runtime: String,
    status: String,
    reason: String,
    score_breakdown: Value,
    required_next_step: String,
    session_name: Option<String>,
    created_at: u64,
    updated_at: u64,
    result: Value,
}

pub(crate) fn handle_route(state: &Arc<AppState>, req: RouteRequest) -> Result<Value, String> {
    if req.objective.trim().is_empty() {
        return Err("missing objective".to_string());
    }
    let mode = req.mode.as_deref().unwrap_or("recommend");
    if !matches!(mode, "recommend" | "start") {
        return Err("mode must be recommend or start".to_string());
    }
    let runtime = req.runtime.as_deref().unwrap_or("auto");
    if !matches!(runtime, "auto" | "pty" | "acp") {
        return Err("runtime must be auto, pty, or acp".to_string());
    }
    if runtime == "auto" && !has_auto_estimates(&req) {
        return Err(
            "runtime=auto requires estimated_minutes and estimated_files or estimated_loc"
                .to_string(),
        );
    }

    let route_id = format!("route-{}", state.next_seq());
    let decision = route_decision(&req);
    let mut record = RouteRecord {
        route_id: route_id.clone(),
        invocation_id: None,
        objective: req.objective.clone(),
        workspace: req.workspace.clone(),
        mode: mode.to_string(),
        runtime: runtime.to_string(),
        recommended_runtime: decision.runtime.clone(),
        status: "recommended".to_string(),
        reason: decision.reason.clone(),
        score_breakdown: decision.score_breakdown.clone(),
        required_next_step: if mode == "start" {
            "inspect board/session/report".to_string()
        } else {
            "call agentcall_route with mode=start and explicit runtime".to_string()
        },
        session_name: None,
        created_at: now_ms(),
        updated_at: now_ms(),
        result: json!({}),
    };

    if mode == "start" {
        match decision.runtime.as_str() {
            "pty" => start_pty_route(state, &req, &mut record)?,
            "acp" => start_acp_route(state, &req, &mut record)?,
            other => return Err(format!("unsupported route runtime: {other}")),
        }
    }
    if route_has_context_fields(&req) {
        let context_packet = create_context(
            state,
            ContextRequest {
                task_id: req
                    .task_id
                    .clone()
                    .ok_or("task_id is required when route context fields are provided")?,
                call_id: req
                    .call_id
                    .clone()
                    .ok_or("call_id is required when route context fields are provided")?,
                objective: req.objective.clone(),
                phase: req.phase.clone(),
                role: req.role.clone(),
                runtime: Some(decision.runtime.clone()),
                workspace: req.workspace.clone(),
                allowed_paths: req.allowed_paths.clone(),
                acceptance_criteria: req.acceptance_criteria.clone(),
                persist: req.persist_context,
            },
        )?;
        merge_result_field(&mut record.result, "context_packet", context_packet);
    }

    upsert_route_record(state, &record)?;
    Ok(json!(record))
}

pub(crate) fn route_state(state: &AppState, id: &str) -> Option<Value> {
    let routes = read_routes(state);
    routes.get(id).cloned()
}

pub(crate) fn routes_state(state: &AppState) -> Value {
    let routes = read_routes(state);
    let mut values: Vec<Value> = routes
        .as_object()
        .map(|items| items.values().cloned().collect())
        .unwrap_or_default();
    values.sort_by(|a, b| {
        a.get("route_id")
            .and_then(Value::as_str)
            .cmp(&b.get("route_id").and_then(Value::as_str))
    });
    json!(values)
}

pub(crate) fn checkpoint_session(state: &Arc<AppState>, session_id: &str) -> Result<Value, String> {
    if session_id.trim().is_empty() {
        return Err("missing session_id".to_string());
    }
    let agent_dir = state.workspace.join(".agentcall");
    let _guard = state.state_writer.lock().unwrap();
    let path = agent_dir.join("state").join("active_sessions.json");
    let mut sessions = read_json_file(&path, json!([]));
    let mut items = sessions.as_array().cloned().unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(existing) = items
        .iter_mut()
        .find(|item| item.get("session_id").and_then(Value::as_str) == Some(session_id))
    {
        if let Some(object) = existing.as_object_mut() {
            object.insert("status".to_string(), json!("checkpoint_requested"));
            object.insert("updated_at".to_string(), json!(now));
        }
    } else {
        items.push(json!({
            "session_id": session_id,
            "status": "checkpoint_requested",
            "runtime": "daemon",
            "created_at": now,
            "updated_at": now,
        }));
    }
    sessions = json!(items);
    write_json_file(&path, &sessions)?;
    append_agent_event_locked(
        state,
        &agent_dir,
        "checkpoint.requested",
        "Checkpoint requested for session.",
        json!({"session_id": session_id, "runtime": "daemon"}),
    )?;
    Ok(json!({"session_id": session_id, "status": "checkpoint_requested"}))
}

#[derive(Deserialize)]
pub(crate) struct ContextRequest {
    task_id: String,
    call_id: String,
    objective: String,
    phase: Option<String>,
    role: Option<String>,
    runtime: Option<String>,
    workspace: Option<String>,
    allowed_paths: Option<Vec<String>>,
    acceptance_criteria: Option<Vec<String>>,
    persist: Option<bool>,
}

pub(crate) fn create_context(state: &Arc<AppState>, req: ContextRequest) -> Result<Value, String> {
    if req.task_id.trim().is_empty()
        || req.call_id.trim().is_empty()
        || req.objective.trim().is_empty()
    {
        return Err("task_id, call_id, and objective are required".to_string());
    }
    let packet = json!({
        "task_id": req.task_id,
        "call_id": req.call_id,
        "phase": req.phase.unwrap_or_else(|| "execute".to_string()),
        "role": req.role.unwrap_or_else(|| "executor".to_string()),
        "runtime": req.runtime.unwrap_or_else(|| "acp".to_string()),
        "workspace": req.workspace,
        "objective": req.objective,
        "allowed_paths": req.allowed_paths.unwrap_or_default(),
        "acceptance_criteria": req.acceptance_criteria.unwrap_or_default(),
    });
    if req.persist.unwrap_or(true) {
        let call_dir = state
            .workspace
            .join(".agentcall")
            .join("tasks")
            .join(packet["task_id"].as_str().unwrap_or("task"))
            .join("calls")
            .join(packet["call_id"].as_str().unwrap_or("call"));
        std::fs::create_dir_all(&call_dir).map_err(|err| err.to_string())?;
        write_json_file(&call_dir.join("context.json"), &packet)?;
        write_json_file(
            &call_dir.join("input.json"),
            &json!({"context_packet": packet}),
        )?;
        std::fs::write(
            call_dir.join("prompt.md"),
            format!(
                "# Context Packet\n\n```json\n{}\n```\n",
                serde_json::to_string_pretty(&packet).map_err(|err| err.to_string())?
            ),
        )
        .map_err(|err| err.to_string())?;
    }
    crate::state::append_agent_event(
        state,
        "context.created",
        "Context packet created.",
        json!({"task_id": packet["task_id"], "call_id": packet["call_id"], "runtime": "daemon"}),
    );
    Ok(packet)
}

#[derive(Deserialize)]
pub(crate) struct TranscriptIndexRequest {
    path: String,
    session_id: Option<String>,
}

pub(crate) fn index_transcript(
    state: &Arc<AppState>,
    req: TranscriptIndexRequest,
) -> Result<Value, String> {
    let path = std::path::PathBuf::from(&req.path);
    let text = std::fs::read_to_string(&path).map_err(|err| err.to_string())?;
    let mut messages = 0u64;
    let mut tool_uses = 0u64;
    let mut tool_results = 0u64;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        messages += 1;
        count_tools(&value, &mut tool_uses, &mut tool_results);
    }
    let session_id = req.session_id.unwrap_or_else(|| {
        path.file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("transcript")
            .to_string()
    });
    let summary = json!({
        "session_id": session_id,
        "transcript_path": path,
        "messages": messages,
        "tool_uses": tool_uses,
        "tool_results": tool_results,
    });
    let agent_dir = state.workspace.join(".agentcall");
    let _guard = state.state_writer.lock().unwrap();
    let state_path = agent_dir.join("state").join("transcripts.json");
    let mut transcripts = read_json_file(&state_path, json!({}));
    if let Some(object) = transcripts.as_object_mut() {
        object.insert(
            summary["session_id"]
                .as_str()
                .unwrap_or("transcript")
                .to_string(),
            summary.clone(),
        );
    }
    write_json_file(&state_path, &transcripts)?;
    append_agent_event_locked(
        state,
        &agent_dir,
        "transcript.indexed",
        "Transcript indexed.",
        json!({"session_id": summary["session_id"], "transcript_path": summary["transcript_path"], "runtime": "daemon"}),
    )?;
    Ok(summary)
}

struct RouteDecision {
    runtime: String,
    reason: String,
    score_breakdown: Value,
}

fn route_decision(req: &RouteRequest) -> RouteDecision {
    let requested = req.runtime.as_deref().unwrap_or("auto");
    if requested == "pty" || requested == "acp" {
        return RouteDecision {
            runtime: requested.to_string(),
            reason: format!("runtime forced by caller: {requested}"),
            score_breakdown: json!({"forced": requested}),
        };
    }
    let estimated_minutes = req.estimated_minutes.unwrap_or(0);
    let estimated_files = req.estimated_files.unwrap_or(0);
    let estimated_loc = req.estimated_loc.unwrap_or(0);
    let needs_continuity = req.needs_continuity.unwrap_or(false);
    let risk = req.risk.as_deref().unwrap_or("medium");
    let mut pty_score = 0i64;
    let mut acp_score = 1i64;
    if estimated_minutes >= 20 {
        pty_score += 2;
    } else {
        acp_score += 1;
    }
    if estimated_files >= 4 || estimated_loc >= 200 {
        pty_score += 2;
    } else {
        acp_score += 1;
    }
    if needs_continuity {
        pty_score += 3;
    }
    if risk == "high" {
        pty_score += 2;
    } else if risk == "low" {
        acp_score += 1;
    }
    let runtime = if pty_score > acp_score { "pty" } else { "acp" };
    RouteDecision {
        runtime: runtime.to_string(),
        reason: if runtime == "pty" {
            "task appears long, broad, risky, or continuity-heavy; use visible handoff".to_string()
        } else {
            "task appears bounded and low-continuity; use ACP agents-as-tools path".to_string()
        },
        score_breakdown: json!({
            "pty": pty_score,
            "acp": acp_score,
            "estimated_minutes": estimated_minutes,
            "estimated_files": estimated_files,
            "estimated_loc": estimated_loc,
            "needs_continuity": needs_continuity,
            "risk": risk,
        }),
    }
}

fn start_pty_route(
    state: &Arc<AppState>,
    req: &RouteRequest,
    record: &mut RouteRecord,
) -> Result<(), String> {
    let session_name = req
        .session_name
        .clone()
        .unwrap_or_else(|| record.route_id.replace("route-", "route-pty-"));
    if !safe_name(&session_name) {
        return Err("unsafe session_name".to_string());
    }
    let command = req.command.clone().unwrap_or_else(|| {
        vec![
            "claude".to_string(),
            "--permission-mode".to_string(),
            "auto".to_string(),
        ]
    });
    let info = start_session(
        state,
        StartRequest {
            name: session_name.clone(),
            command,
            cwd: req.workspace.clone(),
            cols: Some(100),
            rows: Some(40),
        },
    )?;
    record.status = "started".to_string();
    record.session_name = Some(session_name);
    record.result = json!({
        "runtime": "pty",
        "session": info,
        "binding_gate": {
            "required": true,
            "expected_binding_source": "env",
            "status": "pending_hook"
        }
    });
    Ok(())
}

fn start_acp_route(
    state: &Arc<AppState>,
    req: &RouteRequest,
    record: &mut RouteRecord,
) -> Result<(), String> {
    let invocation_id = record.route_id.replace("route-", "acp-");
    record.invocation_id = Some(invocation_id.clone());
    let command = req
        .adapter_command
        .clone()
        .or_else(adapter_command_from_env);
    let timeout_seconds = req.timeout_seconds.unwrap_or(30).clamp(1, 300);
    if let Some(command) = command {
        let result = run_bounded_adapter(&command, &req.objective, timeout_seconds)?;
        record.status = result["status"].as_str().unwrap_or("completed").to_string();
        record.result = json!({
            "runtime": "acp",
            "invocation_id": invocation_id,
            "adapter": "daemon_owned_transitional",
            "timeout_seconds": timeout_seconds,
            "adapter_result": result,
        });
    } else {
        record.status = "adapter_not_configured".to_string();
        record.result = json!({
            "runtime": "acp",
            "invocation_id": invocation_id,
            "adapter": "not_configured",
            "message": "Set AGENTCALL_ACP_ADAPTER_COMMAND or pass adapter_command for v0.8a transitional ACP execution.",
        });
    }
    crate::state::append_agent_event(
        state,
        "route.acp_invocation",
        "ACP route invocation recorded by daemon.",
        json!({"route_id": record.route_id, "invocation_id": invocation_id, "status": record.status}),
    );
    Ok(())
}

fn upsert_route_record(state: &Arc<AppState>, record: &RouteRecord) -> Result<(), String> {
    let agent_dir = state.workspace.join(".agentcall");
    let _guard = state.state_writer.lock().unwrap();
    let path = agent_dir.join("state").join("routes.json");
    let mut routes = read_json_file(&path, json!({}));
    if let Some(object) = routes.as_object_mut() {
        object.insert(
            record.route_id.clone(),
            serde_json::to_value(record).map_err(|err| err.to_string())?,
        );
    }
    write_json_file(&path, &routes)?;
    append_agent_event_locked(
        state,
        &agent_dir,
        "route.recorded",
        "Route recorded by daemon.",
        json!({"route_id": record.route_id, "runtime": record.recommended_runtime, "status": record.status}),
    )
}

fn read_routes(state: &AppState) -> Value {
    read_json_file(
        &state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("routes.json"),
        json!({}),
    )
}

fn has_auto_estimates(req: &RouteRequest) -> bool {
    req.estimated_minutes.is_some()
        && (req.estimated_files.is_some() || req.estimated_loc.is_some())
}

fn route_has_context_fields(req: &RouteRequest) -> bool {
    req.task_id.is_some()
        || req.call_id.is_some()
        || req.phase.is_some()
        || req.role.is_some()
        || req.allowed_paths.is_some()
        || req.acceptance_criteria.is_some()
        || req.persist_context.is_some()
}

fn merge_result_field(result: &mut Value, key: &str, value: Value) {
    if !result.is_object() {
        *result = json!({});
    }
    if let Some(object) = result.as_object_mut() {
        object.insert(key.to_string(), value);
    }
}

fn adapter_command_from_env() -> Option<Vec<String>> {
    let value = std::env::var("AGENTCALL_ACP_ADAPTER_COMMAND").ok()?;
    let parts: Vec<String> = value.split_whitespace().map(str::to_string).collect();
    if parts.is_empty() { None } else { Some(parts) }
}

fn run_bounded_adapter(
    command: &[String],
    objective: &str,
    timeout_seconds: u64,
) -> Result<Value, String> {
    if command.is_empty() {
        return Err("adapter_command cannot be empty".to_string());
    }
    let mut child = Command::new(&command[0])
        .args(&command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to start ACP adapter: {err}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let payload =
            json!({"objective": objective, "runtime": "acp", "source": "agentcall-daemon"});
        let _ = writeln!(
            stdin,
            "{}",
            serde_json::to_string(&payload).unwrap_or_default()
        );
    }
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_thread = thread::spawn(move || read_pipe(stdout));
    let err_thread = thread::spawn(move || read_pipe(stderr));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
            break json!({"kind": "exited", "code": status.code(), "success": status.success()});
        }
        if started.elapsed() > Duration::from_secs(timeout_seconds) {
            let _ = child.kill();
            let _ = child.wait();
            break json!({"kind": "timeout", "success": false});
        }
        thread::sleep(Duration::from_millis(50));
    };
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    Ok(json!({
        "status": if status["success"].as_bool().unwrap_or(false) { "completed" } else { "failed" },
        "process": status,
        "stdout": stdout,
        "stderr": stderr,
    }))
}

fn read_pipe(pipe: Option<impl Read>) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut bytes = Vec::new();
    let _ = pipe.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).to_string()
}

fn count_tools(value: &Value, tool_uses: &mut u64, tool_results: &mut u64) {
    match value {
        Value::Object(object) => {
            if object.get("type").and_then(Value::as_str) == Some("tool_use") {
                *tool_uses += 1;
            }
            if object.get("type").and_then(Value::as_str) == Some("tool_result") {
                *tool_results += 1;
            }
            for value in object.values() {
                count_tools(value, tool_uses, tool_results);
            }
        }
        Value::Array(items) => {
            for value in items {
                count_tools(value, tool_uses, tool_results);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn auto_route_requires_estimates() {
        let state = Arc::new(AppState::new(test_workspace("auto-missing")));
        let result = handle_route(
            &state,
            RouteRequest {
                objective: "review a focused diff".to_string(),
                workspace: None,
                mode: Some("recommend".to_string()),
                runtime: Some("auto".to_string()),
                estimated_minutes: None,
                estimated_files: Some(1),
                estimated_loc: None,
                needs_continuity: None,
                risk: None,
                session_name: None,
                command: None,
                adapter_command: None,
                timeout_seconds: None,
                task_id: None,
                call_id: None,
                phase: None,
                role: None,
                allowed_paths: None,
                acceptance_criteria: None,
                persist_context: None,
            },
        );
        assert!(result.unwrap_err().contains("runtime=auto requires"));
    }

    #[test]
    fn forced_acp_route_is_daemon_recorded_without_python_adapter() {
        let workspace = test_workspace("forced-acp");
        let state = Arc::new(AppState::new(workspace.clone()));
        let route = handle_route(
            &state,
            RouteRequest {
                objective: "bounded review".to_string(),
                workspace: None,
                mode: Some("start".to_string()),
                runtime: Some("acp".to_string()),
                estimated_minutes: None,
                estimated_files: None,
                estimated_loc: None,
                needs_continuity: None,
                risk: None,
                session_name: None,
                command: None,
                adapter_command: None,
                timeout_seconds: None,
                task_id: None,
                call_id: None,
                phase: None,
                role: None,
                allowed_paths: None,
                acceptance_criteria: None,
                persist_context: None,
            },
        )
        .unwrap();
        assert_eq!(route["recommended_runtime"], "acp");
        assert_eq!(route["status"], "adapter_not_configured");
        assert_eq!(route["result"]["adapter"], "not_configured");
        assert!(workspace.join(".agentcall/state/routes.json").exists());
    }

    #[test]
    fn route_can_create_context_packet_without_separate_mcp_tool() {
        let workspace = test_workspace("route-context");
        let state = Arc::new(AppState::new(workspace.clone()));
        let route = handle_route(
            &state,
            RouteRequest {
                objective: "audit route context".to_string(),
                workspace: Some("E:/GameProject/GGMYS".to_string()),
                mode: Some("start".to_string()),
                runtime: Some("acp".to_string()),
                estimated_minutes: None,
                estimated_files: None,
                estimated_loc: None,
                needs_continuity: None,
                risk: None,
                session_name: None,
                command: None,
                adapter_command: None,
                timeout_seconds: None,
                task_id: Some("task-route".to_string()),
                call_id: Some("call-a".to_string()),
                phase: Some("execute".to_string()),
                role: Some("reviewer".to_string()),
                allowed_paths: Some(vec!["src".to_string()]),
                acceptance_criteria: Some(vec!["report risks".to_string()]),
                persist_context: Some(true),
            },
        )
        .unwrap();
        let packet = &route["result"]["context_packet"];
        assert_eq!(packet["task_id"], "task-route");
        assert_eq!(packet["call_id"], "call-a");
        assert_eq!(packet["runtime"], "acp");
        assert_eq!(packet["workspace"], "E:/GameProject/GGMYS");
        assert!(
            workspace
                .join(".agentcall/tasks/task-route/calls/call-a/context.json")
                .exists()
        );
    }

    #[test]
    fn transcript_index_counts_nested_tool_items() {
        let workspace = test_workspace("transcript");
        let transcript = workspace.join("transcript.jsonl");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            &transcript,
            [
                r#"{"role":"user","content":"go"}"#,
                r#"{"role":"assistant","content":[{"type":"tool_use"},{"type":"tool_result"}]}"#,
            ]
            .join("\n"),
        )
        .unwrap();
        let state = Arc::new(AppState::new(workspace));
        let summary = index_transcript(
            &state,
            TranscriptIndexRequest {
                path: transcript.display().to_string(),
                session_id: Some("sess".to_string()),
            },
        )
        .unwrap();
        assert_eq!(summary["messages"], 2);
        assert_eq!(summary["tool_uses"], 1);
        assert_eq!(summary["tool_results"], 1);
    }

    fn test_workspace(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("agentcall-routes-{name}-{nonce}"))
    }
}
