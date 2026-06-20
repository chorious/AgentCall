use crate::actor::submit_session_command;
use crate::commands::{PreparedCommand, prepare_session_send_command};
use crate::ownership::{
    install_reserved_route_leases, release_owner_lease, release_workspace_lease,
    reserve_route_leases,
};
use crate::prompt_gate::{
    DEFAULT_ACK_DEADLINE_MS, route_prompt_id, schedule_prompt_gate_auto_commit,
};
use crate::runtime::{AgentRuntime, StartSpec};
use crate::runtime_pty::ClaudeCodePtyRuntime;
use crate::runtime_sdk::{ClaudeCodeSdkRuntime, sdk_runtime_enabled};
use crate::scheduler::enforce_start_capacity;
use crate::session::{configured_claude_workspace, is_claude_command};
use crate::state::{AppState, append_agent_event_locked, read_json_file, write_json_file};
use crate::store::{RouteDecisionV1, SessionRecord};
use crate::util::{now_ms, safe_name};
use crate::workspace_audit;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
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
    reference_paths: Option<Vec<String>>,
    write_paths: Option<Vec<String>>,
    acceptance_criteria: Option<Vec<String>>,
    persist_context: Option<bool>,
    report_path: Option<String>,
    pty_workflow: Option<String>,
    initial_permission_mode: Option<String>,
}

#[derive(Clone, Serialize)]
struct RouteRecord {
    route_id: String,
    invocation_id: Option<String>,
    owner_id: String,
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
    handle_route_for_owner(state, req, "codex")
}

pub(crate) fn handle_route_for_owner(
    state: &Arc<AppState>,
    req: RouteRequest,
    owner_id: &str,
) -> Result<Value, String> {
    let mut req = req;
    if req.objective.trim().is_empty() {
        return Err("missing objective".to_string());
    }
    let mode = req.mode.clone().unwrap_or_else(|| "start".to_string());
    if !matches!(mode.as_str(), "recommend" | "start") {
        return Err("mode must be recommend or start".to_string());
    }
    let runtime = req.runtime.clone().unwrap_or_else(|| "auto".to_string());
    if !matches!(runtime.as_str(), "auto" | "pty" | "sdk") {
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
    let report_source = ensure_route_report_path(&mut req, &mode, &decision.runtime, &route_id);
    let report_warning = route_report_path_warning(state, &req, &report_source);
    let mut record = RouteRecord {
        route_id: route_id.clone(),
        invocation_id: None,
        owner_id: owner_id.to_string(),
        objective: req.objective.clone(),
        workspace: req.workspace.clone(),
        mode: mode.clone(),
        runtime: runtime.clone(),
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
            "pty" => start_pty_route(state, &req, &mut record, owner_id)?,
            "sdk" => start_sdk_route(state, &req, &mut record)?,
            other => return Err(format!("unsupported route runtime: {other}")),
        }
    }
    if let Some(report) = route_report_projection(state, &req, &report_source, report_warning) {
        merge_result_field(&mut record.result, "report", report);
        if let Some(report_path) = req.report_path.clone() {
            merge_result_field(&mut record.result, "report_path", json!(report_path));
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
                reference_paths: req.reference_paths.clone(),
                write_paths: req.write_paths.clone(),
                acceptance_criteria: req.acceptance_criteria.clone(),
                persist: req.persist_context,
                report_path: req.report_path.clone(),
            },
        )?;
        merge_result_field(&mut record.result, "context_packet", context_packet);
    }

    upsert_route_record(state, &record)?;
    if record.recommended_runtime == "pty" && mode == "start" {
        if let Some(session_name) = record.session_name.clone() {
            schedule_prompt_gate_auto_commit(Arc::clone(state), session_name);
        }
    }
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
    let mut sessions = read_json_file(&path, json!({}));
    if !sessions.is_object() {
        sessions = json!({});
    }
    let now = chrono::Utc::now().to_rfc3339();
    let object = sessions.as_object_mut().unwrap();
    match object.get_mut(session_id) {
        Some(existing) if existing.is_object() => {
            let object = existing.as_object_mut().unwrap();
            object.insert("status".to_string(), json!("checkpoint_requested"));
            object.insert("updated_at".to_string(), json!(now));
        }
        _ => {
            object.insert(
                session_id.to_string(),
                json!({
                    "session_id": session_id,
                    "status": "checkpoint_requested",
                    "runtime": "daemon",
                    "created_at": now,
                    "updated_at": now,
                }),
            );
        }
    }
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
    reference_paths: Option<Vec<String>>,
    write_paths: Option<Vec<String>>,
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
    if !safe_name(&req.task_id) || !safe_name(&req.call_id) {
        return Err("task_id and call_id must be safe path segments".to_string());
    }
    let packet = json!({
        "task_id": req.task_id,
        "call_id": req.call_id,
        "phase": req.phase.unwrap_or_else(|| "execute".to_string()),
        "role": req.role.unwrap_or_else(|| "executor".to_string()),
        "runtime": req.runtime.unwrap_or_else(|| "pty".to_string()),
        "workspace": req.workspace,
        "objective": req.objective,
        "write_paths": req.write_paths.unwrap_or_default(),
        "reference_paths": req.reference_paths.unwrap_or_default(),
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
    let path = canonical_transcript_path(state, &path)?;
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

fn canonical_transcript_path(
    state: &AppState,
    path: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let canonical = path
        .canonicalize()
        .map_err(|err| format!("invalid transcript path: {err}"))?;
    if allowed_transcript_roots(state)
        .into_iter()
        .any(|root| canonical.starts_with(root))
    {
        return Ok(canonical);
    }
    Err("transcript path escapes workspace or configured allowed roots".to_string())
}

fn allowed_transcript_roots(state: &AppState) -> Vec<std::path::PathBuf> {
    let mut roots = vec![];
    if let Ok(root) = state.workspace.canonicalize() {
        roots.push(root);
    }
    if let Some(claude_workspace) = &state.config.claude_workspace {
        if let Ok(root) = claude_workspace.canonicalize() {
            roots.push(root);
        }
    }
    roots
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
                "worker_kind": route_worker_kind_hint(req),
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

fn ensure_route_report_path(
    req: &mut RouteRequest,
    mode: &str,
    runtime: &str,
    route_id: &str,
) -> String {
    if req
        .report_path
        .as_ref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return "caller".to_string();
    }
    if mode != "start" || runtime != "pty" {
        return "none".to_string();
    }
    let session_name = req
        .session_name
        .clone()
        .unwrap_or_else(|| route_id.replace("route-", "route-pty-"));
    let safe_session = safe_path_segment(&session_name);
    let safe_route = safe_path_segment(route_id);
    req.report_path = Some(format!(".agents/agentcall/{safe_route}-{safe_session}.md"));
    "daemon_minted".to_string()
}

fn safe_path_segment(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('-');
        }
    }
    if out.is_empty() {
        "value".to_string()
    } else {
        out
    }
}

fn route_report_projection(
    state: &AppState,
    req: &RouteRequest,
    source: &str,
    warning: Option<Value>,
) -> Option<Value> {
    let path = req
        .report_path
        .as_ref()
        .filter(|value| !value.trim().is_empty())?;
    let target_workspace = route_target_workspace(state, req);
    let abs_path = materialize_route_path(&target_workspace, path);
    let report_workspace = abs_path
        .parent()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| target_workspace.display().to_string());
    Some(json!({
        "status": "report_not_requested",
        "ready": false,
        "path": path,
        "rel_path": path,
        "abs_path": abs_path.display().to_string(),
        "target_workspace": target_workspace.display().to_string(),
        "report_workspace": report_workspace,
        "source": source,
        "warning": warning.unwrap_or(Value::Null)
    }))
}

fn route_report_path_warning(state: &AppState, req: &RouteRequest, source: &str) -> Option<Value> {
    if source != "caller" {
        return None;
    }
    let report_path = req.report_path.as_ref()?;
    let target_workspace = route_target_workspace(state, req);
    let wanted = normalized_route_path_for_warning(&target_workspace, report_path);
    let routes = read_routes(state);
    let duplicate = routes
        .as_object()
        .into_iter()
        .flat_map(|items| items.values())
        .find_map(|route| {
            let route_workspace = route
                .get("workspace")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| state.workspace.clone());
            let existing = route_report_path_from_value(route)?;
            if normalized_route_path_for_warning(&route_workspace, &existing) == wanted {
                Some(json!({
                    "kind": "duplicate_explicit_report_path",
                    "route_id": route.get("route_id").cloned().unwrap_or(Value::Null),
                    "report_path": existing
                }))
            } else {
                None
            }
        });
    duplicate
}

fn route_report_path_from_value(route: &Value) -> Option<String> {
    route
        .pointer("/result/report/path")
        .and_then(Value::as_str)
        .or_else(|| {
            route
                .pointer("/result/context_packet/report_path")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            route
                .pointer("/result/prompt_gate/report_path")
                .and_then(Value::as_str)
        })
        .or_else(|| route.pointer("/result/report_path").and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn normalized_route_path_for_warning(root: &std::path::Path, path: &str) -> String {
    materialize_route_path(root, path)
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
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
    owner_id: &str,
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
    let audit_baseline =
        workspace_audit::initialize_session_audit(state, &session_name, &containment)
            .unwrap_or_else(|err| json!({"status": "unavailable", "error": err}));
    let target_workspace = route_target_workspace(state, req);
    let schedule = enforce_start_capacity(state, owner_id)?;
    let shared_workspace_lease = route_uses_shared_workspace_lease(state, req);
    let leases = reserve_route_leases(
        state,
        &session_name,
        owner_id,
        &target_workspace,
        shared_workspace_lease,
    )?;
    let session_record = SessionRecord {
        session_id: session_name.clone(),
        owner_id: leases.owner_lease.owner_id.clone(),
        workspace: target_workspace.display().to_string(),
        workspace_key: leases.workspace_lease.workspace_key.clone(),
        runtime: "pty".to_string(),
    };
    match state.store.acquire_route_leases_and_create_session(
        &session_record,
        &leases.owner_lease,
        Some(&leases.workspace_lease),
    )? {
        RouteDecisionV1::Created => {}
        RouteDecisionV1::Rejected(reason) => return Err(reason),
    }
    install_reserved_route_leases(state, &leases)?;
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
            let _ = release_owner_lease(state, &session_name, "route_start_failed");
            let _ = release_workspace_lease(state, &session_name, "route_start_failed");
        })?;
    let (handoff_path, short_prompt) =
        create_handoff_prompt(state, &record.route_id, &session_name, req, &containment)?;
    let prompt_gate = if is_claude_command(&runtime_session.info.command) {
        submit_pty_prompt_with_ack(
            state,
            &record.route_id,
            &session_name,
            short_prompt.clone(),
            Some(handoff_path.clone()),
            owner_id,
        )
    } else {
        submit_pty_prompt_without_hook_ack(
            state,
            &record.route_id,
            &session_name,
            short_prompt.clone(),
            owner_id,
        )
    };
    let prompt_status = prompt_gate
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("prompt_pending_ack")
        .to_string();
    record.status = prompt_status.to_string();
    record.session_name = Some(session_name.clone());
    let permission_mode = pty_initial_permission_mode(req, &workflow);
    record.result = json!({
        "runtime": "pty",
        "worker_kind": route_worker_kind(state, req),
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
        "owner_lease": leases.owner_lease,
        "workspace_lease": leases.workspace_lease,
        "prompt": prompt_gate,
        "prompt_gate": prompt_gate,
        "handoff": {
            "path": handoff_path,
            "short_prompt": short_prompt
        },
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
        "containment": containment,
        "workspace_audit": audit_baseline,
        "toolchain_context": toolchain_context_value(state, req)
    });
    Ok(())
}

fn route_target_workspace(state: &AppState, req: &RouteRequest) -> std::path::PathBuf {
    req.workspace
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| state.workspace.clone())
}

fn route_uses_shared_workspace_lease(state: &AppState, req: &RouteRequest) -> bool {
    route_worker_kind(state, req) == "report"
}

fn route_worker_kind(state: &AppState, req: &RouteRequest) -> &'static str {
    if route_is_report_only_write(state, req) {
        "report"
    } else {
        "coding"
    }
}

fn route_worker_kind_hint(req: &RouteRequest) -> &'static str {
    if req.write_paths.as_deref().unwrap_or(&[]).is_empty() {
        "report"
    } else if req.report_path.is_some()
        && req
            .write_paths
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .all(|path| route_write_path_looks_like_report_scope(path))
    {
        "report"
    } else {
        "coding"
    }
}

fn route_write_path_looks_like_report_scope(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/.agents/")
        || normalized.ends_with("/.agents")
        || normalized == ".agents"
        || normalized.starts_with(".agents/")
        || normalized.contains("/.agentcall/")
        || normalized.ends_with("/.agentcall")
        || normalized == ".agentcall"
        || normalized.starts_with(".agentcall/")
        || normalized.contains("/docs/reports")
        || normalized == "docs/reports"
        || normalized.starts_with("docs/reports/")
        || normalized.ends_with("/docs/report")
        || normalized == "docs/report"
}

fn route_is_report_only_write(state: &AppState, req: &RouteRequest) -> bool {
    let Some(report_path) = req
        .report_path
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    else {
        return false;
    };
    let write_paths = req.write_paths.as_deref().unwrap_or(&[]);
    if write_paths.is_empty() {
        return true;
    }
    let target_workspace = route_target_workspace(state, req);
    let report = normalized_route_path_for_warning(&target_workspace, report_path);
    write_paths
        .iter()
        .filter(|path| !path.trim().is_empty())
        .all(|path| route_write_path_is_report_scope(&target_workspace, path, &report))
}

fn route_write_path_is_report_scope(root: &std::path::Path, path: &str, report: &str) -> bool {
    let candidate = normalized_route_path_for_warning(root, path);
    if candidate == report {
        return true;
    }
    let prefix = format!("{}/", candidate.trim_end_matches('/'));
    report.starts_with(&prefix) && report_scope_prefix(&candidate)
}

fn report_scope_prefix(path: &str) -> bool {
    path.contains("/.agents/")
        || path.ends_with("/.agents")
        || path.contains("/.agentcall/")
        || path.ends_with("/.agentcall")
        || path.contains("/docs/reports")
        || path.contains("/docs/reportnreview")
        || path.ends_with("/docs/report")
}

fn pty_containment(state: &AppState, req: &RouteRequest, session_name: &str) -> Value {
    let write_paths_input = req.write_paths.clone().unwrap_or_default();
    let reference_paths = req.reference_paths.clone().unwrap_or_default();
    let scratch_path = format!(".agentcall/workspaces/{session_name}");
    let process_cwd =
        configured_claude_workspace(state).unwrap_or_else(|_| state.workspace.clone());
    let target_workspace = route_target_workspace(state, req);
    let scratch_abs = process_cwd.join(&scratch_path);
    let mut writable_paths = vec![];
    let mut writable_roots = vec![];
    writable_paths.push(scratch_path.clone());
    writable_roots.push(json!({
        "kind": "scratch",
        "display": scratch_path.clone(),
        "abs": scratch_abs.display().to_string()
    }));
    if let Some(report_path) = req
        .report_path
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        writable_paths.push(report_path.clone());
        writable_roots.push(json!({
            "kind": "report",
            "display": report_path,
            "abs": materialize_route_path(&target_workspace, report_path).display().to_string()
        }));
    }
    for path in &write_paths_input {
        if !path.trim().is_empty() && !writable_paths.iter().any(|item| item == path) {
            writable_paths.push(path.clone());
            writable_roots.push(json!({
                "kind": "write_path",
                "display": path,
                "abs": materialize_route_path(&target_workspace, path).display().to_string()
            }));
        }
    }
    let _ = std::fs::create_dir_all(&scratch_abs);
    let _ = std::fs::write(
        scratch_abs.join("README.md"),
        format!(
            "# AgentCall Session Scratch\n\nSession: `{session_name}`\n\nThis directory is writable for bounded helper artifacts, temporary scripts, and report material. Do not write outside route containment.\n"
        ),
    );
    json!({
        "mode": route_worker_kind(state, req),
        "write_paths_input": write_paths_input,
        "reference_paths": reference_paths,
        "writable_paths": writable_paths,
        "scratch_path": scratch_path,
        "roots": {
            "process_cwd": process_cwd.display().to_string(),
            "claude_workspace": process_cwd.display().to_string(),
            "target_workspace": target_workspace.display().to_string(),
            "scratch_root": scratch_abs.display().to_string()
        },
        "writable_roots": writable_roots,
        "scratch_root": scratch_abs.display().to_string(),
        "bash_write_policy": "monitored"
    })
}

fn materialize_route_path(root: &std::path::Path, path: &str) -> std::path::PathBuf {
    let candidate = std::path::PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        root.join(candidate)
    }
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
        || req.reference_paths.is_some()
        || req.write_paths.is_some()
        || req.acceptance_criteria.is_some()
        || req.persist_context.is_some()
        || req.report_path.is_some()
}

fn merge_result_field(result: &mut Value, key: &str, value: Value) {
    if !result.is_object() {
        *result = json!({});
    }
    if let Some(object) = result.as_object_mut() {
        object.insert(key.to_string(), value);
    }
}

fn submit_pty_prompt_with_ack(
    state: &Arc<AppState>,
    route_id: &str,
    wrapper_session: &str,
    prompt: String,
    handoff_path: Option<PathBuf>,
    owner_id: &str,
) -> Value {
    let idempotency_key = route_prompt_id(route_id, wrapper_session);
    let now = now_ms();
    let args = json!({
        "text": prompt,
        "enter": true,
        "idempotency_key": idempotency_key.clone(),
        "owner_id": owner_id
    });
    let command = match prepare_session_send_command(state, wrapper_session, "send", &args) {
        Ok(PreparedCommand::Submit(command)) => command,
        Ok(PreparedCommand::Deduped(previous)) => {
            return json!({
                "schema_version": 2,
                "status": "prompt_pending_ack",
                "command_status": "deduped",
                "idempotency_key": idempotency_key,
                "prompt_idempotency_key": idempotency_key,
                "dispatched": true,
                "acknowledged": false,
                "state": "prompt_pending_ack",
                "task_started": false,
                "prompt_id": idempotency_key,
                "prompt_written_at_ms": now,
                "ack_deadline_ms": DEFAULT_ACK_DEADLINE_MS,
                "commit_ack_deadline_ms": crate::prompt_gate::DEFAULT_COMMIT_ACK_DEADLINE_MS,
                "awaiting_hook": "UserPromptSubmit",
                "commit_attempts": [],
                "handoff_path": handoff_path.as_ref().map(|path| path.display().to_string()),
                "next_observation": "agentcall_session(view=summary)",
                "previous": previous
            });
        }
        Err(err) => {
            return json!({
                "schema_version": 2,
                "status": "prompt_commit_failed",
                "command_status": "prepare_failed",
                "idempotency_key": idempotency_key,
                "prompt_idempotency_key": idempotency_key,
                "dispatched": false,
                "acknowledged": false,
                "state": "prompt_commit_failed",
                "task_started": false,
                "prompt_id": idempotency_key,
                "prompt_written_at_ms": now,
                "ack_deadline_ms": DEFAULT_ACK_DEADLINE_MS,
                "commit_ack_deadline_ms": crate::prompt_gate::DEFAULT_COMMIT_ACK_DEADLINE_MS,
                "awaiting_hook": "UserPromptSubmit",
                "commit_attempts": [],
                "handoff_path": handoff_path.as_ref().map(|path| path.display().to_string()),
                "next_observation": "agentcall_session(view=summary)",
                "last_error": err
            });
        }
    };
    let command_id = command.command_id.clone();
    match submit_session_command(state, wrapper_session, command) {
        Ok(actor_result) => json!({
            "schema_version": 2,
            "status": "prompt_pending_ack",
            "command_status": "dispatched",
            "command_id": command_id,
            "idempotency_key": idempotency_key,
            "prompt_idempotency_key": idempotency_key,
            "dispatched": true,
            "acknowledged": false,
            "state": "prompt_pending_ack",
            "task_started": false,
            "prompt_id": idempotency_key,
            "prompt_written_at_ms": now,
            "ack_deadline_ms": DEFAULT_ACK_DEADLINE_MS,
            "commit_ack_deadline_ms": crate::prompt_gate::DEFAULT_COMMIT_ACK_DEADLINE_MS,
            "awaiting_hook": "UserPromptSubmit",
            "commit_attempts": [],
            "handoff_path": handoff_path.as_ref().map(|path| path.display().to_string()),
            "next_observation": "agentcall_session(view=summary)",
            "actor_result": actor_result
        }),
        Err(err) => json!({
            "schema_version": 2,
            "status": "prompt_commit_failed",
            "command_status": "dispatch_failed",
            "command_id": command_id,
            "idempotency_key": idempotency_key,
            "prompt_idempotency_key": idempotency_key,
            "dispatched": false,
            "acknowledged": false,
            "state": "prompt_commit_failed",
            "task_started": false,
            "prompt_id": idempotency_key,
            "prompt_written_at_ms": now,
            "ack_deadline_ms": DEFAULT_ACK_DEADLINE_MS,
            "commit_ack_deadline_ms": crate::prompt_gate::DEFAULT_COMMIT_ACK_DEADLINE_MS,
            "awaiting_hook": "UserPromptSubmit",
            "commit_attempts": [],
            "handoff_path": handoff_path.as_ref().map(|path| path.display().to_string()),
            "next_observation": "agentcall_session(view=summary)",
            "last_error": err
        }),
    }
}

fn submit_pty_prompt_without_hook_ack(
    state: &Arc<AppState>,
    route_id: &str,
    wrapper_session: &str,
    prompt: String,
    owner_id: &str,
) -> Value {
    let idempotency_key = format!("route_prompt:{route_id}:{wrapper_session}");
    let args = json!({
        "text": prompt,
        "enter": true,
        "idempotency_key": idempotency_key,
        "owner_id": owner_id
    });
    let command = match prepare_session_send_command(state, wrapper_session, "send", &args) {
        Ok(PreparedCommand::Submit(command)) => command,
        Ok(PreparedCommand::Deduped(previous)) => {
            return json!({
                "schema_version": 2,
                "status": "prompt_submitted",
                "command_status": "deduped",
                "idempotency_key": args.get("idempotency_key").and_then(Value::as_str),
                "dispatched": true,
                "state": "prompt_submitted",
                "task_started": true,
                "awaiting_hook": Value::Null,
                "previous": previous,
                "reason": "custom PTY command accepted prompt without Claude Code hook contract"
            });
        }
        Err(err) => {
            return json!({
                "schema_version": 2,
                "status": "prompt_commit_failed",
                "command_status": "prepare_failed",
                "idempotency_key": args.get("idempotency_key").and_then(Value::as_str),
                "dispatched": false,
                "state": "prompt_commit_failed",
                "task_started": false,
                "awaiting_hook": Value::Null,
                "last_error": err
            });
        }
    };
    let command_id = command.command_id.clone();
    match submit_session_command(state, wrapper_session, command) {
        Ok(result) => json!({
            "schema_version": 2,
            "status": "prompt_submitted",
            "command_status": "dispatched",
            "command_id": command_id,
            "idempotency_key": args.get("idempotency_key").and_then(Value::as_str),
            "dispatched": true,
            "state": "prompt_submitted",
            "task_started": true,
            "awaiting_hook": Value::Null,
            "actor_result": result,
            "reason": "custom PTY command accepted prompt without Claude Code hook contract"
        }),
        Err(err) => json!({
            "schema_version": 2,
            "status": "prompt_commit_failed",
            "command_status": "dispatch_failed",
            "command_id": command_id,
            "idempotency_key": args.get("idempotency_key").and_then(Value::as_str),
            "dispatched": false,
            "state": "prompt_commit_failed",
            "task_started": false,
            "awaiting_hook": Value::Null,
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

fn create_handoff_prompt(
    state: &AppState,
    route_id: &str,
    session_name: &str,
    req: &RouteRequest,
    containment: &Value,
) -> Result<(PathBuf, String), String> {
    let task_dir = state
        .workspace
        .join(".agentcall")
        .join("tasks")
        .join(route_id);
    std::fs::create_dir_all(&task_dir).map_err(|err| err.to_string())?;
    let handoff_path = task_dir.join("prompt.md");
    let full_prompt = pty_prompt(state, req, containment);
    std::fs::write(&handoff_path, full_prompt).map_err(|err| err.to_string())?;
    let report = req
        .report_path
        .as_deref()
        .unwrap_or(".agentcall/reports/report.md");
    let report_target = report_abs_path(containment).unwrap_or_else(|| report.to_string());
    let short_prompt = format!(
        "AgentCall handoff for `{session_name}`. Read and follow `{}`. Write the final report to `{report_target}`. When finished, say COMPLETE.",
        handoff_path.display()
    );
    Ok((handoff_path, short_prompt))
}

fn pty_prompt(state: &AppState, req: &RouteRequest, containment: &Value) -> String {
    let criteria = req
        .acceptance_criteria
        .as_ref()
        .map(|items| items.join("\n- "))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Complete the requested task and write a report.".to_string());
    let allowed = pty_prompt_containment(containment);
    let envelope = pty_prompt_control_envelope(req, containment);
    let toolchain = pty_prompt_toolchain_context(state, req);
    let workflow =
        PtyWorkflow::from_request(req.pty_workflow.as_deref()).unwrap_or(PtyWorkflow::Normal);
    match workflow {
        PtyWorkflow::PlanThenAuto => format!(
            "AgentCall PTY handoff. Start in PLAN MODE.\n\nStep 0 - control envelope:\n{}\n\nKnown local toolchain:\n{}\n\nObjective:\n{}\n\nRead/write boundaries:\n- {}\n\nAcceptance criteria:\n- {}\n\nPlan-phase rules:\n- Inspect the code and write a concrete plan only.\n- If anything important is unclear, ask concise clarification questions instead of guessing.\n- Do not modify project files during plan phase.\n- When the plan is ready, use ExitPlanMode and wait for approval. After approval, continue in auto mode and write the requested report.\n",
            envelope, toolchain, req.objective, allowed, criteria
        ),
        PtyWorkflow::Normal => format!(
            "AgentCall utility PTY worker.\n\nStep 0 - control envelope:\n{}\n\nKnown local toolchain:\n{}\n\nObjective:\n{}\n\nRead/write boundaries:\n- {}\n\nAcceptance criteria:\n- {}\n\nRules:\n- Work in auto mode.\n- If key context is unclear, ask a concise question in this PTY.\n- Respect write boundaries. Do not write outside them.\n- Use reference paths for reading context when provided.\n- When finished, write the requested report or summarize exact changes, tests, risks, and remaining questions.\n- Stop at the prompt for supervisor review.\n",
            envelope, toolchain, req.objective, allowed, criteria
        ),
    }
}

fn pty_prompt_control_envelope(req: &RouteRequest, containment: &Value) -> String {
    let mut lines = Vec::new();
    lines.push(
        "Confirm task id, ownership, allowed paths, and report target before editing.".to_string(),
    );
    if let Some(worker_kind) = containment.get("mode").and_then(Value::as_str) {
        lines.push(format!("worker_kind: {worker_kind}"));
        if worker_kind == "report" {
            lines.push("report worker: inspect, analyze, and write only the requested report/scratch artifacts; do not modify implementation files.".to_string());
        } else {
            lines.push("coding worker: modify only the listed write boundaries, then write the requested report.".to_string());
        }
    }
    if let Some(task_id) = req.task_id.as_ref() {
        lines.push(format!("task_id: {task_id}"));
    }
    if let Some(call_id) = req.call_id.as_ref() {
        lines.push(format!("call_id: {call_id}"));
    }
    if let Some(role) = req.role.as_ref() {
        lines.push(format!("role: {role}"));
    }
    if let Some(phase) = req.phase.as_ref() {
        lines.push(format!("phase: {phase}"));
    }
    if let Some(workspace) = req.workspace.as_ref() {
        lines.push(format!("target_workspace: {workspace}"));
    }
    if let Some(report_path) = req.report_path.as_ref() {
        lines.push(format!("report_path: {report_path}"));
        if let Some(report_abs) = report_abs_path(containment) {
            lines.push(format!("report_abs_path: {report_abs}"));
            lines.push(
                "report_path is relative to target_workspace; because Claude cwd may differ, write the final report to report_abs_path exactly."
                    .to_string(),
            );
        }
    }
    if let Some(scratch) = containment.get("scratch_path").and_then(Value::as_str) {
        lines.push(format!("scratch_path: {scratch}"));
    }
    lines.join("\n- ")
}

fn pty_prompt_toolchain_context(state: &AppState, req: &RouteRequest) -> String {
    let context = toolchain_context_value(state, req);
    if context.get("status").and_then(Value::as_str) == Some("missing") {
        return "- toolchain_context: missing\n- If a tool is missing from PATH, report the exact command and continue with static analysis where possible.".to_string();
    }
    let source = context
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut lines = vec![format!("- source: {source}")];
    if let Some(path) = context.get("path").and_then(Value::as_str) {
        lines.push(format!("- context_file: {path}"));
    }
    if let Some(summary) = context.get("summary").and_then(Value::as_str) {
        lines.push(format!("- {summary}"));
    }
    if let Some(hints) = context.get("hints").and_then(Value::as_array) {
        for hint in hints.iter().filter_map(Value::as_str).take(12) {
            lines.push(format!("- {hint}"));
        }
    }
    lines.join("\n")
}

fn toolchain_context_value(state: &AppState, req: &RouteRequest) -> Value {
    let target_workspace = route_target_workspace(state, req);
    let candidates = [
        target_workspace.join(".agentcall").join("toolchain.json"),
        state.workspace.join("config").join("toolchain.local.json"),
    ];
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return json!({
                "status": "invalid",
                "source": "file",
                "path": path.display().to_string(),
                "summary": "Toolchain context file exists but is invalid JSON; do not treat PATH misses as final blockers without reporting this."
            });
        };
        return json!({
            "status": "available",
            "source": "file",
            "path": path.display().to_string(),
            "summary": value.get("summary").and_then(Value::as_str).unwrap_or("Use the provided local toolchain hints before declaring a tool unavailable."),
            "hints": toolchain_hints_from_value(&value),
            "raw": value
        });
    }
    json!({
        "status": "missing",
        "source": "none",
        "summary": "No toolchain context file found."
    })
}

fn toolchain_hints_from_value(value: &Value) -> Value {
    if let Some(hints) = value.get("hints").and_then(Value::as_array) {
        return Value::Array(
            hints
                .iter()
                .filter(|item| item.is_string())
                .cloned()
                .collect(),
        );
    }
    let mut hints = vec![];
    for key in ["go", "node", "npm", "python", "cache", "tmp"] {
        if let Some(item) = value.get(key) {
            hints.push(json!(format!("{key}: {}", compact_json_for_prompt(item))));
        }
    }
    Value::Array(hints)
}

fn compact_json_for_prompt(value: &Value) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| "<unserializable>".to_string())
        .chars()
        .take(500)
        .collect()
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
    if let Some(paths) = containment.get("writable_roots").and_then(Value::as_array) {
        for path in paths {
            let display = path.get("display").and_then(Value::as_str).unwrap_or("");
            let abs = path.get("abs").and_then(Value::as_str).unwrap_or("");
            if !abs.is_empty() {
                lines.push(format!("writable_abs: {display} => {abs}"));
            }
        }
    }
    if let Some(paths) = containment
        .get("write_paths_input")
        .and_then(Value::as_array)
    {
        for path in paths.iter().filter_map(Value::as_str) {
            lines.push(format!("write boundary: {path}"));
        }
    }
    if let Some(paths) = containment.get("reference_paths").and_then(Value::as_array) {
        for path in paths.iter().filter_map(Value::as_str) {
            lines.push(format!("reference path: {path}"));
        }
    }
    if let Some(scratch) = containment.get("scratch_path").and_then(Value::as_str) {
        lines.push(format!("session scratch: {scratch}"));
    }
    if let Some(policy) = containment.get("bash_write_policy").and_then(Value::as_str) {
        lines.push(format!("Bash write policy: {policy}; keep helper scripts and generated artifacts under session scratch/report paths; AgentCall monitors changed folders and will pause for supervisor approval if target folders change outside writable boundaries"));
    }
    if lines.is_empty() {
        "Use the task workspace carefully.".to_string()
    } else {
        lines.join("\n- ")
    }
}

fn report_abs_path(containment: &Value) -> Option<String> {
    containment
        .get("writable_roots")
        .and_then(Value::as_array)?
        .iter()
        .find(|item| item.get("kind").and_then(Value::as_str) == Some("report"))?
        .get("abs")
        .and_then(Value::as_str)
        .map(str::to_string)
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
                reference_paths: None,
                write_paths: None,
                acceptance_criteria: None,
                persist_context: None,
                report_path: None,
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
                reference_paths: None,
                write_paths: None,
                acceptance_criteria: None,
                persist_context: None,
                report_path: None,
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
                reference_paths: None,
                write_paths: None,
                acceptance_criteria: None,
                persist_context: None,
                report_path: None,
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
                reference_paths: Some(vec!["docs".to_string()]),
                write_paths: Some(vec!["src".to_string()]),
                acceptance_criteria: Some(vec!["report risks".to_string()]),
                persist_context: Some(true),
                report_path: Some("src/report.md".to_string()),
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
    fn pty_start_mints_report_path_when_caller_omits_one() {
        let workspace = test_workspace("route-report-minted");
        let state = Arc::new(AppState::test(workspace));
        let mut req = RouteRequest {
            objective: "write a compact report".to_string(),
            workspace: None,
            mode: Some("start".to_string()),
            runtime: Some("pty".to_string()),
            estimated_minutes: None,
            estimated_files: None,
            estimated_loc: None,
            needs_continuity: None,
            risk: None,
            session_name: Some("worker:one".to_string()),
            command: Some(vec!["fake-worker".to_string()]),
            task_id: None,
            call_id: None,
            phase: None,
            role: None,
            reference_paths: None,
            write_paths: Some(vec![".agents/agentcall".to_string()]),
            acceptance_criteria: None,
            persist_context: None,
            report_path: None,
            pty_workflow: None,
            initial_permission_mode: None,
        };
        let source = ensure_route_report_path(&mut req, "start", "pty", "route-123");
        let report = route_report_projection(&state, &req, &source, None).unwrap();

        assert_eq!(report["status"], "report_not_requested");
        assert_eq!(report["ready"], false);
        assert_eq!(report["source"], "daemon_minted");
        assert_eq!(report["path"], ".agents/agentcall/route-123-worker-one.md");
        assert!(
            report["abs_path"]
                .as_str()
                .unwrap()
                .contains("worker-one.md")
        );
    }

    #[test]
    fn report_worker_uses_shared_workspace_lease() {
        let workspace = test_workspace("route-report-shared-lease");
        let state = Arc::new(AppState::test(workspace.clone()));
        let first = report_route(
            "review-a",
            Some(vec![".agents/agentcall/review-a.md".to_string()]),
            ".agents/agentcall/review-a.md",
        );
        let second = report_route(
            "review-b",
            Some(vec![".agents/agentcall".to_string()]),
            ".agents/agentcall/review-b.md",
        );

        assert!(route_uses_shared_workspace_lease(&state, &first));
        assert!(route_uses_shared_workspace_lease(&state, &second));
        let first_reservation = reserve_route_leases(
            &state,
            "review-a",
            "codex",
            &workspace,
            route_uses_shared_workspace_lease(&state, &first),
        )
        .unwrap();
        install_reserved_route_leases(&state, &first_reservation).unwrap();
        let second_reservation = reserve_route_leases(
            &state,
            "review-b",
            "codex",
            &workspace,
            route_uses_shared_workspace_lease(&state, &second),
        )
        .unwrap();

        assert_eq!(
            first_reservation.workspace_lease.mode,
            crate::ownership::WorkspaceLeaseMode::SharedReport
        );
        assert_eq!(
            second_reservation.workspace_lease.mode,
            crate::ownership::WorkspaceLeaseMode::SharedReport
        );
    }

    #[test]
    fn route_request_rejects_removed_read_only_field() {
        let err = serde_json::from_value::<RouteRequest>(json!({
            "objective": "inspect and report",
            "read_only": true
        }))
        .unwrap_err();
        assert!(err.to_string().contains("read_only"));
    }

    #[test]
    fn implementation_write_route_keeps_exclusive_workspace_lease() {
        let workspace = test_workspace("route-report-exclusive");
        let state = Arc::new(AppState::test(workspace));
        let req = report_route(
            "impl-a",
            Some(vec!["src".to_string()]),
            ".agents/agentcall/impl-a.md",
        );

        assert!(!route_uses_shared_workspace_lease(&state, &req));
    }

    #[test]
    fn explicit_duplicate_report_path_is_projected_as_warning() {
        let workspace = test_workspace("route-report-duplicate");
        let state = Arc::new(AppState::test(workspace.clone()));
        let state_dir = workspace.join(".agentcall").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        write_json_file(
            &state_dir.join("routes.json"),
            &json!({
                "route-a": {
                    "route_id": "route-a",
                    "workspace": workspace.display().to_string(),
                    "result": {
                        "report": {
                            "path": "reports/shared.md"
                        }
                    }
                }
            }),
        )
        .unwrap();
        let req = RouteRequest {
            objective: "second".to_string(),
            workspace: Some(workspace.display().to_string()),
            mode: Some("start".to_string()),
            runtime: Some("pty".to_string()),
            estimated_minutes: None,
            estimated_files: None,
            estimated_loc: None,
            needs_continuity: None,
            risk: None,
            session_name: Some("worker-b".to_string()),
            command: Some(vec!["fake-worker".to_string()]),
            task_id: None,
            call_id: None,
            phase: None,
            role: None,
            reference_paths: None,
            write_paths: Some(vec!["reports".to_string()]),
            acceptance_criteria: None,
            persist_context: None,
            report_path: Some("reports/shared.md".to_string()),
            pty_workflow: None,
            initial_permission_mode: None,
        };
        let warning = route_report_path_warning(&state, &req, "caller").unwrap();

        assert_eq!(warning["kind"], "duplicate_explicit_report_path");
        assert_eq!(warning["route_id"], "route-a");
    }

    #[test]
    fn checkpoint_session_preserves_active_sessions_object_shape() {
        let workspace = test_workspace("checkpoint-object");
        let state = Arc::new(AppState::test(workspace.clone()));
        let state_dir = workspace.join(".agentcall").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        write_json_file(
            &state_dir.join("active_sessions.json"),
            &json!({
                "worker-a": {
                    "session_id": "worker-a",
                    "status": "running",
                    "runtime": "daemon"
                },
                "worker-b": {
                    "session_id": "worker-b",
                    "status": "running",
                    "runtime": "daemon"
                }
            }),
        )
        .unwrap();

        checkpoint_session(&state, "worker-a").unwrap();

        let sessions = read_json_file(&state_dir.join("active_sessions.json"), json!({}));
        assert!(sessions.is_object());
        assert_eq!(sessions["worker-a"]["status"], "checkpoint_requested");
        assert_eq!(sessions["worker-b"]["status"], "running");
    }

    #[test]
    fn route_prompt_gate_dispatches_once_without_waiting_for_hook_ack() {
        let workspace = test_workspace("route-prompt-gate");
        let state = Arc::new(AppState::test(workspace));

        let gate = submit_pty_prompt_with_ack(
            &state,
            "route-test",
            "missing-worker",
            "do work".to_string(),
            None,
            "codex",
        );

        assert_eq!(gate["status"], "prompt_commit_failed");
        assert_eq!(gate["command_status"], "dispatch_failed");
        assert_eq!(
            gate["idempotency_key"],
            "route_prompt:route-test:missing-worker"
        );
        assert_eq!(gate["dispatched"], false);
        assert_eq!(gate["awaiting_hook"], "UserPromptSubmit");
        assert_eq!(gate["ack_deadline_ms"], DEFAULT_ACK_DEADLINE_MS);
        assert_eq!(gate["next_observation"], "agentcall_session(view=summary)");
        assert_eq!(gate["commit_attempts"].as_array().unwrap().len(), 0);
        assert!(
            gate["last_error"]
                .as_str()
                .unwrap()
                .contains("missing session actor")
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
            reference_paths: None,
            write_paths: Some(vec!["src".to_string()]),
            acceptance_criteria: None,
            persist_context: None,
            report_path: None,
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
            reference_paths: None,
            write_paths: Some(vec!["src".to_string()]),
            acceptance_criteria: None,
            persist_context: None,
            report_path: Some("docs/report.md".to_string()),
            pty_workflow: None,
            initial_permission_mode: None,
        };
        let containment = pty_containment(&state, &req, "containment-a");
        assert_eq!(containment["mode"], "coding");
        assert_eq!(containment["bash_write_policy"], "monitored");
        assert_eq!(
            containment["scratch_path"],
            ".agentcall/workspaces/containment-a"
        );
        assert_eq!(
            containment["roots"]["process_cwd"],
            workspace.display().to_string()
        );
        assert_eq!(
            containment["roots"]["target_workspace"],
            workspace.display().to_string()
        );
        assert_eq!(
            containment["roots"]["scratch_root"],
            workspace
                .join(".agentcall/workspaces/containment-a")
                .display()
                .to_string()
        );
        let writable = containment["writable_paths"].as_array().unwrap();
        assert!(
            writable
                .iter()
                .any(|value| value == ".agentcall/workspaces/containment-a")
        );
        assert!(writable.iter().any(|value| value == "docs/report.md"));
        assert!(writable.iter().any(|value| value == "src"));
        let writable_roots = containment["writable_roots"].as_array().unwrap();
        assert!(writable_roots.iter().any(|root| {
            root["kind"] == "scratch"
                && root["abs"]
                    == workspace
                        .join(".agentcall/workspaces/containment-a")
                        .display()
                        .to_string()
        }));
        assert!(writable_roots.iter().any(|root| {
            root["kind"] == "report"
                && root["abs"] == workspace.join("docs/report.md").display().to_string()
        }));
        assert!(
            workspace
                .join(".agentcall/workspaces/containment-a/README.md")
                .exists()
        );
    }

    #[test]
    fn pty_handoff_names_absolute_report_path_when_cwd_differs() {
        let workspace = test_workspace("handoff-report-abs");
        let target_workspace = workspace.join("target-workspace");
        let state = AppState::test(workspace.clone());
        let req = RouteRequest {
            objective: "write report".to_string(),
            workspace: Some(target_workspace.display().to_string()),
            mode: Some("start".to_string()),
            runtime: Some("pty".to_string()),
            estimated_minutes: None,
            estimated_files: None,
            estimated_loc: None,
            needs_continuity: None,
            risk: None,
            session_name: Some("handoff-a".to_string()),
            command: None,
            task_id: None,
            call_id: None,
            phase: None,
            role: None,
            reference_paths: None,
            write_paths: Some(vec![".agentcall/reports".to_string()]),
            acceptance_criteria: None,
            persist_context: None,
            report_path: Some(".agentcall/reports/report.md".to_string()),
            pty_workflow: None,
            initial_permission_mode: None,
        };
        let containment = pty_containment(&state, &req, "handoff-a");
        let report_abs = target_workspace
            .join(".agentcall/reports/report.md")
            .display()
            .to_string();
        assert_eq!(
            report_abs_path(&containment).as_deref(),
            Some(report_abs.as_str())
        );

        let envelope = pty_prompt_control_envelope(&req, &containment);
        assert!(envelope.contains("report_abs_path:"));
        assert!(envelope.contains(&report_abs));
        let containment_text = pty_prompt_containment(&containment);
        assert!(containment_text.contains("writable_abs: .agentcall/reports/report.md =>"));
        assert!(containment_text.contains(&report_abs));
    }

    #[test]
    fn pty_containment_report_worker_has_writable_report_and_scratch() {
        let workspace = test_workspace("containment-report");
        let state = AppState::test(workspace.clone());
        let req = RouteRequest {
            objective: "write audit report".to_string(),
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
            reference_paths: None,
            write_paths: None,
            acceptance_criteria: None,
            persist_context: None,
            report_path: Some("docs/report.md".to_string()),
            pty_workflow: None,
            initial_permission_mode: None,
        };
        let containment = pty_containment(&state, &req, "containment-report");
        assert_eq!(containment["mode"], "report");
        let writable = containment["writable_paths"].as_array().unwrap();
        assert!(writable.iter().any(|value| value == "docs/report.md"));
        assert!(!writable.iter().any(|value| value == "src"));
        assert!(
            writable
                .iter()
                .any(|value| value == ".agentcall/workspaces/containment-report")
        );
        assert_eq!(
            containment["roots"]["scratch_root"],
            workspace
                .join(".agentcall/workspaces/containment-report")
                .display()
                .to_string()
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

    #[test]
    fn context_rejects_path_traversal_ids() {
        let workspace = test_workspace("context-traversal");
        let state = Arc::new(AppState::test(workspace));
        let err = create_context(
            &state,
            ContextRequest {
                task_id: "../escape".to_string(),
                call_id: "call-a".to_string(),
                objective: "do work".to_string(),
                phase: None,
                role: None,
                runtime: None,
                workspace: None,
                reference_paths: None,
                write_paths: None,
                acceptance_criteria: None,
                persist: Some(true),
                report_path: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("safe path segments"));
    }

    #[test]
    fn transcript_index_rejects_path_outside_allowed_roots() {
        let workspace = test_workspace("transcript-traversal");
        let outside = test_workspace("transcript-outside");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let transcript = outside.join("transcript.jsonl");
        std::fs::write(&transcript, r#"{"role":"user","content":"go"}"#).unwrap();
        let state = Arc::new(AppState::test(workspace));
        let err = index_transcript(
            &state,
            TranscriptIndexRequest {
                path: transcript.display().to_string(),
                session_id: Some("sess".to_string()),
            },
        )
        .unwrap_err();
        assert!(err.contains("escapes workspace"));
        let _ = std::fs::remove_dir_all(outside);
    }

    fn test_workspace(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("agentcall-routes-{name}-{nonce}"))
    }

    fn report_route(
        session_name: &str,
        write_paths: Option<Vec<String>>,
        report_path: &str,
    ) -> RouteRequest {
        RouteRequest {
            objective: "write a report".to_string(),
            workspace: None,
            mode: Some("start".to_string()),
            runtime: Some("pty".to_string()),
            estimated_minutes: None,
            estimated_files: None,
            estimated_loc: None,
            needs_continuity: None,
            risk: None,
            session_name: Some(session_name.to_string()),
            command: None,
            task_id: None,
            call_id: None,
            phase: None,
            role: None,
            reference_paths: None,
            write_paths,
            acceptance_criteria: None,
            persist_context: None,
            report_path: Some(report_path.to_string()),
            pty_workflow: None,
            initial_permission_mode: None,
        }
    }
}
