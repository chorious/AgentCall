use crate::actor::submit_session_command;
use crate::commands::build_session_send_command;
use crate::ownership::{acquire_workspace_lease, release_workspace_lease};
use crate::runtime::{AgentRuntime, StartSpec};
use crate::runtime_pty::ClaudeCodePtyRuntime;
use crate::runtime_sdk::{ClaudeCodeSdkRuntime, sdk_runtime_enabled};
use crate::scheduler::enforce_start_capacity;
use crate::session::is_claude_command;
use crate::state::{AppState, append_agent_event_locked, read_json_file, write_json_file};
use crate::util::{now_ms, safe_name};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
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
    task_id: Option<String>,
    call_id: Option<String>,
    phase: Option<String>,
    role: Option<String>,
    allowed_paths: Option<Vec<String>>,
    acceptance_criteria: Option<Vec<String>>,
    persist_context: Option<bool>,
    report_path: Option<String>,
    read_only: Option<bool>,
    pty_workflow: Option<String>,
    initial_permission_mode: Option<String>,
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
    suggested_wait_seconds: u64,
    do_not_retry_before_seconds: u64,
    slow_worker_policy: String,
    session_name: Option<String>,
    created_at: u64,
    updated_at: u64,
    result: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PtyWorkflow {
    PlanThenAuto,
    Normal,
}

impl PtyWorkflow {
    pub(crate) fn from_request(value: Option<&str>) -> Result<Self, String> {
        match value.unwrap_or("normal") {
            "plan_then_auto" => Ok(Self::PlanThenAuto),
            "normal" => Ok(Self::Normal),
            other => Err(format!(
                "pty_workflow must be plan_then_auto or normal, got {other}"
            )),
        }
    }

    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::PlanThenAuto => "plan_then_auto",
            Self::Normal => "normal",
        }
    }
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
    if !matches!(runtime, "auto" | "pty" | "sdk") {
        return Err("runtime must be auto, pty, or sdk".to_string());
    }
    if runtime == "sdk" && !sdk_runtime_enabled(state) {
        return Err(
            "sdk runtime is experimental and disabled; set experimental_sdk_runtime=true in local config"
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
            "wait_then_inspect_session".to_string()
        } else {
            "call agentcall_route with mode=start to launch a PTY utility worker".to_string()
        },
        suggested_wait_seconds: 45,
        do_not_retry_before_seconds: 60,
        slow_worker_policy: "Claude Code PTY is an asynchronous worker. Wait for prompt_gate, hooks, or session summary progress before retrying; do not treat quiet reading/thinking as failure unless attention_status or prompt_gate reports a problem.".to_string(),
        session_name: None,
        created_at: now_ms(),
        updated_at: now_ms(),
        result: json!({}),
    };

    if mode == "start" {
        match decision.runtime.as_str() {
            "pty" => start_pty_route(state, &req, &mut record)?,
            "sdk" => start_sdk_route(state, &req, &mut record)?,
            other => return Err(format!("unsupported route runtime: {other}")),
        }
    }
    if route_has_context_fields(&req) {
        let default_call_id = record
            .session_name
            .clone()
            .or_else(|| record.invocation_id.clone())
            .unwrap_or_else(|| record.route_id.clone());
        let context_packet = create_context(
            state,
            ContextRequest {
                task_id: req
                    .task_id
                    .clone()
                    .unwrap_or_else(|| record.route_id.clone()),
                call_id: req.call_id.clone().unwrap_or(default_call_id),
                objective: req.objective.clone(),
                phase: req.phase.clone(),
                role: req.role.clone(),
                runtime: Some(decision.runtime.clone()),
                workspace: req.workspace.clone(),
                allowed_paths: req.allowed_paths.clone(),
                acceptance_criteria: req.acceptance_criteria.clone(),
                persist: req.persist_context,
                report_path: req.report_path.clone(),
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
    report_path: Option<String>,
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
        "runtime": req.runtime.unwrap_or_else(|| "pty".to_string()),
        "workspace": req.workspace,
        "objective": req.objective,
        "allowed_paths": req.allowed_paths.unwrap_or_default(),
        "acceptance_criteria": req.acceptance_criteria.unwrap_or_default(),
        "report_path": req.report_path,
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
    if requested == "sdk" {
        return RouteDecision {
            runtime: "sdk".to_string(),
            reason: "runtime forced by caller: experimental SDK runtime".to_string(),
            score_breakdown: json!({
                "decision_model": "explicit_experimental_runtime",
                "requested_runtime": requested,
                "recommended_runtime": "sdk",
                "experimental": true,
            }),
        };
    }
    if requested == "pty" || requested == "auto" {
        return RouteDecision {
            runtime: "pty".to_string(),
            reason: if requested == "pty" {
                "runtime forced by caller: pty utility worker".to_string()
            } else {
                "AgentCall v3.0 auto runtime starts a PTY utility worker".to_string()
            },
            score_breakdown: json!({
                "decision_model": "pty_only_v3",
                "requested_runtime": requested,
                "recommended_runtime": "pty",
                "worker_kind": "utility",
                "legacy_estimates_ignored": {
                    "estimated_minutes": req.estimated_minutes,
                    "estimated_files": req.estimated_files,
                    "estimated_loc": req.estimated_loc,
                    "needs_continuity": req.needs_continuity,
                    "risk": req.risk,
                }
            }),
        };
    }
    RouteDecision {
        runtime: "needs_contract".to_string(),
        reason: "unsupported route request".to_string(),
        score_breakdown: json!({"sop_status": "needs_contract"}),
    }
}

fn start_sdk_route(
    state: &Arc<AppState>,
    req: &RouteRequest,
    record: &mut RouteRecord,
) -> Result<(), String> {
    let runtime = ClaudeCodeSdkRuntime::new(Arc::clone(state));
    let result = runtime.start(StartSpec {
        name: req
            .session_name
            .clone()
            .unwrap_or_else(|| record.route_id.replace("route-", "route-sdk-")),
        command: req.command.clone().unwrap_or_default(),
        cwd: req.workspace.clone(),
        cols: None,
        rows: None,
    });
    record.status = "unsupported_experimental_runtime".to_string();
    record.required_next_step =
        "use runtime=pty until native SDK worker start is implemented".to_string();
    record.result = json!({
        "runtime": runtime.id(),
        "runtime_session": Value::Null,
        "capabilities": {
            "runtime_id": runtime.capabilities().runtime_id,
            "supports_pty": runtime.capabilities().supports_pty,
            "supports_sdk": runtime.capabilities().supports_sdk,
            "command_path": runtime.capabilities().command_path,
        },
        "error": result.err().unwrap_or_else(|| "sdk runtime did not start".to_string()),
    });
    Ok(())
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
    let workflow = PtyWorkflow::from_request(req.pty_workflow.as_deref())?;
    let claude_session_id = if workflow == PtyWorkflow::PlanThenAuto {
        Some(new_claude_session_id(state))
    } else {
        None
    };
    let command = pty_command(req, &workflow, claude_session_id.as_deref())?;
    let containment = pty_containment(state, req, &session_name);
    let target_workspace = route_target_workspace(state, req);
    let schedule = enforce_start_capacity(state, "codex")?;
    let workspace_lease = acquire_workspace_lease(
        state,
        &session_name,
        &target_workspace,
        req.read_only.unwrap_or(false),
    )?;
    let runtime = ClaudeCodePtyRuntime::new(Arc::clone(state));
    let runtime_session = runtime
        .start(StartSpec {
            name: session_name.clone(),
            command,
            cwd: req.workspace.clone(),
            cols: Some(100),
            rows: Some(40),
        })
        .inspect_err(|_| {
            let _ = release_workspace_lease(state, &session_name, "route_start_failed");
        })?;
    let prompt_gate = if is_claude_command(&runtime_session.info.command) {
        submit_pty_prompt_with_ack(state, &session_name, pty_prompt(req, &containment))
    } else {
        submit_pty_prompt_without_hook_ack(state, &session_name, pty_prompt(req, &containment))
    };
    let prompt_status = prompt_gate
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("started_pending_prompt_ack")
        .to_string();
    record.status = prompt_status.to_string();
    record.session_name = Some(session_name.clone());
    let permission_mode = pty_initial_permission_mode(req, &workflow);
    record.result = json!({
        "runtime": "pty",
        "worker_kind": "utility",
        "pty_workflow": workflow.as_str(),
        "workflow_status": if workflow == PtyWorkflow::PlanThenAuto { "plan_running" } else { "running" },
        "phase": if workflow == PtyWorkflow::PlanThenAuto { "plan" } else { "execute" },
        "permission_mode": permission_mode,
        "mode_source": "route",
        "claude_session_id": claude_session_id,
        "plan_session_name": if workflow == PtyWorkflow::PlanThenAuto { Some(session_name.clone()) } else { None },
        "auto_session_name": serde_json::Value::Null,
        "session": runtime_session.info,
        "runtime_session": {
            "session_id": runtime_session.session_id,
            "runtime": runtime_session.runtime,
            "command_path": runtime.capabilities().command_path,
        },
        "scheduler": schedule.to_value(),
        "workspace_lease": workspace_lease,
        "prompt_gate": prompt_gate,
        "binding_gate": {
            "required": true,
            "expected_binding_source": "env",
            "status": "pending_hook"
        },
        "patience_policy": {
            "suggested_wait_seconds": 60,
            "do_not_retry_before_seconds": 60,
            "stall_threshold_seconds": 300,
            "hint": "Claude Code PTY may spend time reading files, thinking, or preparing tool calls. Inspect session summary/attention before retrying or restarting."
        },
        "containment": containment
    });
    Ok(())
}

fn route_target_workspace(state: &AppState, req: &RouteRequest) -> std::path::PathBuf {
    req.workspace
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| state.workspace.clone())
}

fn pty_containment(state: &AppState, req: &RouteRequest, session_name: &str) -> Value {
    let read_only = req.read_only.unwrap_or(false);
    let explicit_allowed = req.allowed_paths.clone().unwrap_or_default();
    let scratch_path = format!(".agentcall/workspaces/{session_name}");
    let mut writable_paths = vec![];
    if !read_only {
        writable_paths.push(scratch_path.clone());
        if let Some(report_path) = req
            .report_path
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        {
            writable_paths.push(report_path.clone());
        }
        for path in &explicit_allowed {
            if !path.trim().is_empty() && !writable_paths.iter().any(|item| item == path) {
                writable_paths.push(path.clone());
            }
        }
        let scratch_abs = state.workspace.join(&scratch_path);
        let _ = std::fs::create_dir_all(&scratch_abs);
        let _ = std::fs::write(
            scratch_abs.join("README.md"),
            format!(
                "# AgentCall Session Scratch\n\nSession: `{session_name}`\n\nThis directory is writable for bounded helper artifacts, temporary scripts, and report material. Do not write outside route containment.\n"
            ),
        );
    }
    json!({
        "mode": if read_only { "read_only" } else { "enforced_readonly_bash" },
        "read_only": read_only,
        "allowed_paths": explicit_allowed,
        "writable_paths": writable_paths,
        "scratch_path": if read_only { Value::Null } else { json!(scratch_path) },
        "bash_write_policy": "readonly_only"
    })
}

pub(crate) fn route_for_wrapper_session(
    state: &AppState,
    wrapper_session: &str,
) -> Option<(String, Value)> {
    let routes = read_routes(state);
    let object = routes.as_object()?;
    object.iter().find_map(|(route_id, route)| {
        let session_match =
            route.get("session_name").and_then(Value::as_str) == Some(wrapper_session);
        let plan_match = route
            .get("result")
            .and_then(|result| result.get("plan_session_name"))
            .and_then(Value::as_str)
            == Some(wrapper_session);
        let auto_match = route
            .get("result")
            .and_then(|result| result.get("auto_session_name"))
            .and_then(Value::as_str)
            == Some(wrapper_session);
        let binding_match = route
            .get("result")
            .and_then(|result| result.get("binding_gate"))
            .and_then(|gate| gate.get("wrapper_session"))
            .and_then(Value::as_str)
            == Some(wrapper_session);
        if session_match || plan_match || auto_match || binding_match {
            Some((route_id.clone(), route.clone()))
        } else {
            None
        }
    })
}

pub(crate) fn patch_route_record(
    state: &AppState,
    route_id: &str,
    patch: Value,
) -> Result<(), String> {
    let agent_dir = state.workspace.join(".agentcall");
    let _guard = state.state_writer.lock().unwrap();
    patch_route_record_locked(state, &agent_dir, route_id, patch)
}

pub(crate) fn patch_route_record_locked(
    state: &AppState,
    agent_dir: &std::path::Path,
    route_id: &str,
    patch: Value,
) -> Result<(), String> {
    let path = agent_dir.join("state").join("routes.json");
    let mut routes = read_json_file(&path, json!({}));
    if let Some(route) = routes.get_mut(route_id) {
        deep_merge(route, &patch);
    }
    write_json_file(&path, &routes)?;
    append_agent_event_locked(
        state,
        &agent_dir,
        "route.updated",
        "Route updated by daemon.",
        json!({"route_id": route_id}),
    )
}

fn deep_merge(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(target), Value::Object(patch)) => {
            for (key, value) in patch {
                if let Some(existing) = target.get_mut(key) {
                    deep_merge(existing, value);
                } else {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        (target, patch) => {
            *target = patch.clone();
        }
    }
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

#[allow(dead_code)]
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
        || req.report_path.is_some()
        || req.read_only.is_some()
}

fn merge_result_field(result: &mut Value, key: &str, value: Value) {
    if !result.is_object() {
        *result = json!({});
    }
    if let Some(object) = result.as_object_mut() {
        object.insert(key.to_string(), value);
    }
}

fn wait_for_user_prompt_submit(
    state: &Arc<AppState>,
    wrapper_session: &str,
    wait: Duration,
) -> bool {
    let started = Instant::now();
    while started.elapsed() < wait {
        let bindings = read_json_file(
            &state
                .workspace
                .join(".agentcall")
                .join("state")
                .join("runtime_binding.json"),
            json!({}),
        );
        if bindings
            .get(wrapper_session)
            .and_then(|binding| binding.get("last_hook_event"))
            .and_then(Value::as_str)
            == Some("UserPromptSubmit")
        {
            return true;
        }
        thread::sleep(Duration::from_millis(250));
    }
    false
}

fn submit_pty_prompt_with_ack(
    state: &Arc<AppState>,
    wrapper_session: &str,
    prompt: String,
) -> Value {
    let mut last_error = None;
    for attempt in 1..=2u64 {
        let args = json!({
            "text": prompt.clone(),
            "enter": true,
            "idempotency_key": format!("route-prompt-{wrapper_session}-{attempt}"),
            "owner_id": "codex"
        });
        let command = build_session_send_command(
            wrapper_session,
            "send",
            args.get("idempotency_key")
                .and_then(Value::as_str)
                .unwrap_or("route-prompt"),
            &args,
        );
        match submit_session_command(state, wrapper_session, command) {
            Ok(_) => {
                if wait_for_user_prompt_submit(state, wrapper_session, Duration::from_secs(8)) {
                    return json!({
                        "status": "started_and_prompt_submitted",
                        "expected_hook": "UserPromptSubmit",
                        "attempts": attempt,
                        "acknowledged": true
                    });
                }
            }
            Err(err) => last_error = Some(err),
        }
    }
    json!({
        "status": "started_pending_prompt_ack",
        "expected_hook": "UserPromptSubmit",
        "attempts": 2,
        "acknowledged": false,
        "last_error": last_error
    })
}

fn submit_pty_prompt_without_hook_ack(
    state: &Arc<AppState>,
    wrapper_session: &str,
    prompt: String,
) -> Value {
    let args = json!({
        "text": prompt,
        "enter": true,
        "idempotency_key": format!("route-prompt-{wrapper_session}-custom-worker"),
        "owner_id": "codex"
    });
    let command = build_session_send_command(
        wrapper_session,
        "send",
        args.get("idempotency_key")
            .and_then(Value::as_str)
            .unwrap_or("route-prompt"),
        &args,
    );
    match submit_session_command(state, wrapper_session, command) {
        Ok(result) => json!({
            "status": "started_prompt_dispatched_without_hook_ack",
            "expected_hook": Value::Null,
            "acknowledged": false,
            "actor_result": result,
            "reason": "custom PTY command is not expected to emit Claude Code UserPromptSubmit hooks"
        }),
        Err(err) => json!({
            "status": "started_pending_prompt_dispatch",
            "expected_hook": Value::Null,
            "acknowledged": false,
            "last_error": err
        }),
    }
}

fn pty_initial_permission_mode(req: &RouteRequest, workflow: &PtyWorkflow) -> String {
    match workflow {
        PtyWorkflow::PlanThenAuto => "plan".to_string(),
        PtyWorkflow::Normal => req
            .initial_permission_mode
            .clone()
            .unwrap_or_else(|| "auto".to_string()),
    }
}

fn pty_command(
    req: &RouteRequest,
    workflow: &PtyWorkflow,
    claude_session_id: Option<&str>,
) -> Result<Vec<String>, String> {
    if let Some(command) = &req.command {
        return Ok(command.clone());
    }
    let permission_mode = pty_initial_permission_mode(req, workflow);
    if !matches!(permission_mode.as_str(), "plan" | "auto" | "default") {
        return Err("initial_permission_mode must be plan, auto, or default".to_string());
    }
    let mut command = vec!["claude".to_string()];
    if permission_mode != "default" {
        command.push("--permission-mode".to_string());
        command.push(permission_mode);
    }
    if let Some(session_id) = claude_session_id {
        command.push("--session-id".to_string());
        command.push(session_id.to_string());
    }
    Ok(command)
}

fn new_claude_session_id(state: &AppState) -> String {
    let seq = state.next_seq();
    let now = now_ms();
    let pid = std::process::id() as u64;
    format!(
        "{:08x}-{:04x}-4{:03x}-8{:03x}-{:012x}",
        (now & 0xffff_ffff) as u32,
        ((now >> 32) & 0xffff) as u16,
        (seq & 0x0fff) as u16,
        ((seq >> 12) & 0x0fff) as u16,
        ((now << 20) ^ (seq << 8) ^ pid) & 0x0000_ffff_ffff_ffff
    )
}

fn pty_prompt(req: &RouteRequest, containment: &Value) -> String {
    let criteria = req
        .acceptance_criteria
        .as_ref()
        .map(|items| items.join("\n- "))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Complete the requested task and write a report.".to_string());
    let allowed = pty_prompt_containment(containment);
    let workflow =
        PtyWorkflow::from_request(req.pty_workflow.as_deref()).unwrap_or(PtyWorkflow::Normal);
    match workflow {
        PtyWorkflow::PlanThenAuto => format!(
            "AgentCall PTY handoff. Start in PLAN MODE.\n\nObjective:\n{}\n\nAllowed paths / ownership:\n- {}\n\nAcceptance criteria:\n- {}\n\nPlan-phase rules:\n- Inspect the code and write a concrete plan only.\n- If anything important is unclear, ask concise clarification questions instead of guessing.\n- Do not modify project files during plan phase.\n- When the plan is ready, use ExitPlanMode and wait for approval. After approval, continue in auto mode and write the requested report.\n",
            req.objective, allowed, criteria
        ),
        PtyWorkflow::Normal => format!(
            "AgentCall utility PTY worker.\n\nObjective:\n{}\n\nAllowed paths / ownership:\n- {}\n\nAcceptance criteria:\n- {}\n\nRules:\n- Work in auto mode.\n- If key context is unclear, ask a concise question in this PTY.\n- Respect allowed_paths. Do not write outside them.\n- When finished, write the requested report or summarize exact changes, tests, risks, and remaining questions.\n- Stop at the prompt for supervisor review.\n",
            req.objective, allowed, criteria
        ),
    }
}

fn pty_prompt_containment(containment: &Value) -> String {
    let mut lines = vec![];
    if let Some(mode) = containment.get("mode").and_then(Value::as_str) {
        lines.push(format!("containment mode: {mode}"));
    }
    if let Some(paths) = containment.get("writable_paths").and_then(Value::as_array) {
        for path in paths.iter().filter_map(Value::as_str) {
            lines.push(format!("writable: {path}"));
        }
    }
    if let Some(paths) = containment.get("allowed_paths").and_then(Value::as_array) {
        for path in paths.iter().filter_map(Value::as_str) {
            lines.push(format!("owned path: {path}"));
        }
    }
    if let Some(scratch) = containment.get("scratch_path").and_then(Value::as_str) {
        lines.push(format!("session scratch: {scratch}"));
    }
    if let Some(policy) = containment.get("bash_write_policy").and_then(Value::as_str) {
        lines.push(format!(
            "Bash write policy: {policy}; use Write/Edit for scratch/report artifacts"
        ));
    }
    if lines.is_empty() {
        "Use the task workspace carefully.".to_string()
    } else {
        lines.join("\n- ")
    }
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
    use crate::config::LocalConfig;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn auto_route_recommends_pty_utility_worker() {
        let state = Arc::new(AppState::test(test_workspace("auto-missing")));
        let route = handle_route(
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
                task_id: None,
                call_id: None,
                phase: None,
                role: None,
                allowed_paths: None,
                acceptance_criteria: None,
                persist_context: None,
                report_path: None,
                read_only: None,
                pty_workflow: None,
                initial_permission_mode: None,
            },
        )
        .unwrap();
        assert_eq!(route["status"], "recommended");
        assert_eq!(route["recommended_runtime"], "pty");
        assert_eq!(route["score_breakdown"]["decision_model"], "pty_only_v3");
    }

    #[test]
    fn sdk_route_is_rejected_until_experimental_config_enabled() {
        let state = Arc::new(AppState::test(test_workspace("sdk-disabled")));
        let err = handle_route(
            &state,
            RouteRequest {
                objective: "try sdk".to_string(),
                workspace: None,
                mode: Some("recommend".to_string()),
                runtime: Some("sdk".to_string()),
                estimated_minutes: None,
                estimated_files: None,
                estimated_loc: None,
                needs_continuity: None,
                risk: None,
                session_name: None,
                command: None,
                task_id: None,
                call_id: None,
                phase: None,
                role: None,
                allowed_paths: None,
                acceptance_criteria: None,
                persist_context: None,
                report_path: None,
                read_only: None,
                pty_workflow: None,
                initial_permission_mode: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("experimental and disabled"));
    }

    #[test]
    fn enabled_sdk_route_returns_explicit_experimental_stub() {
        let workspace = test_workspace("sdk-enabled");
        let state = Arc::new(AppState::new(
            workspace.clone(),
            LocalConfig {
                claude_workspace: Some(workspace),
                experimental_sdk_runtime: Some(true),
                ..LocalConfig::default()
            },
            None,
        ));
        let route = handle_route(
            &state,
            RouteRequest {
                objective: "try sdk".to_string(),
                workspace: None,
                mode: Some("start".to_string()),
                runtime: Some("sdk".to_string()),
                estimated_minutes: None,
                estimated_files: None,
                estimated_loc: None,
                needs_continuity: None,
                risk: None,
                session_name: Some("sdk-a".to_string()),
                command: None,
                task_id: None,
                call_id: None,
                phase: None,
                role: None,
                allowed_paths: None,
                acceptance_criteria: None,
                persist_context: None,
                report_path: None,
                read_only: None,
                pty_workflow: None,
                initial_permission_mode: None,
            },
        )
        .unwrap();
        assert_eq!(route["status"], "unsupported_experimental_runtime");
        assert_eq!(route["recommended_runtime"], "sdk");
        assert_eq!(
            route["result"]["capabilities"]["command_path"],
            "EventEnvelopeProjectionContract"
        );
        assert!(
            route["result"]["error"]
                .as_str()
                .unwrap()
                .contains("experimental_stub")
        );
    }

    #[test]
    fn route_can_create_context_packet_without_separate_mcp_tool() {
        let workspace = test_workspace("route-context");
        let state = Arc::new(AppState::test(workspace.clone()));
        let route = handle_route(
            &state,
            RouteRequest {
                objective: "audit route context".to_string(),
                workspace: Some("E:/GameProject/GGMYS".to_string()),
                mode: Some("recommend".to_string()),
                runtime: Some("auto".to_string()),
                estimated_minutes: None,
                estimated_files: None,
                estimated_loc: None,
                needs_continuity: None,
                risk: None,
                session_name: None,
                command: None,
                task_id: Some("task-route".to_string()),
                call_id: Some("call-a".to_string()),
                phase: Some("execute".to_string()),
                role: Some("reviewer".to_string()),
                allowed_paths: Some(vec!["src".to_string()]),
                acceptance_criteria: Some(vec!["report risks".to_string()]),
                persist_context: Some(true),
                report_path: Some("src/report.md".to_string()),
                read_only: None,
                pty_workflow: None,
                initial_permission_mode: None,
            },
        )
        .unwrap();
        let packet = &route["result"]["context_packet"];
        assert_eq!(packet["task_id"], "task-route");
        assert_eq!(packet["call_id"], "call-a");
        assert_eq!(packet["runtime"], "pty");
        assert_eq!(packet["workspace"], "E:/GameProject/GGMYS");
        assert!(
            workspace
                .join(".agentcall/tasks/task-route/calls/call-a/context.json")
                .exists()
        );
    }

    #[test]
    fn plan_then_auto_forces_plan_even_when_caller_requests_auto() {
        let req = RouteRequest {
            objective: "review only".to_string(),
            workspace: None,
            mode: Some("start".to_string()),
            runtime: Some("pty".to_string()),
            estimated_minutes: None,
            estimated_files: None,
            estimated_loc: None,
            needs_continuity: None,
            risk: None,
            session_name: None,
            command: None,
            task_id: None,
            call_id: None,
            phase: None,
            role: None,
            allowed_paths: None,
            acceptance_criteria: None,
            persist_context: None,
            report_path: None,
            read_only: None,
            pty_workflow: None,
            initial_permission_mode: Some("auto".to_string()),
        };
        let command = pty_command(
            &req,
            &PtyWorkflow::PlanThenAuto,
            Some("11111111-1111-4111-8111-111111111111"),
        )
        .unwrap();
        assert_eq!(
            command,
            vec![
                "claude",
                "--permission-mode",
                "plan",
                "--session-id",
                "11111111-1111-4111-8111-111111111111"
            ]
        );
    }

    #[test]
    fn generated_claude_session_id_has_uuid_shape() {
        let workspace = test_workspace("uuid-shape");
        let state = Arc::new(AppState::test(workspace));
        let session_id = new_claude_session_id(&state);
        let parts: Vec<&str> = session_id.split('-').collect();
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert_eq!(parts[2].len(), 4);
        assert_eq!(parts[3].len(), 4);
        assert_eq!(parts[4].len(), 12);
        assert!(
            parts
                .iter()
                .all(|part| part.chars().all(|ch| ch.is_ascii_hexdigit()))
        );
    }

    #[test]
    fn route_start_uses_runtime_boundary() {
        let source = include_str!("routes.rs");
        let direct_start_call = ["start", "_", "session", "("].concat();
        let direct_start_import = ["use crate::session::", "start", "_", "session"].concat();
        assert!(
            !source.contains(&direct_start_call),
            "routes.rs must not call session start directly"
        );
        assert!(
            !source.contains(&direct_start_import),
            "routes.rs must not import session start directly"
        );
        assert!(
            source.contains("ClaudeCodePtyRuntime"),
            "PTY route must cross the AgentRuntime boundary"
        );
    }

    #[test]
    fn pty_containment_defaults_to_writable_scratch_and_report() {
        let workspace = test_workspace("containment");
        let state = AppState::test(workspace.clone());
        let req = RouteRequest {
            objective: "write report".to_string(),
            workspace: None,
            mode: Some("start".to_string()),
            runtime: Some("pty".to_string()),
            estimated_minutes: None,
            estimated_files: None,
            estimated_loc: None,
            needs_continuity: None,
            risk: None,
            session_name: Some("containment-a".to_string()),
            command: None,
            task_id: None,
            call_id: None,
            phase: None,
            role: None,
            allowed_paths: Some(vec!["src".to_string()]),
            acceptance_criteria: None,
            persist_context: None,
            report_path: Some("docs/report.md".to_string()),
            read_only: None,
            pty_workflow: None,
            initial_permission_mode: None,
        };
        let containment = pty_containment(&state, &req, "containment-a");
        assert_eq!(containment["mode"], "enforced_readonly_bash");
        assert_eq!(containment["bash_write_policy"], "readonly_only");
        assert_eq!(
            containment["scratch_path"],
            ".agentcall/workspaces/containment-a"
        );
        let writable = containment["writable_paths"].as_array().unwrap();
        assert!(
            writable
                .iter()
                .any(|value| value == ".agentcall/workspaces/containment-a")
        );
        assert!(writable.iter().any(|value| value == "docs/report.md"));
        assert!(writable.iter().any(|value| value == "src"));
        assert!(
            workspace
                .join(".agentcall/workspaces/containment-a/README.md")
                .exists()
        );
    }

    #[test]
    fn pty_containment_read_only_has_no_writable_scratch() {
        let workspace = test_workspace("containment-readonly");
        let state = AppState::test(workspace);
        let req = RouteRequest {
            objective: "read only audit".to_string(),
            workspace: None,
            mode: Some("start".to_string()),
            runtime: Some("pty".to_string()),
            estimated_minutes: None,
            estimated_files: None,
            estimated_loc: None,
            needs_continuity: None,
            risk: None,
            session_name: Some("containment-ro".to_string()),
            command: None,
            task_id: None,
            call_id: None,
            phase: None,
            role: None,
            allowed_paths: Some(vec!["src".to_string()]),
            acceptance_criteria: None,
            persist_context: None,
            report_path: Some("docs/report.md".to_string()),
            read_only: Some(true),
            pty_workflow: None,
            initial_permission_mode: None,
        };
        let containment = pty_containment(&state, &req, "containment-ro");
        assert_eq!(containment["mode"], "read_only");
        assert!(containment["writable_paths"].as_array().unwrap().is_empty());
        assert!(containment["scratch_path"].is_null());
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
        let state = Arc::new(AppState::test(workspace));
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
