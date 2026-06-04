use crate::acp::{AcpInvocation, AcpPermissionPolicy, run_acp_invocation};
use crate::acp_supervisor::{
    AcpInvocationStart, AcpSupervisorConfig, active_invocation_count, record_finished,
    record_progress, record_started, start_heartbeat,
};
use crate::session::{
    InputRequest, StartRequest, configured_claude_workspace, start_session, write_input,
};
use crate::state::{AppState, append_agent_event_locked, read_json_file, write_json_file};
use crate::util::{now_ms, safe_name};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const SOP_TEMPLATES: [&str; 5] = [
    "read-and-report",
    "evidence-check",
    "contract-check",
    "diff-review",
    "single-report-update",
];

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
    template: Option<String>,
    target_files: Option<Vec<String>>,
    report_path: Option<String>,
    max_reads: Option<u64>,
    max_writes: Option<u64>,
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
    session_name: Option<String>,
    template: Option<String>,
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
        match value.unwrap_or("plan_then_auto") {
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
    if !matches!(runtime, "auto" | "pty" | "acp") {
        return Err("runtime must be auto, pty, or acp".to_string());
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
            "call agentcall_route with mode=start after satisfying the SOP gate".to_string()
        },
        session_name: None,
        template: req.template.clone(),
        created_at: now_ms(),
        updated_at: now_ms(),
        result: json!({}),
    };

    if decision.runtime == "needs_contract" {
        record.status = "needs_contract".to_string();
        record.required_next_step =
            "provide a valid SOP template contract or force runtime=pty".to_string();
        record.result = decision.score_breakdown.clone();
    } else if mode == "start" {
        match decision.runtime.as_str() {
            "pty" => start_pty_route(state, &req, &mut record)?,
            "acp" => start_acp_route(state, &req, &mut record)?,
            other => return Err(format!("unsupported route runtime: {other}")),
        }
    }
    if route_has_context_fields(&req) && !(mode == "start" && decision.runtime == "acp") {
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
                template: req.template.clone(),
                target_files: req.target_files.clone(),
                report_path: req.report_path.clone(),
                max_reads: req.max_reads,
                max_writes: req.max_writes,
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
    template: Option<String>,
    target_files: Option<Vec<String>>,
    report_path: Option<String>,
    max_reads: Option<u64>,
    max_writes: Option<u64>,
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
        "template": req.template,
        "target_files": req.target_files.unwrap_or_default(),
        "report_path": req.report_path,
        "max_reads": req.max_reads,
        "max_writes": req.max_writes,
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
    if requested == "pty" {
        return RouteDecision {
            runtime: requested.to_string(),
            reason: format!("runtime forced by caller: {requested}"),
            score_breakdown: json!({"forced": requested}),
        };
    }
    let contract = validate_sop_contract(req);
    if requested == "acp" || requested == "auto" {
        return match contract {
            Ok(contract) => RouteDecision {
                runtime: "acp".to_string(),
                reason: if requested == "acp" {
                    format!("runtime forced by caller: acp; SOP template {} passed gate", contract.template)
                } else {
                    format!("SOP template {} passed gate; use ACP worker", contract.template)
                },
                score_breakdown: json!({
                    "decision_model": "sop_gate",
                    "template": contract.template,
                    "target_files": contract.target_files,
                    "report_path": contract.report_path,
                    "max_reads": contract.max_reads,
                    "max_writes": contract.max_writes,
                    "sop_status": "contract_ready"
                }),
            },
            Err(missing) => RouteDecision {
                runtime: "needs_contract".to_string(),
                reason: "ACP requires a valid SOP contract; route will not infer smallness from estimates".to_string(),
                score_breakdown: json!({
                    "decision_model": "sop_gate",
                    "sop_status": "needs_contract",
                    "requested_runtime": requested,
                    "missing_or_invalid": missing,
                    "legacy_estimates_ignored": {
                        "estimated_minutes": req.estimated_minutes,
                        "estimated_files": req.estimated_files,
                        "estimated_loc": req.estimated_loc,
                        "needs_continuity": req.needs_continuity,
                        "risk": req.risk,
                    },
                    "recommended_runtime": "pty",
                    "guidance": "provide template, target_files, report_path, allowed_paths, and acceptance_criteria, or force runtime=pty"
                }),
            },
        };
    }
    RouteDecision {
        runtime: "needs_contract".to_string(),
        reason: "unsupported route request".to_string(),
        score_breakdown: json!({"sop_status": "needs_contract"}),
    }
}

#[derive(Clone, Debug)]
struct SopContract {
    template: String,
    target_files: Vec<String>,
    allowed_paths: Vec<String>,
    report_path: String,
    max_reads: u64,
    max_writes: u64,
}

fn validate_sop_contract(req: &RouteRequest) -> Result<SopContract, Vec<String>> {
    let mut missing = Vec::new();
    let template = req.template.as_deref().unwrap_or("").trim().to_string();
    if !SOP_TEMPLATES.contains(&template.as_str()) {
        missing.push("template must be one of read-and-report, evidence-check, contract-check, diff-review, single-report-update".to_string());
    }
    let target_files = req.target_files.clone().unwrap_or_default();
    if target_files.is_empty() {
        missing.push("target_files is required".to_string());
    }
    let allowed_paths = req.allowed_paths.clone().unwrap_or_default();
    if allowed_paths.is_empty() {
        missing.push("allowed_paths is required for ACP SOP containment".to_string());
    }
    let report_path = req.report_path.as_deref().unwrap_or("").trim().to_string();
    if report_path.is_empty() {
        missing.push("report_path is required".to_string());
    }
    if req
        .acceptance_criteria
        .as_ref()
        .map(|items| items.is_empty())
        .unwrap_or(true)
    {
        missing.push("acceptance_criteria is required".to_string());
    }
    let max_reads = req
        .max_reads
        .unwrap_or_else(|| target_files.len().max(1) as u64 + 5);
    let max_writes = req.max_writes.unwrap_or(1);
    if max_writes > 1 {
        missing.push("max_writes must be <= 1 for ACP SOP workers".to_string());
    }
    if !report_path.is_empty()
        && !allowed_paths.is_empty()
        && !allowed_paths
            .iter()
            .any(|allowed| path_within_or_equal(&report_path, allowed))
    {
        missing.push("report_path must be inside allowed_paths".to_string());
    }
    if template == "single-report-update"
        && !report_path.is_empty()
        && target_files
            .iter()
            .any(|file| !same_path(file, &report_path))
    {
        missing.push("single-report-update target_files must only include report_path".to_string());
    }
    if missing.is_empty() {
        Ok(SopContract {
            template,
            target_files,
            allowed_paths,
            report_path,
            max_reads,
            max_writes,
        })
    } else {
        Err(missing)
    }
}

#[allow(dead_code)]
fn sop_contract_from_context(packet: &Value) -> Result<SopContract, String> {
    let template = packet
        .get("template")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let target_files = string_array(packet.get("target_files"));
    let allowed_paths = string_array(packet.get("allowed_paths"));
    let report_path = packet
        .get("report_path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if template.is_empty() || report_path.is_empty() {
        return Err("context packet does not contain a valid SOP contract".to_string());
    }
    Ok(SopContract {
        template,
        target_files,
        allowed_paths,
        report_path,
        max_reads: packet
            .get("max_reads")
            .and_then(Value::as_u64)
            .unwrap_or(20),
        max_writes: packet
            .get("max_writes")
            .and_then(Value::as_u64)
            .unwrap_or(1),
    })
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn normalized_path(value: &str) -> String {
    value
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

fn same_path(left: &str, right: &str) -> bool {
    normalized_path(left) == normalized_path(right)
}

fn path_within_or_equal(path: &str, parent: &str) -> bool {
    let path = normalized_path(path);
    let parent = normalized_path(parent);
    path == parent || path.starts_with(&(parent + "\\"))
}

#[allow(dead_code)]
fn legacy_size_decision(req: &RouteRequest) -> RouteDecision {
    let estimated_minutes = req.estimated_minutes.unwrap_or(0);
    let estimated_files = req.estimated_files.unwrap_or(0);
    let estimated_loc = req.estimated_loc.unwrap_or(0);
    RouteDecision {
        runtime: "pty".to_string(),
        reason: "legacy estimates are retained for compatibility but do not select ACP".to_string(),
        score_breakdown: json!({
            "decision_model": "legacy_estimates_disabled",
            "estimated_minutes": estimated_minutes,
            "estimated_files": estimated_files,
            "estimated_loc": estimated_loc,
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
    let workflow = PtyWorkflow::from_request(req.pty_workflow.as_deref())?;
    let claude_session_id = if workflow == PtyWorkflow::PlanThenAuto {
        Some(new_claude_session_id(state))
    } else {
        None
    };
    let command = pty_command(req, &workflow, claude_session_id.as_deref())?;
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
    let prompt_status = match write_input(
        state,
        &session_name,
        InputRequest {
            text: pty_prompt(req),
            enter: Some(true),
        },
    ) {
        Ok(_) => {
            if wait_for_user_prompt_submit(state, &session_name, Duration::from_secs(15)) {
                "started_and_prompt_submitted"
            } else {
                "started_pending_prompt"
            }
        }
        Err(_) => "started_pending_prompt",
    };
    record.status = prompt_status.to_string();
    record.session_name = Some(session_name.clone());
    let permission_mode = pty_initial_permission_mode(req, &workflow);
    record.result = json!({
        "runtime": "pty",
        "pty_workflow": workflow.as_str(),
        "workflow_status": if workflow == PtyWorkflow::PlanThenAuto { "plan_running" } else { "running" },
        "phase": if workflow == PtyWorkflow::PlanThenAuto { "plan" } else { "execute" },
        "permission_mode": permission_mode,
        "mode_source": "route",
        "claude_session_id": claude_session_id,
        "plan_session_name": if workflow == PtyWorkflow::PlanThenAuto { Some(session_name.clone()) } else { None },
        "auto_session_name": serde_json::Value::Null,
        "session": info,
        "prompt_gate": {
            "status": prompt_status,
            "expected_hook": "UserPromptSubmit"
        },
        "binding_gate": {
            "required": true,
            "expected_binding_source": "env",
            "status": "pending_hook"
        }
    });
    Ok(())
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

struct AcpChildContext {
    task_id: String,
    call_id: String,
    role: String,
    phase: String,
}

fn acp_child_context(
    req: &RouteRequest,
    record: &RouteRecord,
    invocation_id: &str,
) -> AcpChildContext {
    AcpChildContext {
        task_id: req
            .task_id
            .clone()
            .unwrap_or_else(|| record.route_id.clone()),
        call_id: req
            .call_id
            .clone()
            .unwrap_or_else(|| invocation_id.to_string()),
        role: req.role.clone().unwrap_or_else(|| "executor".to_string()),
        phase: req.phase.clone().unwrap_or_else(|| "execute".to_string()),
    }
}

fn start_acp_route(
    state: &Arc<AppState>,
    req: &RouteRequest,
    record: &mut RouteRecord,
) -> Result<(), String> {
    let contract = validate_sop_contract(req)
        .map_err(|missing| format!("ACP SOP contract is incomplete: {}", missing.join("; ")))?;
    let invocation_id = record.route_id.replace("route-", "acp-");
    record.invocation_id = Some(invocation_id.clone());
    let supervisor_config = AcpSupervisorConfig::from_state(state);
    let timeout_seconds = req
        .timeout_seconds
        .unwrap_or(supervisor_config.default_timeout_seconds);
    if timeout_seconds == 0 || timeout_seconds > supervisor_config.max_timeout_seconds {
        return Err(format!(
            "timeout_seconds must be between 1 and configured acp_max_timeout_seconds ({})",
            supervisor_config.max_timeout_seconds
        ));
    }
    let active_count = active_invocation_count(state);
    if active_count >= supervisor_config.max_active_invocations {
        record.status = "acp_capacity_exceeded".to_string();
        record.required_next_step =
            "wait for an active ACP invocation to finish or force PTY".to_string();
        record.result = json!({
            "runtime": "acp",
            "invocation_id": invocation_id,
            "sop_status": "acp_capacity_exceeded",
            "active_invocations": active_count,
            "max_active_invocations": supervisor_config.max_active_invocations,
            "message": "ACP active invocation cap reached; AgentCall does not queue ACP work.",
        });
        crate::state::append_agent_event(
            state,
            "acp.capacity_exceeded",
            "ACP route rejected because the active invocation cap is reached.",
            json!({
                "route_id": record.route_id,
                "invocation_id": record.invocation_id,
                "active_invocations": active_count,
                "max_active_invocations": supervisor_config.max_active_invocations,
            }),
        );
        return Ok(());
    }
    let child = acp_child_context(req, record, &invocation_id);
    let context_packet = create_context(
        state,
        ContextRequest {
            task_id: child.task_id.clone(),
            call_id: child.call_id.clone(),
            objective: req.objective.clone(),
            phase: Some(child.phase.clone()),
            role: Some(child.role.clone()),
            runtime: Some("acp".to_string()),
            workspace: req.workspace.clone(),
            allowed_paths: req.allowed_paths.clone(),
            acceptance_criteria: req.acceptance_criteria.clone(),
            persist: req.persist_context,
            template: req.template.clone(),
            target_files: req.target_files.clone(),
            report_path: req.report_path.clone(),
            max_reads: req.max_reads,
            max_writes: req.max_writes,
        },
    )?;
    let command = req
        .adapter_command
        .clone()
        .or_else(|| state.config.acp_command.clone())
        .or_else(acp_command_from_env);
    let workspace = req
        .workspace
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| state.workspace.clone());
    let cwd = configured_claude_workspace(state)?;
    let prompt = acp_prompt(req, &context_packet);
    let lifecycle_base = json!({
        "task_id": child.task_id.clone(),
        "call_id": child.call_id.clone(),
        "role": child.role.clone(),
        "phase": child.phase.clone(),
        "runtime": "acp",
        "workspace": workspace.clone(),
        "cwd": cwd.clone(),
        "invocation_id": invocation_id.clone(),
        "context_packet": context_packet.clone(),
        "template": contract.template.clone(),
        "report_path": contract.report_path.clone(),
    });
    crate::state::append_agent_event(
        state,
        "child.call_started",
        "ACP child call started.",
        lifecycle_base.clone(),
    );
    crate::state::append_agent_event(
        state,
        "agent.state_changed",
        "ACP child is running.",
        merge_json(
            lifecycle_base.clone(),
            json!({"state": "running", "agent": "rust_native_acp"}),
        ),
    );
    if let Some(command) = command {
        let route_id = record.route_id.clone();
        let state_for_thread = Arc::clone(state);
        let context_for_thread = context_packet.clone();
        let lifecycle_for_thread = lifecycle_base.clone();
        let invocation_for_thread = invocation_id.clone();
        let child_call_id = child.call_id.clone();
        let mode_id = acp_mode_id(req);
        let policy = AcpPermissionPolicy {
            template: contract.template.clone(),
            target_files: contract.target_files.clone(),
            allowed_paths: contract.allowed_paths.clone(),
            report_path: Some(contract.report_path.clone()),
        };
        record_started(
            state,
            AcpInvocationStart {
                route_id: record.route_id.clone(),
                invocation_id: invocation_id.clone(),
                task_id: child.task_id.clone(),
                call_id: child.call_id.clone(),
                workspace: workspace.clone(),
                cwd: cwd.clone(),
                template: contract.template.clone(),
                report_path: contract.report_path.clone(),
                command: command.clone(),
                hard_timeout_seconds: timeout_seconds,
                checkpoint_due_after_seconds: supervisor_config.checkpoint_due_seconds,
                heartbeat_interval_seconds: supervisor_config.heartbeat_interval_seconds,
            },
        )?;
        record.status = "started".to_string();
        record.required_next_step = "inspect_board_or_report".to_string();
        record.result = json!({
            "runtime": "acp",
            "invocation_id": invocation_id,
            "adapter": "rust_native_acp",
            "command": command.clone(),
            "workspace": workspace.clone(),
            "cwd": cwd.clone(),
            "context_packet": context_packet.clone(),
            "template": contract.template,
            "sop_status": "running",
            "permission_denials": [],
            "report_contract_status": "pending",
            "checkpoint_due": false,
            "hard_timeout_seconds": timeout_seconds,
            "checkpoint_due_after_seconds": supervisor_config.checkpoint_due_seconds,
            "heartbeat_interval_seconds": supervisor_config.heartbeat_interval_seconds,
            "binding_gate": {
                "required": true,
                "expected_binding_source": "env",
                "wrapper_session": child.call_id,
                "status": "pending_hook"
            },
            "required_next_step": "inspect_board_or_report",
        });
        upsert_route_record(state, record)?;
        let done = Arc::new(AtomicBool::new(false));
        start_heartbeat(
            Arc::clone(&state_for_thread),
            route_id.clone(),
            invocation_id.clone(),
            Arc::clone(&done),
            supervisor_config.clone(),
        );
        thread::spawn(move || {
            let progress_state = Arc::clone(&state_for_thread);
            let progress_invocation = invocation_for_thread.clone();
            let progress_route_id = route_id.clone();
            let progress = Arc::new(move |update: Value| {
                let is_denial =
                    update.get("kind").and_then(Value::as_str) == Some("permission_denied");
                record_progress(&progress_state, &progress_invocation, update.clone());
                if is_denial {
                    crate::state::append_agent_event(
                        &progress_state,
                        "acp.permission_denied",
                        "ACP permission request denied by SOP policy.",
                        json!({
                            "route_id": progress_route_id.clone(),
                            "invocation_id": progress_invocation.clone(),
                            "runtime": "acp",
                            "tool": update.get("tool").cloned().unwrap_or(Value::Null),
                            "paths": update.get("paths").cloned().unwrap_or_else(|| json!([])),
                        }),
                    );
                }
            });
            let result = run_acp_invocation(AcpInvocation {
                command: command.clone(),
                cwd: cwd.clone(),
                wrapper_session: child_call_id.clone(),
                mode: mode_id,
                prompt,
                timeout_seconds,
                permission_policy: Some(policy),
                progress: Some(progress),
            });
            done.store(true, Ordering::SeqCst);
            match result {
                Ok(result) => finish_acp_route(
                    &state_for_thread,
                    &route_id,
                    &invocation_for_thread,
                    lifecycle_for_thread,
                    context_for_thread,
                    Some(result),
                    None,
                ),
                Err(err) => finish_acp_route(
                    &state_for_thread,
                    &route_id,
                    &invocation_for_thread,
                    lifecycle_for_thread,
                    context_for_thread,
                    None,
                    Some(err),
                ),
            }
        });
    } else {
        record.status = "acp_command_not_configured".to_string();
        record.result = json!({
            "runtime": "acp",
            "invocation_id": invocation_id,
            "adapter": "rust_native_acp",
            "workspace": workspace,
            "cwd": cwd,
            "context_packet": context_packet,
            "binding_gate": {
                "required": true,
                "expected_binding_source": "env",
                "wrapper_session": child.call_id,
                "status": "no_acp_command"
            },
            "hard_timeout_seconds": timeout_seconds,
            "checkpoint_due_after_seconds": supervisor_config.checkpoint_due_seconds,
            "heartbeat_interval_seconds": supervisor_config.heartbeat_interval_seconds,
            "message": "Set AGENTCALL_ACP_COMMAND or pass adapter_command to run native ACP.",
        });
        crate::state::append_agent_event(
            state,
            "agent.state_changed",
            "ACP child failed because ACP command is not configured.",
            merge_json(
                lifecycle_base.clone(),
                json!({"state": "failed", "agent": "rust_native_acp", "error": "acp_command_not_configured"}),
            ),
        );
    }
    crate::state::append_agent_event(
        state,
        "route.acp_invocation",
        "ACP route invocation recorded by daemon.",
        json!({"route_id": record.route_id, "invocation_id": invocation_id, "status": record.status}),
    );
    Ok(())
}

fn finish_acp_route(
    state: &Arc<AppState>,
    route_id: &str,
    invocation_id: &str,
    lifecycle_base: Value,
    context_packet: Value,
    result: Option<Value>,
    error: Option<String>,
) {
    if let Some(result) = result {
        let report_text = report_text_from_path_or_result(&context_packet, &result);
        let report = extract_report_validation(&report_text);
        let result_summary = acp_result_summary(&result);
        let acp_status = result["status"].as_str().unwrap_or("completed");
        let lifecycle_state = if acp_status == "completed"
            && report
                .get("valid")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            "completed"
        } else {
            "failed_report_contract"
        };
        let report_contract_status = if lifecycle_state == "completed" {
            "valid"
        } else {
            "failed_report_contract"
        };
        record_finished(
            state,
            invocation_id,
            lifecycle_state,
            report_contract_status,
            merge_json(result_summary.clone(), json!({"report": report.clone()})),
        );
        let _ = patch_route_record(
            state,
            route_id,
            json!({
                "status": lifecycle_state,
                "updated_at": now_ms(),
                "result": {
                    "sop_status": lifecycle_state,
                    "report": report,
                    "report_contract_status": report_contract_status,
                    "acp_result_summary": result_summary,
                    "checkpoint_due": false
                }
            }),
        );
        crate::state::append_agent_event(
            state,
            "child.report_received",
            "ACP child report received.",
            merge_json(
                lifecycle_base.clone(),
                json!({"status": lifecycle_state, "report": report.clone()}),
            ),
        );
        crate::state::append_agent_event(
            state,
            "agent.state_changed",
            "ACP child completed.",
            merge_json(
                lifecycle_base,
                json!({"state": lifecycle_state, "agent": "rust_native_acp", "invocation_id": invocation_id, "report": report.clone()}),
            ),
        );
    } else if let Some(error) = error {
        let lifecycle_state = if error.to_ascii_lowercase().contains("timed out") {
            "failed_timeout"
        } else {
            "failed"
        };
        record_finished(
            state,
            invocation_id,
            lifecycle_state,
            "not_available",
            json!({"error": error.clone()}),
        );
        let _ = patch_route_record(
            state,
            route_id,
            json!({
                "status": lifecycle_state,
                "updated_at": now_ms(),
                "result": {
                    "sop_status": lifecycle_state,
                    "report_contract_status": "not_available",
                    "error": error.clone(),
                    "checkpoint_due": false
                }
            }),
        );
        crate::state::append_agent_event(
            state,
            "child.report_received",
            "ACP child failed before report was received.",
            merge_json(
                lifecycle_base.clone(),
                json!({"status": lifecycle_state, "error": error}),
            ),
        );
        crate::state::append_agent_event(
            state,
            "agent.state_changed",
            "ACP child failed.",
            merge_json(
                lifecycle_base,
                json!({"state": lifecycle_state, "agent": "rust_native_acp", "invocation_id": invocation_id, "error": error}),
            ),
        );
    }
}

fn acp_result_summary(result: &Value) -> Value {
    json!({
        "status": result.get("status").and_then(Value::as_str),
        "stop_reason": result.get("stop_reason").and_then(Value::as_str),
        "update_count": result.get("update_count").and_then(Value::as_u64).unwrap_or(0),
        "process": result.get("process").cloned().unwrap_or(Value::Null),
        "session_id": result.get("session_id").and_then(Value::as_str),
    })
}

fn report_text_from_path_or_result(context_packet: &Value, result: &Value) -> String {
    if let Some(report_path) = context_packet.get("report_path").and_then(Value::as_str) {
        let path = PathBuf::from(report_path);
        if let Ok(text) = std::fs::read_to_string(&path) {
            return text;
        }
    }
    result
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
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
        || req.template.is_some()
        || req.target_files.is_some()
        || req.report_path.is_some()
        || req.max_reads.is_some()
        || req.max_writes.is_some()
}

fn merge_result_field(result: &mut Value, key: &str, value: Value) {
    if !result.is_object() {
        *result = json!({});
    }
    if let Some(object) = result.as_object_mut() {
        object.insert(key.to_string(), value);
    }
}

fn acp_command_from_env() -> Option<Vec<String>> {
    let value = std::env::var("AGENTCALL_ACP_COMMAND").ok()?;
    let parts: Vec<String> = value.split_whitespace().map(str::to_string).collect();
    if parts.is_empty() { None } else { Some(parts) }
}

fn acp_mode_id(req: &RouteRequest) -> String {
    match req.phase.as_deref().unwrap_or("execute") {
        "plan" | "review" => "plan".to_string(),
        _ => "acceptEdits".to_string(),
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

fn pty_prompt(req: &RouteRequest) -> String {
    let criteria = req
        .acceptance_criteria
        .as_ref()
        .map(|items| items.join("\n- "))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Complete the requested task and write a report.".to_string());
    let allowed = req
        .allowed_paths
        .as_ref()
        .map(|items| items.join("\n- "))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Use the task workspace carefully.".to_string());
    let workflow =
        PtyWorkflow::from_request(req.pty_workflow.as_deref()).unwrap_or(PtyWorkflow::PlanThenAuto);
    match workflow {
        PtyWorkflow::PlanThenAuto => format!(
            "AgentCall PTY handoff. Start in PLAN MODE.\n\nObjective:\n{}\n\nAllowed paths / ownership:\n- {}\n\nAcceptance criteria:\n- {}\n\nPlan-phase rules:\n- Inspect the code and write a concrete plan only.\n- If anything important is unclear, ask concise clarification questions instead of guessing.\n- Do not modify project files during plan phase.\n- When the plan is ready, use ExitPlanMode and wait for approval. After approval, continue in auto mode and write the requested report.\n",
            req.objective, allowed, criteria
        ),
        PtyWorkflow::Normal => format!(
            "AgentCall PTY handoff.\n\nObjective:\n{}\n\nAllowed paths / ownership:\n- {}\n\nAcceptance criteria:\n- {}\n\nWhen finished, write a report and stop at the prompt for review.\n",
            req.objective, allowed, criteria
        ),
    }
}

fn acp_prompt(req: &RouteRequest, context_packet: &Value) -> String {
    let task_id = context_packet["task_id"].as_str().unwrap_or("route-task");
    let call_id = context_packet["call_id"].as_str().unwrap_or("route-call");
    let role = context_packet["role"].as_str().unwrap_or("executor");
    let phase = context_packet["phase"].as_str().unwrap_or("execute");
    let allowed = context_packet["allowed_paths"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n- ")
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Entire workspace".to_string());
    let criteria = context_packet["acceptance_criteria"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n- ")
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Produce a valid report".to_string());
    let context_json =
        serde_json::to_string_pretty(context_packet).unwrap_or_else(|_| "{}".to_string());
    let template = context_packet["template"].as_str().unwrap_or("unknown");
    let target_files = context_packet["target_files"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n- ")
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "No target files supplied".to_string());
    let report_path = context_packet["report_path"].as_str().unwrap_or("");
    format!(
        "# AgentCall ACP Invocation: {call_id}\n\n\
Task: `{task_id}`\n\
Role: `{role}`\n\
Mode: `{phase}`\n\n\
## SOP Template\n\n`{template}`\n\n\
## Objective\n\n{}\n\n\
## Target Files\n\n- {target_files}\n\n\
## Writable Report Path\n\n`{report_path}`\n\n\
## Allowed Paths\n\n- {allowed}\n\n\
## Acceptance Criteria\n\n- {criteria}\n\n\
## Context Packet\n\n\
Use this packet as the authoritative project context for this lifecycle.\n\n\
```json\n{context_json}\n```\n\n\
## Mode Rules\n\n\
- This is an ACP SOP worker, not a free implementation runtime.\n\
- Read only the target files or allowed paths needed for evidence.\n\
- Write/Edit/MultiEdit is only allowed for the single report path above.\n\
- Do not modify implementation files.\n\
- Bash write, redirect, delete, move, and copy commands are forbidden.\n\
- Stop after producing the report; do not continue into another lifecycle.\n\n\
## Required Report Contract\n\n\
Return exactly one structured report at the report path and in final text when possible. It must include these fields: \
status, summary, verdict, evidence, files_read, changed_files, risks, next_recommended_action, context_sufficiency. \
`changed_files` must contain only the report path. \
`context_sufficiency` must say whether the provided target files and criteria were enough.\n",
        req.objective
    )
}

fn merge_json(mut base: Value, patch: Value) -> Value {
    if let (Some(base), Some(patch)) = (base.as_object_mut(), patch.as_object()) {
        for (key, value) in patch {
            base.insert(key.clone(), value.clone());
        }
    }
    base
}

fn extract_report_validation(text: &str) -> Value {
    let required = [
        "status",
        "summary",
        "verdict",
        "evidence",
        "files_read",
        "changed_files",
        "risks",
        "next_recommended_action",
        "context_sufficiency",
    ];
    if let Some(report) = extract_json_object(text) {
        let missing: Vec<&str> = required
            .iter()
            .copied()
            .filter(|field| report.get(*field).is_none())
            .collect();
        return json!({
            "format": "json",
            "valid": missing.is_empty(),
            "validation_status": if missing.is_empty() { "valid" } else { "missing_fields" },
            "missing_fields": missing,
            "report": report,
        });
    }

    let lower = text.to_ascii_lowercase();
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|field| !lower.contains(field))
        .collect();
    let looks_like_report = lower.contains("status:") || lower.contains("##");
    json!({
        "format": if looks_like_report { "markdown" } else { "none" },
        "valid": looks_like_report && missing.is_empty(),
        "validation_status": if looks_like_report && missing.is_empty() { "valid" } else { "missing_fields" },
        "missing_fields": missing,
        "text_excerpt": text.chars().take(1200).collect::<String>(),
    })
}

fn extract_json_object(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for index in start..bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    let candidate = &text[start..=index];
                    if let Ok(value) = serde_json::from_str::<Value>(candidate) {
                        return Some(value);
                    }
                    return None;
                }
            }
            _ => {}
        }
    }
    None
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
    fn auto_route_requires_sop_contract() {
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
                adapter_command: None,
                timeout_seconds: None,
                task_id: None,
                call_id: None,
                phase: None,
                role: None,
                allowed_paths: None,
                acceptance_criteria: None,
                persist_context: None,
                template: None,
                target_files: None,
                report_path: None,
                max_reads: None,
                max_writes: None,
                pty_workflow: None,
                initial_permission_mode: None,
            },
        )
        .unwrap();
        assert_eq!(route["status"], "needs_contract");
        assert_eq!(route["recommended_runtime"], "needs_contract");
    }

    #[test]
    fn forced_acp_route_is_daemon_recorded_without_configured_command() {
        let workspace = test_workspace("forced-acp");
        let state = Arc::new(AppState::test(workspace.clone()));
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
                allowed_paths: Some(vec![workspace.display().to_string()]),
                acceptance_criteria: Some(vec!["produce report".to_string()]),
                persist_context: None,
                template: Some("read-and-report".to_string()),
                target_files: Some(vec![workspace.join("README.md").display().to_string()]),
                report_path: Some(
                    workspace
                        .join(".agentcall/reports/forced_acp.md")
                        .display()
                        .to_string(),
                ),
                max_reads: None,
                max_writes: Some(1),
                pty_workflow: None,
                initial_permission_mode: None,
            },
        )
        .unwrap();
        assert_eq!(route["recommended_runtime"], "acp");
        assert_eq!(route["status"], "acp_command_not_configured");
        assert_eq!(route["result"]["adapter"], "rust_native_acp");
        assert!(workspace.join(".agentcall/state/routes.json").exists());
    }

    #[test]
    fn forced_acp_route_uses_configured_claude_workspace_as_cwd() {
        let workspace = test_workspace("forced-acp-cwd");
        let claude_workspace = test_workspace("forced-acp-claude-cwd");
        let state = Arc::new(AppState::new(
            workspace.clone(),
            LocalConfig {
                claude_workspace: Some(claude_workspace.clone()),
                acp_command: None,
                ..LocalConfig::default()
            },
            None,
        ));
        let route = handle_route(
            &state,
            RouteRequest {
                objective: "bounded review".to_string(),
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
                task_id: None,
                call_id: None,
                phase: None,
                role: None,
                allowed_paths: Some(vec![workspace.display().to_string()]),
                acceptance_criteria: Some(vec!["produce report".to_string()]),
                persist_context: None,
                template: Some("read-and-report".to_string()),
                target_files: Some(vec!["E:/GameProject/GGMYS/README.md".to_string()]),
                report_path: Some(
                    workspace
                        .join(".agentcall/reports/forced_acp_cwd.md")
                        .display()
                        .to_string(),
                ),
                max_reads: None,
                max_writes: Some(1),
                pty_workflow: None,
                initial_permission_mode: None,
            },
        )
        .unwrap();

        assert_eq!(route["result"]["workspace"], "E:/GameProject/GGMYS");
        assert_eq!(
            route["result"]["cwd"],
            claude_workspace.display().to_string()
        );
    }

    #[test]
    fn forced_acp_route_defaults_child_identity_and_projects_lifecycle() {
        let workspace = test_workspace("forced-acp-child-identity");
        let state = Arc::new(AppState::test(workspace.clone()));
        let route = handle_route(
            &state,
            RouteRequest {
                objective: "bounded review".to_string(),
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
                task_id: None,
                call_id: None,
                phase: None,
                role: None,
                allowed_paths: Some(vec![workspace.display().to_string()]),
                acceptance_criteria: Some(vec!["produce report".to_string()]),
                persist_context: None,
                template: Some("read-and-report".to_string()),
                target_files: Some(vec!["E:/GameProject/GGMYS/README.md".to_string()]),
                report_path: Some(
                    workspace
                        .join(".agentcall/reports/forced_acp_child.md")
                        .display()
                        .to_string(),
                ),
                max_reads: None,
                max_writes: Some(1),
                pty_workflow: None,
                initial_permission_mode: None,
            },
        )
        .unwrap();

        let route_id = route["route_id"].as_str().unwrap();
        let invocation_id = route["invocation_id"].as_str().unwrap();
        assert_eq!(route["result"]["context_packet"]["task_id"], route_id);
        assert_eq!(route["result"]["context_packet"]["call_id"], invocation_id);
        assert_eq!(
            route["result"]["binding_gate"]["wrapper_session"],
            invocation_id
        );

        let events = std::fs::read_to_string(workspace.join(".agentcall/events.ndjson")).unwrap();
        assert!(events.contains(r#""type":"context.created""#));
        assert!(events.contains(r#""type":"child.call_started""#));
        assert!(events.contains(r#""type":"agent.state_changed""#));
        assert!(events.contains(r#""state":"running""#));
        assert!(events.contains(r#""state":"failed""#));
    }

    #[test]
    fn forced_acp_route_rejects_timeout_above_configured_cap() {
        let workspace = test_workspace("forced-acp-timeout-cap");
        let state = Arc::new(AppState::new(
            workspace.clone(),
            LocalConfig {
                claude_workspace: Some(workspace.clone()),
                acp_max_timeout_seconds: Some(1800),
                ..LocalConfig::default()
            },
            None,
        ));
        let err = handle_route(
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
                timeout_seconds: Some(1801),
                task_id: None,
                call_id: None,
                phase: None,
                role: None,
                allowed_paths: Some(vec![workspace.display().to_string()]),
                acceptance_criteria: Some(vec!["produce report".to_string()]),
                persist_context: None,
                template: Some("read-and-report".to_string()),
                target_files: Some(vec![workspace.join("README.md").display().to_string()]),
                report_path: Some(
                    workspace
                        .join(".agentcall/reports/timeout_cap.md")
                        .display()
                        .to_string(),
                ),
                max_reads: None,
                max_writes: Some(1),
                pty_workflow: None,
                initial_permission_mode: None,
            },
        )
        .unwrap_err();
        assert!(err.contains("acp_max_timeout_seconds (1800)"));
    }

    #[test]
    fn forced_acp_route_rejects_when_active_capacity_is_full() {
        let workspace = test_workspace("forced-acp-capacity");
        let state = Arc::new(AppState::test(workspace.clone()));
        record_started(
            &state,
            AcpInvocationStart {
                route_id: "route-existing".to_string(),
                invocation_id: "acp-existing".to_string(),
                task_id: "task".to_string(),
                call_id: "call".to_string(),
                workspace: workspace.clone(),
                cwd: workspace.clone(),
                template: "read-and-report".to_string(),
                report_path: workspace.join("existing.md").display().to_string(),
                command: vec!["fake-acp".to_string()],
                hard_timeout_seconds: 1800,
                checkpoint_due_after_seconds: 600,
                heartbeat_interval_seconds: 60,
            },
        )
        .unwrap();
        record_started(
            &state,
            AcpInvocationStart {
                route_id: "route-existing-2".to_string(),
                invocation_id: "acp-existing-2".to_string(),
                task_id: "task".to_string(),
                call_id: "call".to_string(),
                workspace: workspace.clone(),
                cwd: workspace.clone(),
                template: "read-and-report".to_string(),
                report_path: workspace.join("existing-2.md").display().to_string(),
                command: vec!["fake-acp".to_string()],
                hard_timeout_seconds: 1800,
                checkpoint_due_after_seconds: 600,
                heartbeat_interval_seconds: 60,
            },
        )
        .unwrap();
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
                allowed_paths: Some(vec![workspace.display().to_string()]),
                acceptance_criteria: Some(vec!["produce report".to_string()]),
                persist_context: None,
                template: Some("read-and-report".to_string()),
                target_files: Some(vec![workspace.join("README.md").display().to_string()]),
                report_path: Some(
                    workspace
                        .join(".agentcall/reports/capacity.md")
                        .display()
                        .to_string(),
                ),
                max_reads: None,
                max_writes: Some(1),
                pty_workflow: None,
                initial_permission_mode: None,
            },
        )
        .unwrap();
        assert_eq!(route["status"], "acp_capacity_exceeded");
        assert_eq!(route["result"]["max_active_invocations"], 2);
    }

    #[test]
    fn report_validation_accepts_markdown_report_contract() {
        let report = extract_report_validation(
            "```yaml\nstatus: done\nsummary: ok\nverdict: pass\nevidence: []\nfiles_read: []\nchanged_files: []\nrisks: []\nnext_recommended_action: none\ncontext_sufficiency: {status: sufficient}\n```",
        );
        assert_eq!(report["format"], "markdown");
        assert_eq!(report["valid"], true);
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
                template: Some("read-and-report".to_string()),
                target_files: Some(vec!["src/lib.rs".to_string()]),
                report_path: Some("src/report.md".to_string()),
                max_reads: Some(10),
                max_writes: Some(1),
                pty_workflow: None,
                initial_permission_mode: None,
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
            adapter_command: None,
            timeout_seconds: None,
            task_id: None,
            call_id: None,
            phase: None,
            role: None,
            allowed_paths: None,
            acceptance_criteria: None,
            persist_context: None,
            template: None,
            target_files: None,
            report_path: None,
            max_reads: None,
            max_writes: None,
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
