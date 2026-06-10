use crate::routes::{patch_route_record_locked, route_for_wrapper_session};
use crate::state::{
    AppState, append_agent_event, append_agent_event_locked, read_json_file, write_json_file,
};
use crate::store::ReportIndexRecord;
use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Deserialize)]
pub(crate) struct HookIngestRequest {
    event: String,
    payload: serde_json::Value,
    runtime: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct EventAppendRequest {
    event_type: String,
    message: Option<String>,
    data: Option<serde_json::Value>,
    idempotency_key: Option<String>,
}

pub(crate) fn file_claims_state(state: &AppState) -> serde_json::Value {
    read_json_file(
        &state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("file_claims.json"),
        serde_json::json!({}),
    )
}

pub(crate) fn unmatched_hooks_state(state: &AppState) -> serde_json::Value {
    read_json_file(
        &state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("unmatched_hooks.json"),
        serde_json::json!([]),
    )
}

pub(crate) fn runtime_bindings_state(state: &AppState) -> serde_json::Value {
    read_json_file(
        &state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("runtime_binding.json"),
        serde_json::json!({}),
    )
}

pub(crate) fn pending_supervisor_instructions_state(state: &AppState) -> serde_json::Value {
    read_json_file(
        &state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("pending_supervisor_instructions.json"),
        serde_json::json!({}),
    )
}

pub(crate) fn policy_denials_state(state: &AppState) -> serde_json::Value {
    read_json_file(
        &state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("policy_denials.json"),
        serde_json::json!({}),
    )
}

pub(crate) fn cleanup_wrapper_session(
    state: &AppState,
    wrapper_session: &str,
    reason: &str,
) -> Result<serde_json::Value, String> {
    let _guard = state.state_writer.lock().unwrap();
    let agent_dir = state.workspace.join(".agentcall");
    let state_dir = agent_dir.join("state");
    fs::create_dir_all(&state_dir).map_err(|err| err.to_string())?;

    let bindings = read_json_file(
        &state_dir.join("runtime_binding.json"),
        serde_json::json!({}),
    );
    let hook_session_id = bindings
        .get(wrapper_session)
        .and_then(|binding| binding.get("claude_session_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let mut session_ids = vec![wrapper_session.to_string()];
    if let Some(hook_session_id) = hook_session_id {
        session_ids.push(hook_session_id);
    }

    let released_claims = release_claims_for_session_ids_locked(&state_dir, &session_ids)?;
    let cancelled_instructions =
        cancel_pending_supervisor_instructions_locked(&state_dir, wrapper_session, reason)?;

    if let Some((route_id, _route)) = route_for_wrapper_session(state, wrapper_session) {
        patch_route_record_locked(
            state,
            &agent_dir,
            &route_id,
            serde_json::json!({
                "status": reason,
                "updated_at": crate::util::now_ms(),
                "required_next_step": "inspect_report_or_restart_worker",
                "result": {
                    "workflow_status": reason,
                    "session_end_cleanup": {
                        "reason": reason,
                        "released_claims": released_claims,
                        "cancelled_pending_instructions": cancelled_instructions
                    }
                }
            }),
        )?;
    }

    append_agent_event_locked(
        state,
        &agent_dir,
        "session.cleanup",
        "Session runtime state cleaned up.",
        serde_json::json!({
            "wrapper_session": wrapper_session,
            "reason": reason,
            "session_ids": session_ids,
            "released_claims": released_claims,
            "cancelled_pending_instructions": cancelled_instructions,
        }),
    )?;

    Ok(serde_json::json!({
        "wrapper_session": wrapper_session,
        "reason": reason,
        "released_claims": released_claims,
        "cancelled_pending_instructions": cancelled_instructions
    }))
}

pub(crate) fn queue_supervisor_instruction(
    state: &AppState,
    wrapper_session: &str,
    action: &str,
    text: &str,
) -> Result<serde_json::Value, String> {
    let _guard = state.state_writer.lock().unwrap();
    let agent_dir = state.workspace.join(".agentcall");
    let state_dir = agent_dir.join("state");
    fs::create_dir_all(&state_dir).map_err(|err| err.to_string())?;
    let queued = queue_supervisor_instruction_locked(&state_dir, wrapper_session, action, text)?;
    append_agent_event_locked(
        state,
        &agent_dir,
        "supervisor_instruction.queued",
        "Supervisor instruction queued for hook injection.",
        serde_json::json!({
            "wrapper_session": wrapper_session,
            "action": action,
            "delivery": "next_hook_context_injection",
            "instruction_id": queued.get("id").cloned().unwrap_or(serde_json::Value::Null)
        }),
    )?;
    Ok(queued)
}

pub(crate) fn ingest_hook(
    state: &AppState,
    req: HookIngestRequest,
) -> Result<serde_json::Value, String> {
    let _guard = state.state_writer.lock().unwrap();
    let agent_dir = state.workspace.join(".agentcall");
    let state_dir = agent_dir.join("state");
    fs::create_dir_all(&state_dir).map_err(|err| err.to_string())?;

    let mut payload = req.payload;
    let session_id = session_id_from_payload(&payload);
    let unmatched = session_id.is_none();
    let session_id = session_id.unwrap_or_else(|| fallback_session_id(&payload));
    let tool_name = string_field(&payload, &["tool_name", "toolName"]);
    let workspace = string_field(&payload, &["workspace", "cwd"]);
    let transcript_path = string_field(&payload, &["transcript_path"]);
    let status = infer_hook_status(&req.event, &payload);
    let runtime = req
        .runtime
        .clone()
        .unwrap_or_else(|| "claude-code-session".to_string());
    let env_wrapper_session = string_field(&payload, &["wrapper_session", "wrapperSession"]);
    let (wrapper_session, binding_source) = upsert_runtime_binding_locked(
        &state_dir,
        env_wrapper_session.as_deref(),
        &session_id,
        transcript_path.as_deref(),
        workspace.as_deref(),
        &req.event,
        &status,
        tool_name.as_deref(),
    )?;
    let context_injection = if is_context_injection_event(&req.event) {
        Some(context_injection(
            state,
            &runtime,
            &state_dir,
            wrapper_session.as_deref(),
        )?)
    } else {
        None
    };
    payload["session_id"] = serde_json::json!(session_id.clone());
    payload["binding_source"] = serde_json::json!(binding_source.clone());
    if let Some(wrapper_session) = &wrapper_session {
        payload["wrapper_session"] = serde_json::json!(wrapper_session);
    }

    if unmatched {
        append_unmatched_hook_locked(&state_dir, &req.event, &session_id, &payload)?;
    }

    let decision = apply_hook_policy_locked(
        state,
        &state_dir,
        &req.event,
        &session_id,
        tool_name.as_deref(),
        &payload,
        wrapper_session.as_deref(),
    )?;
    if req.event == "UserPromptSubmit" {
        if let Some(wrapper) = wrapper_session.as_deref() {
            mark_route_prompt_submitted_locked(state, &agent_dir, wrapper)?;
        }
    }

    upsert_active_session_locked(
        &state_dir,
        &session_id,
        serde_json::json!({
            "session_id": session_id,
            "runtime": runtime,
            "status": status,
            "agent": string_field(&payload, &["agent", "agent_name"]).unwrap_or_else(|| "claude-code".to_string()),
            "pid": payload.get("pid").cloned().unwrap_or(serde_json::Value::Null),
            "transcript_path": transcript_path,
            "workspace": workspace,
            "wrapper_session": wrapper_session,
            "binding_source": binding_source,
            "updated_at": chrono::Utc::now().to_rfc3339(),
            "last_hook_event": req.event,
            "last_tool": tool_name,
        }),
    )?;

    append_agent_event_locked(
        state,
        &agent_dir,
        &format!("hook.{}", req.event),
        &format!("Claude Code hook received: {}", req.event),
        serde_json::json!({
            "hook_event": req.event,
            "session_id": session_id,
            "status": status,
            "tool_name": tool_name,
            "workspace": workspace,
            "transcript_path": transcript_path,
            "wrapper_session": wrapper_session,
            "binding_source": binding_source,
            "raw": payload,
            "decision": decision,
        }),
    )?;

    let mut response = serde_json::json!({
        "event_type": format!("hook.{}", req.event),
        "session_id": session_id,
        "status": status,
        "wrapper_session": wrapper_session,
        "binding_source": binding_source,
        "decision": decision,
        "unmatched": unmatched
    });
    if let Some(context_injection) = context_injection.filter(|value| !value.trim().is_empty()) {
        response["context_injection"] = serde_json::json!(context_injection);
    }
    Ok(response)
}

fn mark_route_prompt_submitted_locked(
    state: &AppState,
    agent_dir: &Path,
    wrapper_session: &str,
) -> Result<(), String> {
    let Some((route_id, route)) = route_for_wrapper_session(state, wrapper_session) else {
        return Ok(());
    };
    let status = route
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if !matches!(
        status,
        "started_pending_prompt_ack"
            | "started_pending_prompt_dispatch"
            | "started_prompt_dispatched_without_hook_ack"
            | "recommended"
            | "started"
    ) {
        return Ok(());
    }
    let observed_at = chrono::Utc::now().to_rfc3339();
    patch_route_record_locked(
        state,
        agent_dir,
        &route_id,
        serde_json::json!({
            "status": "prompt_submitted",
            "updated_at": crate::util::now_ms(),
            "required_next_step": "inspect_session_summary",
            "result": {
                "workflow_status": "running",
                "prompt": {
                    "acknowledged": true,
                    "ack_observed": "hook.UserPromptSubmit",
                    "ack_observed_at": observed_at
                },
                "prompt_gate": {
                    "acknowledged": true,
                    "ack_observed": "hook.UserPromptSubmit",
                    "ack_observed_at": observed_at
                }
            }
        }),
    )
}

fn is_context_injection_event(event: &str) -> bool {
    matches!(event, "SessionStart" | "UserPromptSubmit" | "PostToolBatch")
}

fn context_injection(
    state: &AppState,
    runtime: &str,
    state_dir: &Path,
    wrapper_session: Option<&str>,
) -> Result<String, String> {
    let sessions = read_json_file(
        &state_dir.join("active_sessions.json"),
        serde_json::json!({}),
    );
    let claims = read_json_file(&state_dir.join("file_claims.json"), serde_json::json!({}));
    let active_sessions = sessions.as_object().map(|items| items.len()).unwrap_or(0);
    let active_claims = claims
        .as_object()
        .map(|items| {
            items
                .values()
                .filter(|claim| {
                    claim.get("status").and_then(|value| value.as_str()) == Some("active")
                })
                .count()
        })
        .unwrap_or(0);
    let structured_reports = count_reports(&state.workspace.join(".agentcall"));
    let mut context = format!(
        "# AgentCall Context\n\n- runtime: {runtime}\n- workspace: {}\n- active_sessions: {active_sessions}\n- active_file_claims: {active_claims}\n- structured_reports: {structured_reports}\n\nAgentCall discipline:\n- Inspect the board before delegation or handoff.\n- Use AgentCall PTY utility workers for child work.\n- Respect allowed_paths and file claims; do not write outside assigned ownership.\n- Require a concise report or exact change summary at lifecycle end.\n- Write review only for drift, blockers, failed validation, or revision.\n",
        state.workspace.display()
    );
    if let Some(wrapper) = wrapper_session {
        inject_policy_guidance_locked(state, state_dir, &mut context, wrapper)?;
        let pending = take_pending_supervisor_instructions_locked(state_dir, wrapper)?;
        if !pending.is_empty() {
            let agent_dir = state_dir
                .parent()
                .unwrap_or_else(|| Path::new(".agentcall"));
            append_agent_event_locked(
                state,
                agent_dir,
                "supervisor_instruction.injected",
                "Queued supervisor instruction injected through hook context.",
                serde_json::json!({
                    "wrapper_session": wrapper,
                    "count": pending.len(),
                    "delivery": "hook.additionalContext"
                }),
            )?;
            mark_supervisor_instruction_injected_locked(state_dir, wrapper)?;
            context.push_str("\n# AgentCall Supervisor Update\n\n");
            context.push_str(
                "The supervisor sent these instructions while you were busy. Apply them before continuing. If they conflict with current work, stop and report the conflict clearly.\n\n",
            );
            for (index, item) in pending.iter().enumerate() {
                let action = item
                    .get("action")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("send");
                let text = item
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                context.push_str(&format!("{}. [{action}] {text}\n", index + 1));
            }
        }
    }
    Ok(context)
}

fn mark_supervisor_instruction_injected_locked(
    state_dir: &Path,
    wrapper_session: &str,
) -> Result<(), String> {
    let path = state_dir.join("runtime_binding.json");
    let mut bindings = read_json_file(&path, serde_json::json!({}));
    if let Some(binding) = bindings.get_mut(wrapper_session) {
        binding["last_supervisor_instruction_injected_at"] =
            serde_json::json!(chrono::Utc::now().to_rfc3339());
    }
    write_json_file(&path, &bindings)
}

fn count_reports(agent_dir: &Path) -> usize {
    let mut count = 0usize;
    count_report_files(&agent_dir.join("tasks"), &mut count);
    count_report_files(&agent_dir.join("reports"), &mut count);
    count
}

fn count_report_files(path: &Path, count: &mut usize) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            count_report_files(&path, count);
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.eq_ignore_ascii_case("report.md")
            || name.starts_with("Report_")
            || name.ends_with(".report.json")
        {
            *count += 1;
        }
    }
}

fn queue_supervisor_instruction_locked(
    state_dir: &Path,
    wrapper_session: &str,
    action: &str,
    text: &str,
) -> Result<serde_json::Value, String> {
    let path = state_dir.join("pending_supervisor_instructions.json");
    let mut pending = read_json_file(&path, serde_json::json!({}));
    if !pending.is_object() {
        pending = serde_json::json!({});
    }
    let id = format!(
        "instr-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    );
    let item = serde_json::json!({
        "id": id,
        "wrapper_session": wrapper_session,
        "action": action,
        "text": text,
        "status": "pending_hook_injection",
        "created_at": chrono::Utc::now().to_rfc3339(),
    });
    if !pending
        .get(wrapper_session)
        .and_then(serde_json::Value::as_array)
        .is_some()
    {
        pending[wrapper_session] = serde_json::json!([]);
    }
    pending[wrapper_session]
        .as_array_mut()
        .unwrap()
        .push(item.clone());
    write_json_file(&path, &pending)?;
    Ok(item)
}

fn take_pending_supervisor_instructions_locked(
    state_dir: &Path,
    wrapper_session: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let path = state_dir.join("pending_supervisor_instructions.json");
    let mut pending = read_json_file(&path, serde_json::json!({}));
    if !pending.is_object() {
        return Ok(vec![]);
    }
    let items = pending
        .get_mut(wrapper_session)
        .and_then(serde_json::Value::as_array_mut)
        .map(std::mem::take)
        .unwrap_or_default();
    if let Some(object) = pending.as_object_mut() {
        object.remove(wrapper_session);
    }
    write_json_file(&path, &pending)?;
    Ok(items)
}

pub(crate) fn append_event_request(
    state: &AppState,
    req: EventAppendRequest,
) -> Result<serde_json::Value, String> {
    let idempotency_key = req
        .idempotency_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "write command rejected: idempotency_key is required".to_string())?;
    let mut data = req.data.unwrap_or_else(|| serde_json::json!({}));
    if !data.is_object() {
        data = serde_json::json!({"payload": data});
    }
    data["idempotency_key"] = serde_json::json!(idempotency_key);
    append_agent_event(
        state,
        &req.event_type,
        req.message.as_deref().unwrap_or(""),
        data,
    );
    Ok(serde_json::json!({"ok": true, "idempotency_key": idempotency_key}))
}

pub(crate) fn append_unmatched_hook_locked(
    state_dir: &Path,
    event: &str,
    session_id: &str,
    payload: &serde_json::Value,
) -> Result<(), String> {
    let path = state_dir.join("unmatched_hooks.json");
    let mut items = read_json_file(&path, serde_json::json!([]));
    if !items.is_array() {
        items = serde_json::json!([]);
    }
    items.as_array_mut().unwrap().push(serde_json::json!({
        "event": event,
        "fallback_session_id": session_id,
        "payload": payload,
        "observed_at": chrono::Utc::now().to_rfc3339(),
    }));
    write_json_file(&path, &items)
}

pub(crate) fn upsert_active_session_locked(
    state_dir: &Path,
    session_id: &str,
    session: serde_json::Value,
) -> Result<(), String> {
    let path = state_dir.join("active_sessions.json");
    let mut sessions = read_json_file(&path, serde_json::json!({}));
    if !sessions.is_object() {
        sessions = serde_json::json!({});
    }
    sessions[session_id] = session;
    write_json_file(&path, &sessions)
}

pub(crate) fn upsert_runtime_binding_locked(
    state_dir: &Path,
    env_wrapper_session: Option<&str>,
    claude_session_id: &str,
    transcript_path: Option<&str>,
    cwd: Option<&str>,
    event: &str,
    status: &str,
    tool_name: Option<&str>,
) -> Result<(Option<String>, String), String> {
    let path = state_dir.join("runtime_binding.json");
    let mut bindings = read_json_file(&path, serde_json::json!({}));
    if !bindings.is_object() {
        bindings = serde_json::json!({});
    }
    let env_wrapper_session = env_wrapper_session.filter(|value| !value.trim().is_empty());
    let wrapper_session = env_wrapper_session
        .map(|value| value.to_string())
        .or_else(|| find_known_wrapper_binding(&bindings, claude_session_id, transcript_path));
    let Some(wrapper_session) = wrapper_session else {
        return Ok((None, "unbound".to_string()));
    };
    let binding_source = if env_wrapper_session.is_some() {
        "env"
    } else {
        "known_session"
    };
    let previous = bindings
        .get(&wrapper_session)
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let mut seen_hooks = previous
        .get("seen_hooks")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if !seen_hooks.is_object() {
        seen_hooks = serde_json::json!({});
    }
    let now = chrono::Utc::now().to_rfc3339();
    seen_hooks[event] = serde_json::json!(true);
    let mut binding = serde_json::json!({
        "wrapper_session": wrapper_session.clone(),
        "claude_session_id": claude_session_id,
        "transcript_path": transcript_path,
        "cwd": cwd,
        "last_hook_event": event,
        "last_hook_status": status,
        "last_tool": tool_name,
        "last_seen": now,
        "seen_hooks": seen_hooks,
        "binding_source": binding_source,
    });
    if matches!(
        event,
        "PostToolBatch" | "UserPromptSubmit" | "PreToolUse" | "PostToolUse"
    ) {
        binding[&format!("last_{}_at", event.to_ascii_lowercase())] = binding["last_seen"].clone();
    }
    if let Some(value) = previous.get("last_supervisor_instruction_injected_at") {
        binding["last_supervisor_instruction_injected_at"] = value.clone();
    }
    bindings[&wrapper_session] = binding;
    write_json_file(&path, &bindings)?;
    Ok((Some(wrapper_session), binding_source.to_string()))
}

pub(crate) fn find_known_wrapper_binding(
    bindings: &serde_json::Value,
    claude_session_id: &str,
    transcript_path: Option<&str>,
) -> Option<String> {
    let object = bindings.as_object()?;
    object.iter().find_map(|(wrapper, binding)| {
        let session_match = binding
            .get("claude_session_id")
            .and_then(|value| value.as_str())
            == Some(claude_session_id);
        let transcript_match = transcript_path.is_some()
            && binding
                .get("transcript_path")
                .and_then(|value| value.as_str())
                == transcript_path;
        if session_match || transcript_match {
            Some(wrapper.clone())
        } else {
            None
        }
    })
}

pub(crate) fn apply_hook_policy_locked(
    state: &AppState,
    state_dir: &Path,
    event: &str,
    session_id: &str,
    tool_name: Option<&str>,
    payload: &serde_json::Value,
    wrapper_session: Option<&str>,
) -> Result<serde_json::Value, String> {
    match event {
        "PreToolUse" => pre_tool_use_claim_locked(
            state,
            state_dir,
            session_id,
            tool_name,
            payload,
            wrapper_session,
        ),
        "PostToolUse" => post_tool_use_observe_locked(
            state,
            state_dir,
            session_id,
            tool_name,
            payload,
            wrapper_session,
        ),
        "Stop" | "SubagentStop" | "SessionEnd" => {
            release_claims_locked(state, state_dir, session_id)
        }
        _ => Ok(serde_json::Value::Null),
    }
}

pub(crate) fn pre_tool_use_claim_locked(
    state: &AppState,
    state_dir: &Path,
    session_id: &str,
    tool_name: Option<&str>,
    payload: &serde_json::Value,
    wrapper_session: Option<&str>,
) -> Result<serde_json::Value, String> {
    let tool_name = tool_name.unwrap_or("");
    if let Some(wrapper) = wrapper_session {
        if let Some(decision) = binding_trust_policy_decision(
            state,
            state_dir,
            Some(wrapper),
            tool_name,
            payload,
        )? {
            return Ok(decision);
        }
        if let Some(decision) =
            pty_plan_policy_decision(state, state_dir, wrapper, tool_name, payload)?
        {
            return Ok(decision);
        }
        if let Some(policy) = pty_path_policy_for_wrapper(state, wrapper) {
            if let Some(denial) = pty_path_policy_denial(tool_name, payload, &policy) {
                let policy_block = record_policy_denial_locked(
                    state, state_dir, wrapper, tool_name, payload, &denial, &policy,
                )?;
                return Ok(serde_json::json!({
                    "allowed": false,
                    "reason": denial,
                    "policy": {
                        "runtime": "pty",
                        "mode": policy.mode.clone(),
                        "allowed_paths": policy.allowed_paths.clone(),
                        "writable_paths": policy.writable_paths.clone(),
                        "writable_roots": policy_writable_roots_json(&policy),
                        "scratch_path": policy.scratch_path.clone(),
                        "roots": policy_roots_json(&policy),
                        "bash_write_policy": policy.bash_write_policy.clone()
                    },
                    "policy_block": policy_block,
                    "files": extract_tool_files(payload),
                    "conflicts": []
                }));
            }
            reset_policy_denials_for_wrapper_locked(state_dir, wrapper)?;
        }
    } else if let Some(decision) =
        binding_trust_policy_decision(state, state_dir, None, tool_name, payload)?
    {
        return Ok(decision);
    }
    if !is_write_tool(tool_name) {
        return Ok(
            serde_json::json!({"allowed": true, "reason": "tool does not claim files", "files": [], "conflicts": []}),
        );
    }
    let files = extract_tool_files(payload);
    if files.is_empty() {
        return Ok(
            serde_json::json!({"allowed": true, "reason": "write tool did not expose file path", "files": [], "conflicts": []}),
        );
    }
    let path = state_dir.join("file_claims.json");
    let mut claims = read_json_file(&path, serde_json::json!({}));
    if !claims.is_object() {
        claims = serde_json::json!({});
    }
    mark_expired_claims_stale(&mut claims);
    let mut conflicts = vec![];
    for file in &files {
        let claim = &claims[file];
        if claim.get("status").and_then(|value| value.as_str()) == Some("active")
            && claim.get("session_id").and_then(|value| value.as_str()) != Some(session_id)
        {
            conflicts.push(serde_json::json!({"file": file, "claim": claim}));
        }
    }
    if !conflicts.is_empty() {
        write_json_file(&path, &claims)?;
        return Ok(
            serde_json::json!({"allowed": false, "reason": "file claim conflict", "files": files, "conflicts": conflicts}),
        );
    }
    let now = chrono::Utc::now();
    let expires = now + chrono::Duration::minutes(5);
    for file in &files {
        let old_claimed_at = claims[file]
            .get("claimed_at")
            .cloned()
            .unwrap_or_else(|| serde_json::json!(now.to_rfc3339()));
        claims[file] = serde_json::json!({
            "file": file,
            "session_id": session_id,
            "tool_name": tool_name,
            "last_tool_name": tool_name,
            "status": "active",
            "claimed_at": old_claimed_at,
            "updated_at": now.to_rfc3339(),
            "expires_at": expires.to_rfc3339(),
            "owner_pid": payload.get("pid").cloned().unwrap_or(serde_json::Value::Null),
            "workspace": string_field(payload, &["workspace", "cwd"]).unwrap_or_else(|| state.workspace.display().to_string()),
            "project": state.workspace.file_name().and_then(|name| name.to_str()).unwrap_or("workspace"),
            "transcript_path": string_field(payload, &["transcript_path"]),
        });
    }
    write_json_file(&path, &claims)?;
    Ok(
        serde_json::json!({"allowed": true, "reason": "file claims acquired", "files": files, "conflicts": []}),
    )
}

pub(crate) fn post_tool_use_observe_locked(
    state: &AppState,
    state_dir: &Path,
    session_id: &str,
    tool_name: Option<&str>,
    payload: &serde_json::Value,
    wrapper_session: Option<&str>,
) -> Result<serde_json::Value, String> {
    let tool_name = tool_name.unwrap_or("");
    let files = extract_tool_files(payload);
    if files.is_empty() {
        return Ok(
            serde_json::json!({"allowed": true, "reason": "no file paths observed", "files": [], "conflicts": []}),
        );
    }
    if !is_write_tool(tool_name) {
        return Ok(
            serde_json::json!({"allowed": true, "reason": "read observed", "files": files, "conflicts": []}),
        );
    }
    let path = state_dir.join("file_claims.json");
    let mut claims = read_json_file(&path, serde_json::json!({}));
    if !claims.is_object() {
        claims = serde_json::json!({});
    }
    let now = chrono::Utc::now().to_rfc3339();
    for file in &files {
        if claims[file]
            .get("session_id")
            .and_then(|value| value.as_str())
            == Some(session_id)
        {
            claims[file]["updated_at"] = serde_json::json!(now);
            claims[file]["last_tool_name"] = serde_json::json!(tool_name);
        }
    }
    write_json_file(&path, &claims)?;
    let report_write =
        wrapper_session.and_then(|wrapper| report_write_for_wrapper(state, wrapper, &files));
    if let Some((wrapper, route_id, report_path)) = report_write {
        let agent_dir = state_dir
            .parent()
            .unwrap_or_else(|| Path::new(".agentcall"));
        patch_route_record_locked(
            state,
            agent_dir,
            &route_id,
            serde_json::json!({
                "status": "report_ready",
                "updated_at": crate::util::now_ms(),
                "required_next_step": "accept_report_or_stop_worker",
                "result": {
                    "workflow_status": "report_ready",
                    "report_ready": true,
                    "report_path": report_path,
                    "report_source": "hook_write"
                }
            }),
        )?;
        let _ = state.store.save_report_index(&ReportIndexRecord {
            report_id: format!("report-{}", stable_hash_hex(report_path.as_bytes())),
            session_id: Some(wrapper.clone()),
            path: report_path.clone(),
            status: "ready".to_string(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        });
        return Ok(serde_json::json!({
            "allowed": true,
            "reason": "write observed",
            "files": files,
            "conflicts": [],
            "report_ready": true,
            "report_path": report_path,
            "report_source": "hook_write",
            "wrapper_session": wrapper,
            "route_id": route_id
        }));
    }
    Ok(
        serde_json::json!({"allowed": true, "reason": "write observed", "files": files, "conflicts": []}),
    )
}

pub(crate) fn release_claims_locked(
    _state: &AppState,
    state_dir: &Path,
    session_id: &str,
) -> Result<serde_json::Value, String> {
    let released = release_claims_for_session_ids_locked(state_dir, &[session_id.to_string()])?;
    Ok(
        serde_json::json!({"allowed": true, "reason": "session claims released", "files": released, "conflicts": []}),
    )
}

fn release_claims_for_session_ids_locked(
    state_dir: &Path,
    session_ids: &[String],
) -> Result<Vec<String>, String> {
    let path = state_dir.join("file_claims.json");
    let mut claims = read_json_file(&path, serde_json::json!({}));
    if !claims.is_object() {
        claims = serde_json::json!({});
    }
    let mut released = vec![];
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(items) = claims.as_object_mut() {
        for (file, claim) in items.iter_mut() {
            let claim_session = claim.get("session_id").and_then(|value| value.as_str());
            if claim_session.is_some_and(|value| session_ids.iter().any(|id| id == value))
                && claim.get("status").and_then(|value| value.as_str()) == Some("active")
            {
                claim["status"] = serde_json::json!("released");
                claim["released_at"] = serde_json::json!(now);
                released.push(file.clone());
            }
        }
    }
    write_json_file(&path, &claims)?;
    Ok(released)
}

fn cancel_pending_supervisor_instructions_locked(
    state_dir: &Path,
    wrapper_session: &str,
    reason: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let path = state_dir.join("pending_supervisor_instructions.json");
    let mut pending = read_json_file(&path, serde_json::json!({}));
    if !pending.is_object() {
        return Ok(vec![]);
    }
    let mut items = pending
        .get_mut(wrapper_session)
        .and_then(serde_json::Value::as_array_mut)
        .map(std::mem::take)
        .unwrap_or_default();
    for item in &mut items {
        item["status"] = serde_json::json!("cancelled_session_ended");
        item["cancelled_at"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
        item["cancel_reason"] = serde_json::json!(reason);
    }
    if let Some(object) = pending.as_object_mut() {
        object.remove(wrapper_session);
    }
    write_json_file(&path, &pending)?;
    Ok(items)
}

pub(crate) fn mark_expired_claims_stale(claims: &mut serde_json::Value) {
    let now = chrono::Utc::now();
    let Some(items) = claims.as_object_mut() else {
        return;
    };
    for claim in items.values_mut() {
        if claim.get("status").and_then(|value| value.as_str()) != Some("active") {
            continue;
        }
        let Some(expires_at) = claim.get("expires_at").and_then(|value| value.as_str()) else {
            continue;
        };
        let Ok(expires) = chrono::DateTime::parse_from_rfc3339(expires_at) else {
            continue;
        };
        if expires.with_timezone(&chrono::Utc) <= now {
            claim["status"] = serde_json::json!("stale");
            claim["stale_at"] = serde_json::json!(now.to_rfc3339());
        }
    }
}

fn pty_plan_policy_decision(
    state: &AppState,
    state_dir: &Path,
    wrapper_session: &str,
    tool_name: &str,
    payload: &serde_json::Value,
) -> Result<Option<serde_json::Value>, String> {
    let Some((route_id, route)) = route_for_wrapper_session(state, wrapper_session) else {
        return Ok(None);
    };
    if route
        .get("recommended_runtime")
        .and_then(serde_json::Value::as_str)
        != Some("pty")
    {
        return Ok(None);
    }
    let result = route.get("result").unwrap_or(&serde_json::Value::Null);
    if result
        .get("pty_workflow")
        .and_then(serde_json::Value::as_str)
        != Some("plan_then_auto")
    {
        return Ok(None);
    }
    let phase = result
        .get("phase")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("plan");
    if phase != "plan" {
        return Ok(None);
    }
    if tool_name == "ExitPlanMode" {
        let agent_dir = state_dir
            .parent()
            .unwrap_or_else(|| Path::new(".agentcall"));
        patch_route_record_locked(
            state,
            agent_dir,
            &route_id,
            serde_json::json!({
                "status": "plan_ready",
                "updated_at": crate::util::now_ms(),
                "result": {
                    "workflow_status": "plan_ready",
                    "phase": "plan",
                    "permission_mode": "plan",
                    "mode_source": "hook",
                    "last_plan_ready_at": chrono::Utc::now().to_rfc3339()
                }
            }),
        )?;
        return Ok(Some(serde_json::json!({
            "allowed": true,
            "reason": "plan ready; waiting for explicit approve_plan/start_auto",
            "route_id": route_id,
            "files": extract_tool_files(payload),
            "conflicts": []
        })));
    }
    if tool_name == "Bash" {
        if bash_readonly_allowed(payload) {
            return Ok(None);
        }
        return Ok(Some(serde_json::json!({
            "allowed": false,
            "reason": "plan phase denies non-read-only bash command",
            "route_id": route_id,
            "files": extract_tool_files(payload),
            "conflicts": []
        })));
    }
    if is_write_tool(tool_name) {
        let files = extract_tool_files(payload);
        if files.iter().all(|file| is_claude_plan_file(file)) {
            return Ok(None);
        }
        return Ok(Some(serde_json::json!({
            "allowed": false,
            "reason": "plan phase denies project file writes before approve_plan/start_auto",
            "route_id": route_id,
            "files": files,
            "conflicts": []
        })));
    }
    Ok(None)
}

fn binding_trust_policy_decision(
    state: &AppState,
    state_dir: &Path,
    wrapper_session: Option<&str>,
    tool_name: &str,
    payload: &serde_json::Value,
) -> Result<Option<serde_json::Value>, String> {
    if !tool_requires_trusted_binding(tool_name, payload) {
        return Ok(None);
    }
    if let Some(wrapper) = wrapper_session {
        if route_for_wrapper_session(state, wrapper).is_none() {
            return Ok(None);
        }
        if runtime_binding_trusted_for_wrapper(state_dir, wrapper) {
            return Ok(None);
        }
        return Ok(Some(binding_untrusted_denial(
            wrapper_session,
            tool_name,
            payload,
            "route worker write requires trusted hook binding",
        )));
    }
    if any_active_route_requires_binding(state_dir) {
        return Ok(Some(binding_untrusted_denial(
            None,
            tool_name,
            payload,
            "unbound hook cannot perform write or non-read-only bash while AgentCall route binding is required",
        )));
    }
    Ok(None)
}

fn tool_requires_trusted_binding(tool_name: &str, payload: &serde_json::Value) -> bool {
    is_write_tool(tool_name) || (tool_name == "Bash" && !bash_readonly_allowed(payload))
}

fn runtime_binding_trusted_for_wrapper(state_dir: &Path, wrapper_session: &str) -> bool {
    let bindings = read_json_file(
        &state_dir.join("runtime_binding.json"),
        serde_json::json!({}),
    );
    bindings
        .get(wrapper_session)
        .and_then(|binding| binding.get("binding_source"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|source| matches!(source, "env" | "known_session"))
}

fn any_active_route_requires_binding(state_dir: &Path) -> bool {
    let routes = read_json_file(&state_dir.join("routes.json"), serde_json::json!({}));
    let Some(routes) = routes.as_object() else {
        return false;
    };
    routes.values().any(route_requires_binding)
}

fn route_requires_binding(route: &serde_json::Value) -> bool {
    if route
        .get("recommended_runtime")
        .and_then(serde_json::Value::as_str)
        != Some("pty")
    {
        return false;
    }
    let required = route
        .pointer("/result/binding_gate/required")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !required {
        return false;
    }
    !route_status_is_terminal(
        route.get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(""),
    )
}

fn route_status_is_terminal(status: &str) -> bool {
    status.starts_with("failed")
        || matches!(
            status,
            "completed"
                | "cancelled"
                | "stopped"
                | "killed"
                | "report_accepted"
                | "closed"
                | "orphaned"
        )
}

fn binding_untrusted_denial(
    wrapper_session: Option<&str>,
    tool_name: &str,
    payload: &serde_json::Value,
    reason: &str,
) -> serde_json::Value {
    serde_json::json!({
        "allowed": false,
        "reason": "binding_untrusted",
        "details": reason,
        "wrapper_session": wrapper_session,
        "binding_required": true,
        "tool": tool_name,
        "files": extract_tool_files(payload),
        "conflicts": [],
        "recommended_action": "restart_worker_or_fix_hook_env_binding"
    })
}

#[derive(Clone)]
struct PtyPathPolicy {
    allowed_paths: Vec<String>,
    writable_paths: Vec<String>,
    writable_roots: Vec<WritableRoot>,
    mode: String,
    scratch_path: Option<String>,
    process_cwd: Option<String>,
    target_workspace: Option<String>,
    scratch_root: Option<String>,
    bash_write_policy: String,
}

#[derive(Clone)]
struct WritableRoot {
    kind: String,
    display: String,
    abs: String,
}

fn pty_path_policy_for_wrapper(state: &AppState, wrapper_session: &str) -> Option<PtyPathPolicy> {
    let (_route_id, route) = route_for_wrapper_session(state, wrapper_session)?;
    if route
        .get("recommended_runtime")
        .and_then(serde_json::Value::as_str)
        != Some("pty")
    {
        return None;
    }
    let result = route.get("result")?;
    let containment = result.get("containment")?;
    let mut allowed_paths = string_array(
        result
            .get("containment")
            .and_then(|containment| containment.get("allowed_paths")),
    );
    let mut writable_paths = string_array(containment.get("writable_paths"));
    if allowed_paths.is_empty() {
        allowed_paths = string_array(
            result
                .get("context_packet")
                .and_then(|packet| packet.get("allowed_paths")),
        );
    }
    if writable_paths.is_empty() {
        writable_paths = allowed_paths.clone();
    }
    let writable_roots = writable_roots_from_containment(containment);
    let roots = containment.get("roots");
    if allowed_paths.is_empty() && writable_paths.is_empty() && writable_roots.is_empty() {
        return None;
    }
    Some(PtyPathPolicy {
        allowed_paths,
        writable_paths,
        writable_roots,
        mode: containment
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("enforced")
            .to_string(),
        scratch_path: containment
            .get("scratch_path")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        process_cwd: roots
            .and_then(|roots| roots.get("process_cwd"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        target_workspace: roots
            .and_then(|roots| roots.get("target_workspace"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        scratch_root: roots
            .and_then(|roots| roots.get("scratch_root"))
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                containment
                    .get("scratch_root")
                    .and_then(serde_json::Value::as_str)
            })
            .map(str::to_string),
        bash_write_policy: containment
            .get("bash_write_policy")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("readonly_only")
            .to_string(),
    })
}

fn writable_roots_from_containment(containment: &serde_json::Value) -> Vec<WritableRoot> {
    containment
        .get("writable_roots")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let abs = item.get("abs").and_then(serde_json::Value::as_str)?;
                    Some(WritableRoot {
                        kind: item
                            .get("kind")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("writable_root")
                            .to_string(),
                        display: item
                            .get("display")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or(abs)
                            .to_string(),
                        abs: abs.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn pty_path_policy_denial(
    tool_name: &str,
    payload: &serde_json::Value,
    policy: &PtyPathPolicy,
) -> Option<String> {
    if tool_name == "TaskCreate" && policy.mode == "read_only" {
        return Some(
            "PTY path policy denies TaskCreate during read-only route; report missing context instead"
                .to_string(),
        );
    }
    if is_write_tool(tool_name) {
        let files = extract_tool_files(payload);
        if files.is_empty() {
            return Some(
                "PTY path policy denies write tool without explicit file path".to_string(),
            );
        }
        if files
            .iter()
            .all(|file| write_path_allowed_by_policy(file, policy))
        {
            return None;
        }
        return Some(
            "PTY path policy denies write outside allowed_paths or writable_paths".to_string(),
        );
    }
    if tool_name == "Bash" && !bash_readonly_allowed(payload) {
        return Some(
            "PTY path policy denies non-read-only bash when allowed_paths are enforced".to_string(),
        );
    }
    None
}

fn write_path_allowed_by_policy(file: &str, policy: &PtyPathPolicy) -> bool {
    policy
        .writable_roots
        .iter()
        .any(|root| path_within_or_equal(file, &root.abs))
        || policy
            .writable_paths
            .iter()
            .any(|allowed| path_within_or_equal(file, allowed))
}

const POLICY_DENIAL_THRESHOLD: u64 = 2;
const POLICY_DENIAL_WINDOW_SECONDS: i64 = 60;

fn record_policy_denial_locked(
    state: &AppState,
    state_dir: &Path,
    wrapper_session: &str,
    tool_name: &str,
    payload: &serde_json::Value,
    reason: &str,
    policy: &PtyPathPolicy,
) -> Result<serde_json::Value, String> {
    let path = state_dir.join("policy_denials.json");
    let mut denials = read_json_file(&path, serde_json::json!({}));
    if !denials.is_object() {
        denials = serde_json::json!({});
    }
    let now_dt = chrono::Utc::now();
    let now = now_dt.to_rfc3339();
    let normalized = normalized_policy_target(tool_name, payload);
    let key = format!(
        "{}:{}:{}",
        tool_name,
        stable_hash_hex(normalized.as_bytes()),
        stable_hash_hex(reason.as_bytes())
    );
    let previous = denials
        .get(wrapper_session)
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let previous_key = previous.get("key").and_then(serde_json::Value::as_str);
    let repeat_count = if previous_key == Some(key.as_str()) {
        previous
            .get("repeat_count")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
            + 1
    } else {
        1
    };
    let mut recent_denials = previous
        .get("recent_denials")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    recent_denials.retain(|item| {
        item.get("last_seen")
            .and_then(serde_json::Value::as_str)
            .and_then(|seen| chrono::DateTime::parse_from_rfc3339(seen).ok())
            .map(|seen| {
                now_dt
                    .signed_duration_since(seen.with_timezone(&chrono::Utc))
                    .num_seconds()
                    <= POLICY_DENIAL_WINDOW_SECONDS
            })
            .unwrap_or(false)
    });
    recent_denials.push(serde_json::json!({
        "key": key.clone(),
        "tool": tool_name,
        "target": normalized.clone(),
        "reason": reason,
        "last_seen": now.clone()
    }));
    let recent_denial_count = recent_denials.len() as u64;
    let active =
        repeat_count >= POLICY_DENIAL_THRESHOLD || recent_denial_count >= POLICY_DENIAL_THRESHOLD;
    let category = policy_denial_category(tool_name, reason);
    let path_diagnosis = policy_path_diagnosis(tool_name, payload, policy, "deny");
    let recommended_action = match category.as_str() {
        "missing_scratch_or_report_path" => "extend_allowed_paths_or_use_write_tool",
        _ => "interrupt_or_send_blocker_instruction",
    };
    let mut suggested_allowed_paths = vec![];
    if category == "missing_scratch_or_report_path" {
        if let Some(scratch) = &policy.scratch_path {
            suggested_allowed_paths.push(serde_json::json!(scratch));
        }
    }
    let block = serde_json::json!({
        "active": active,
        "key": key,
        "wrapper_session": wrapper_session,
        "tool": tool_name,
        "target": normalized,
        "reason": reason,
        "repeat_count": repeat_count,
        "recent_denial_count": recent_denial_count,
        "recent_denials": recent_denials,
        "window_seconds": POLICY_DENIAL_WINDOW_SECONDS,
        "threshold": POLICY_DENIAL_THRESHOLD,
        "category": category,
        "recommended_action": recommended_action,
        "path_diagnosis": path_diagnosis,
        "suggested_allowed_paths": suggested_allowed_paths,
        "guidance_injected": previous
            .get("guidance_injected")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
            && previous_key == Some(key.as_str()),
        "last_seen": now,
        "policy": {
            "mode": policy.mode.clone(),
            "allowed_paths": policy.allowed_paths.clone(),
            "writable_paths": policy.writable_paths.clone(),
            "writable_roots": policy_writable_roots_json(policy),
            "scratch_path": policy.scratch_path.clone(),
            "roots": policy_roots_json(policy),
            "bash_write_policy": policy.bash_write_policy.clone()
        }
    });
    denials[wrapper_session] = block.clone();
    write_json_file(&path, &denials)?;
    if active && previous.get("active").and_then(serde_json::Value::as_bool) != Some(true) {
        append_agent_event_locked(
            state,
            state_dir
                .parent()
                .unwrap_or_else(|| Path::new(".agentcall")),
            "policy_denial.blocked",
            "PTY worker is blocked by repeated policy denials.",
            serde_json::json!({
                "wrapper_session": wrapper_session,
                "policy_block": block
            }),
        )?;
    }
    Ok(block)
}

fn policy_path_diagnosis(
    tool_name: &str,
    payload: &serde_json::Value,
    policy: &PtyPathPolicy,
    decision_actual: &str,
) -> serde_json::Value {
    if !is_write_tool(tool_name) {
        return serde_json::Value::Null;
    }
    let Some(target) = extract_tool_files(payload).first().cloned() else {
        return serde_json::Value::Null;
    };
    diagnose_policy_path(&target, policy, decision_actual)
}

fn diagnose_policy_path(
    target: &str,
    policy: &PtyPathPolicy,
    decision_actual: &str,
) -> serde_json::Value {
    let normalized = normalize_compare_path(target);
    let matched_writable = policy
        .writable_roots
        .iter()
        .find(|root| path_within_or_equal(target, &root.abs));
    let (root_class, relative_to_root) = matched_writable
        .map(|root| {
            (
                root.kind.clone(),
                relative_path_for_diagnosis(&normalized, &root.abs),
            )
        })
        .or_else(|| {
            policy.scratch_root.as_ref().and_then(|root| {
                path_within_or_equal(target, root).then(|| {
                    (
                        "scratch_root".to_string(),
                        relative_path_for_diagnosis(&normalized, root),
                    )
                })
            })
        })
        .or_else(|| {
            policy.target_workspace.as_ref().and_then(|root| {
                path_within_or_equal(target, root).then(|| {
                    (
                        "target_workspace".to_string(),
                        relative_path_for_diagnosis(&normalized, root),
                    )
                })
            })
        })
        .or_else(|| {
            policy.process_cwd.as_ref().and_then(|root| {
                path_within_or_equal(target, root).then(|| {
                    (
                        "process_cwd".to_string(),
                        relative_path_for_diagnosis(&normalized, root),
                    )
                })
            })
        })
        .unwrap_or_else(|| ("outside_known_roots".to_string(), serde_json::Value::Null));
    let matched_writable_root = matched_writable
        .map(|root| root.kind.clone())
        .unwrap_or_else(|| "none".to_string());
    let decision_expected = if matched_writable_root == "none" {
        "deny"
    } else {
        "allow"
    };
    let diagnosis = if decision_expected == "allow" && decision_actual == "deny" {
        format!(
            "target is inside {matched_writable_root}; denial likely indicates policy normalization drift"
        )
    } else if decision_expected == "deny" {
        "target is outside writable roots for this route".to_string()
    } else {
        "policy decision matches writable roots".to_string()
    };
    serde_json::json!({
        "target": {
            "raw": target,
            "normalized": normalized,
            "root_class": root_class,
            "relative_to_root": relative_to_root
        },
        "roots": policy_roots_json(policy),
        "matched_writable_root": matched_writable_root,
        "decision_expected": decision_expected,
        "decision_actual": decision_actual,
        "diagnosis": diagnosis
    })
}

fn relative_path_for_diagnosis(normalized_target: &str, root: &str) -> serde_json::Value {
    let normalized_root = normalize_compare_path(root);
    normalized_target
        .strip_prefix(&(normalized_root + "/"))
        .map(|value| serde_json::json!(value))
        .unwrap_or_else(|| serde_json::Value::Null)
}

fn policy_writable_roots_json(policy: &PtyPathPolicy) -> serde_json::Value {
    serde_json::Value::Array(
        policy
            .writable_roots
            .iter()
            .map(|root| {
                serde_json::json!({
                    "kind": root.kind.clone(),
                    "display": root.display.clone(),
                    "abs": root.abs.clone()
                })
            })
            .collect(),
    )
}

fn policy_roots_json(policy: &PtyPathPolicy) -> serde_json::Value {
    serde_json::json!({
        "process_cwd": policy.process_cwd.clone(),
        "target_workspace": policy.target_workspace.clone(),
        "scratch_root": policy.scratch_root.clone()
    })
}

fn reset_policy_denials_for_wrapper_locked(
    state_dir: &Path,
    wrapper_session: &str,
) -> Result<(), String> {
    let path = state_dir.join("policy_denials.json");
    let mut denials = read_json_file(&path, serde_json::json!({}));
    if let Some(object) = denials.as_object_mut() {
        object.remove(wrapper_session);
    }
    write_json_file(&path, &denials)
}

fn report_write_for_wrapper(
    state: &AppState,
    wrapper_session: &str,
    files: &[String],
) -> Option<(String, String, String)> {
    let (route_id, route) = route_for_wrapper_session(state, wrapper_session)?;
    let report_path = route_report_path(&route)?;
    if files
        .iter()
        .any(|file| paths_equivalent_for_policy(&state.workspace, file, &report_path))
    {
        Some((wrapper_session.to_string(), route_id, report_path))
    } else {
        None
    }
}

fn route_report_path(route: &serde_json::Value) -> Option<String> {
    route
        .get("result")
        .and_then(|result| result.get("context_packet"))
        .and_then(|packet| packet.get("report_path"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| {
            route
                .get("result")
                .and_then(|result| result.get("report_path"))
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
        })
}

fn paths_equivalent_for_policy(workspace: &Path, left: &str, right: &str) -> bool {
    normalized_route_path(workspace, left) == normalized_route_path(workspace, right)
}

fn normalized_route_path(workspace: &Path, path: &str) -> String {
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace.join(path)
    };
    absolute
        .canonicalize()
        .unwrap_or(absolute)
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase()
}

fn inject_policy_guidance_locked(
    state: &AppState,
    state_dir: &Path,
    context: &mut String,
    wrapper_session: &str,
) -> Result<(), String> {
    let path = state_dir.join("policy_denials.json");
    let mut denials = read_json_file(&path, serde_json::json!({}));
    let Some(block) = denials.get_mut(wrapper_session) else {
        return Ok(());
    };
    if block.get("active").and_then(serde_json::Value::as_bool) != Some(true)
        || block
            .get("guidance_injected")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
    {
        return Ok(());
    }
    let reason = block
        .get("reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("policy denied the last tool call");
    let target = block
        .get("target")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let category = block
        .get("category")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("needs_supervisor_decision");
    context.push_str("\n# AgentCall Policy Block\n\n");
    context.push_str(
        "AgentCall denied the same tool action repeatedly. Do not retry the same action.\n",
    );
    context.push_str(&format!("- category: {category}\n"));
    context.push_str(&format!("- denied target: {target}\n"));
    context.push_str(&format!("- reason: {reason}\n"));
    context.push_str(
        "- next step: use existing references, use Write/Edit inside allowed scratch/report paths, or report this as a blocker for supervisor action.\n",
    );
    block["guidance_injected"] = serde_json::json!(true);
    block["guidance_injected_at"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    let event_block = block.clone();
    write_json_file(&path, &denials)?;
    append_agent_event_locked(
        state,
        state_dir
            .parent()
            .unwrap_or_else(|| Path::new(".agentcall")),
        "policy_denial.guidance_injected",
        "Policy denial guidance injected through hook context.",
        serde_json::json!({
            "wrapper_session": wrapper_session,
            "policy_block": event_block
        }),
    )
}

fn normalized_policy_target(tool_name: &str, payload: &serde_json::Value) -> String {
    if tool_name == "Bash" {
        let command = payload
            .get("tool_input")
            .or_else(|| payload.get("toolInput"))
            .and_then(|value| value.get("command"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        return command.split_whitespace().collect::<Vec<_>>().join(" ");
    }
    let files = extract_tool_files(payload);
    if files.is_empty() {
        tool_name.to_string()
    } else {
        files
            .iter()
            .map(|file| normalize_workspace_path(file))
            .collect::<Vec<_>>()
            .join("|")
    }
}

fn policy_denial_category(tool_name: &str, reason: &str) -> String {
    if reason.contains("without explicit file path") {
        "missing_scratch_or_report_path".to_string()
    } else if tool_name == "Bash" || reason.contains("outside") {
        "dangerous_or_out_of_scope".to_string()
    } else {
        "needs_supervisor_decision".to_string()
    }
}

fn stable_hash_hex(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn is_claude_plan_file(path: &str) -> bool {
    let normalized = normalize_workspace_path(path).to_ascii_lowercase();
    normalized.contains("/.claude/plans/") || normalized.starts_with(".claude/plans/")
}

fn bash_readonly_allowed(payload: &serde_json::Value) -> bool {
    let command = payload
        .get("tool_input")
        .or_else(|| payload.get("toolInput"))
        .and_then(|value| value.get("command"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if command.is_empty() {
        return false;
    }
    let forbidden = [
        ">",
        ">>",
        "| tee",
        "set-content",
        "out-file",
        "new-item",
        "remove-item",
        "del ",
        "erase ",
        "rm ",
        "move-item",
        "mv ",
        "copy-item",
        "cp ",
        "mkdir",
        "rmdir",
        "echo ",
    ];
    if forbidden.iter().any(|needle| command.contains(needle)) {
        return false;
    }
    let allowed = [
        "pwd",
        "cd",
        "ls",
        "dir",
        "cat ",
        "type ",
        "rg ",
        "findstr ",
        "git status",
        "git diff",
        "git show",
    ];
    allowed.iter().any(|prefix| command.starts_with(prefix))
}

fn string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn path_within_or_equal(path: &str, parent: &str) -> bool {
    let path = normalize_compare_path(path);
    let parent = normalize_compare_path(parent);
    path == parent || path.starts_with(&(parent + "/"))
}

fn normalize_compare_path(path: &str) -> String {
    normalize_workspace_path(path)
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

pub(crate) fn session_id_from_payload(payload: &serde_json::Value) -> Option<String> {
    string_field(payload, &["session_id", "sessionId", "agent_id"])
        .or_else(|| string_field(payload, &["transcript_path"]))
}

pub(crate) fn fallback_session_id(payload: &serde_json::Value) -> String {
    if let Some(path) = string_field(payload, &["transcript_path"]) {
        return format!("transcript:{}", stable_hash(&path));
    }
    format!("unmatched:{}", stable_hash(&payload.to_string()))
}

pub(crate) fn stable_hash(value: &str) -> u64 {
    let mut hash = 1469598103934665603u64;
    for byte in value.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

pub(crate) fn string_field(payload: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = payload.get(*key).and_then(|value| value.as_str()) {
            return Some(value.to_string());
        }
    }
    None
}

pub(crate) fn extract_tool_files(payload: &serde_json::Value) -> Vec<String> {
    let input = payload
        .get("tool_input")
        .or_else(|| payload.get("toolInput"))
        .and_then(|value| value.as_object());
    let Some(input) = input else {
        return vec![];
    };
    let mut files = vec![];
    for key in ["file_path", "path", "notebook_path"] {
        if let Some(value) = input.get(key).and_then(|value| value.as_str()) {
            files.push(normalize_workspace_path(value));
        }
    }
    files.sort();
    files.dedup();
    files
}

pub(crate) fn normalize_workspace_path(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_string();
    }
    normalized
}

pub(crate) fn is_write_tool(tool_name: &str) -> bool {
    matches!(tool_name, "Edit" | "MultiEdit" | "Write" | "NotebookEdit")
}

pub(crate) fn infer_hook_status(event: &str, payload: &serde_json::Value) -> String {
    if let Some(status) = payload.get("status").and_then(|value| value.as_str()) {
        return status.to_string();
    }
    match event {
        "PreToolUse"
            if string_field(payload, &["tool_name", "toolName"]).as_deref()
                == Some("ExitPlanMode") =>
        {
            "plan_ready".to_string()
        }
        "SessionStart" | "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolBatch" => {
            "working".to_string()
        }
        "Notification" => {
            let message = payload
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if message.contains("permission") {
                "needs_permission".to_string()
            } else if message.contains("idle") || message.contains("waiting") {
                "waiting_input".to_string()
            } else {
                "notified".to_string()
            }
        }
        "Stop" => "idle".to_string(),
        "SubagentStop" => {
            if payload.get("checkpoint_request").and_then(|value| value.as_bool()) == Some(true)
                || payload
                    .get("agentcall_checkpoint")
                    .and_then(|value| value.as_bool())
                    == Some(true)
            {
                "checkpoint_due".to_string()
            } else {
                "observed".to_string()
            }
        }
        "SessionEnd" => "completed".to_string(),
        _ => "observed".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::now_ms;
    use std::env;
    use std::sync::Arc;

    fn test_state(name: &str) -> Arc<AppState> {
        let root = env::temp_dir().join(format!(
            "agentcall-daemon-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".agentcall").join("state")).unwrap();
        Arc::new(AppState::test(root))
    }

    fn write_payload(session_id: &str, file: &str) -> serde_json::Value {
        serde_json::json!({
            "session_id": session_id,
            "tool_name": "Write",
            "tool_input": {"file_path": file},
            "cwd": "E:\\Project\\AgentCall"
        })
    }

    fn bash_payload(session_id: &str, command: &str) -> serde_json::Value {
        serde_json::json!({
            "session_id": session_id,
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "cwd": "E:\\Project\\AgentCall"
        })
    }

    fn install_pty_plan_route(state: &AppState, wrapper_session: &str) {
        let path = state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("routes.json");
        write_json_file(
            &path,
            &serde_json::json!({
                "route-pty": {
                    "route_id": "route-pty",
                    "recommended_runtime": "pty",
                    "session_name": wrapper_session,
                    "result": {
                        "pty_workflow": "plan_then_auto",
                        "workflow_status": "plan_running",
                        "phase": "plan",
                        "permission_mode": "plan",
                        "plan_session_name": wrapper_session
                    }
                }
            }),
        )
        .unwrap();
    }

    fn install_pty_auto_route(state: &AppState, wrapper_session: &str, allowed_paths: &[&str]) {
        let path = state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("routes.json");
        write_json_file(
            &path,
            &serde_json::json!({
                "route-pty": {
                    "route_id": "route-pty",
                    "recommended_runtime": "pty",
                    "session_name": wrapper_session,
                    "result": {
                        "pty_workflow": "normal",
                        "workflow_status": "running",
                        "phase": "execute",
                        "permission_mode": "auto",
                        "containment": {
                            "mode": "enforced",
                            "allowed_paths": allowed_paths
                        }
                    }
                }
            }),
        )
        .unwrap();
    }

    fn install_pty_report_route(
        state: &AppState,
        wrapper_session: &str,
        report_path: &str,
        read_only: bool,
    ) {
        let path = state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("routes.json");
        let mode = if read_only {
            "read_only"
        } else {
            "enforced_readonly_bash"
        };
        let scratch = format!(".agentcall/workspaces/{wrapper_session}");
        let writable_paths = if read_only {
            serde_json::json!([])
        } else {
            serde_json::json!([report_path, scratch])
        };
        write_json_file(
            &path,
            &serde_json::json!({
                "route-report": {
                    "route_id": "route-report",
                    "recommended_runtime": "pty",
                    "session_name": wrapper_session,
                    "status": "started",
                    "result": {
                        "pty_workflow": "normal",
                        "workflow_status": "running",
                        "phase": "execute",
                        "permission_mode": "auto",
                        "context_packet": {
                            "report_path": report_path,
                            "allowed_paths": ["docs/reports"]
                        },
                        "containment": {
                            "mode": mode,
                            "allowed_paths": ["docs/reports"],
                            "writable_paths": writable_paths,
                            "scratch_path": if read_only { serde_json::Value::Null } else { serde_json::json!(scratch) },
                            "bash_write_policy": "readonly_only"
                        }
                    }
                }
            }),
        )
        .unwrap();
    }

    #[test]
    fn append_event_request_requires_idempotency_key() {
        let state = test_state("append-event-idempotency");
        let missing = append_event_request(
            &state,
            EventAppendRequest {
                event_type: "manual.event".to_string(),
                message: Some("missing key".to_string()),
                data: Some(serde_json::json!({"session_id": "worker-a"})),
                idempotency_key: None,
            },
        )
        .unwrap_err();
        assert!(missing.contains("idempotency_key"));

        let accepted = append_event_request(
            &state,
            EventAppendRequest {
                event_type: "manual.event".to_string(),
                message: Some("has key".to_string()),
                data: Some(serde_json::json!({"session_id": "worker-a"})),
                idempotency_key: Some("cmd-123".to_string()),
            },
        )
        .unwrap();
        assert_eq!(accepted["ok"], true);

        let events = crate::state::read_events(&state.workspace.join(".agentcall/events.ndjson"));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["data"]["idempotency_key"], "cmd-123");
        assert_eq!(events[0]["session_key"], "worker-a");
    }

    #[test]
    fn session_start_returns_context_injection_but_tool_hooks_do_not() {
        let state = test_state("context-injection");
        let session_start = ingest_hook(
            &state,
            HookIngestRequest {
                event: "SessionStart".to_string(),
                payload: serde_json::json!({"session_id": "claude-a"}),
                runtime: Some("claude".to_string()),
            },
        )
        .unwrap();
        assert!(
            session_start["context_injection"]
                .as_str()
                .unwrap()
                .contains("AgentCall Context")
        );

        let pre_tool = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: write_payload("claude-a", "src/lib.rs"),
                runtime: Some("claude".to_string()),
            },
        )
        .unwrap();
        assert!(pre_tool.get("context_injection").is_none());
    }

    #[test]
    fn post_tool_batch_injects_queued_supervisor_instruction_once() {
        let state = test_state("pending-instruction");
        queue_supervisor_instruction(
            &state,
            "pty-a",
            "request_report",
            "Stop new implementation and write the report.",
        )
        .unwrap();

        let first = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PostToolBatch".to_string(),
                payload: serde_json::json!({
                    "session_id": "claude-a",
                    "wrapper_session": "pty-a"
                }),
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        let context = first["context_injection"].as_str().unwrap();
        assert!(context.contains("AgentCall Supervisor Update"));
        assert!(context.contains("Stop new implementation"));

        let second = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PostToolBatch".to_string(),
                payload: serde_json::json!({
                    "session_id": "claude-a",
                    "wrapper_session": "pty-a"
                }),
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        assert!(
            !second["context_injection"]
                .as_str()
                .unwrap()
                .contains("AgentCall Supervisor Update")
        );
    }

    #[test]
    fn pty_plan_phase_denies_project_file_writes() {
        let state = test_state("pty-plan-deny-write");
        install_pty_plan_route(&state, "pty-a");
        let mut payload = write_payload("claude-a", "src/lib.rs");
        payload["wrapper_session"] = serde_json::json!("pty-a");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload,
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        assert_eq!(result["decision"]["allowed"], false);
        assert!(
            result["decision"]["reason"]
                .as_str()
                .unwrap()
                .contains("plan phase denies")
        );
    }

    #[test]
    fn pty_plan_phase_allows_claude_plan_file_write() {
        let state = test_state("pty-plan-allow-plan-file");
        install_pty_plan_route(&state, "pty-a");
        let mut payload =
            write_payload("claude-a", "C:/Users/MUSHI/.claude/plans/agentcall-plan.md");
        payload["wrapper_session"] = serde_json::json!("pty-a");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload,
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        assert_eq!(result["decision"]["allowed"], true);
    }

    #[test]
    fn user_prompt_submit_marks_pending_route_prompt_submitted() {
        let state = test_state("pty-prompt-ack-route");
        let path = state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("routes.json");
        write_json_file(
            &path,
            &serde_json::json!({
                "route-pty": {
                    "route_id": "route-pty",
                    "recommended_runtime": "pty",
                    "session_name": "pty-a",
                    "status": "started_pending_prompt_ack",
                    "result": {
                        "pty_workflow": "normal",
                        "workflow_status": "starting",
                        "phase": "execute",
                        "prompt": {
                            "acknowledged": false,
                            "ack_expected": "hook.UserPromptSubmit"
                        },
                        "prompt_gate": {
                            "acknowledged": false,
                            "ack_expected": "hook.UserPromptSubmit"
                        }
                    }
                }
            }),
        )
        .unwrap();
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "UserPromptSubmit".to_string(),
                payload: serde_json::json!({
                    "session_id": "claude-a",
                    "wrapper_session": "pty-a",
                    "prompt": "do work"
                }),
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        assert_eq!(result["status"], "working");

        let routes = read_json_file(&path, serde_json::json!({}));
        assert_eq!(routes["route-pty"]["status"], "prompt_submitted");
        assert_eq!(
            routes["route-pty"]["result"]["prompt"]["ack_observed"],
            "hook.UserPromptSubmit"
        );
        assert_eq!(
            routes["route-pty"]["result"]["prompt_gate"]["acknowledged"],
            true
        );
    }

    #[test]
    fn pty_auto_route_denies_write_outside_allowed_paths() {
        let state = test_state("pty-auto-deny-outside");
        install_pty_auto_route(&state, "pty-a", &["src"]);
        let mut payload = write_payload("claude-a", "docs/report.md");
        payload["wrapper_session"] = serde_json::json!("pty-a");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload,
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        assert_eq!(result["decision"]["allowed"], false);
        assert!(
            result["decision"]["reason"]
                .as_str()
                .unwrap()
                .contains("outside allowed_paths")
        );
    }

    #[test]
    fn pty_auto_route_allows_write_inside_allowed_paths() {
        let state = test_state("pty-auto-allow-inside");
        install_pty_auto_route(&state, "pty-a", &["src"]);
        let mut payload = write_payload("claude-a", "src/lib.rs");
        payload["wrapper_session"] = serde_json::json!("pty-a");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload,
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        assert_eq!(result["decision"]["allowed"], true);
    }

    #[test]
    fn pty_route_direct_policy_denies_untrusted_wrapper_binding() {
        let state = test_state("pty-untrusted-wrapper-binding");
        install_pty_auto_route(&state, "pty-a", &["src"]);
        let state_dir = state.workspace.join(".agentcall").join("state");
        let payload = write_payload("claude-a", "src/lib.rs");
        let result = pre_tool_use_claim_locked(
            &state,
            &state_dir,
            "claude-a",
            Some("Write"),
            &payload,
            Some("pty-a"),
        )
        .unwrap();
        assert_eq!(result["allowed"], false);
        assert_eq!(result["reason"], "binding_untrusted");
    }

    #[test]
    fn active_route_denies_unbound_write_until_hook_env_binds() {
        let state = test_state("pty-unbound-write-denied");
        let path = state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("routes.json");
        write_json_file(
            &path,
            &serde_json::json!({
                "route-pty": {
                    "route_id": "route-pty",
                    "recommended_runtime": "pty",
                    "session_name": "pty-a",
                    "status": "started_pending_prompt_ack",
                    "result": {
                        "binding_gate": {
                            "required": true,
                            "expected_binding_source": "env",
                            "status": "pending_hook"
                        },
                        "containment": {
                            "mode": "enforced_readonly_bash",
                            "allowed_paths": ["src"],
                            "writable_paths": ["src"],
                            "bash_write_policy": "readonly_only"
                        }
                    }
                }
            }),
        )
        .unwrap();
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: write_payload("claude-a", "src/lib.rs"),
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        assert_eq!(result["decision"]["allowed"], false);
        assert_eq!(result["decision"]["reason"], "binding_untrusted");
    }

    #[test]
    fn pty_auto_route_allows_write_inside_writable_scratch() {
        let state = test_state("pty-auto-allow-scratch");
        let path = state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("routes.json");
        write_json_file(
            &path,
            &serde_json::json!({
                "route-pty": {
                    "route_id": "route-pty",
                    "recommended_runtime": "pty",
                    "session_name": "pty-a",
                    "result": {
                        "pty_workflow": "normal",
                        "workflow_status": "running",
                        "phase": "execute",
                        "permission_mode": "auto",
                        "containment": {
                            "mode": "enforced_readonly_bash",
                            "allowed_paths": ["src"],
                            "writable_paths": [".agentcall/workspaces/pty-a", "docs/report.md"],
                            "scratch_path": ".agentcall/workspaces/pty-a",
                            "bash_write_policy": "readonly_only"
                        }
                    }
                }
            }),
        )
        .unwrap();
        let mut payload = write_payload("claude-a", ".agentcall/workspaces/pty-a/tmp.md");
        payload["wrapper_session"] = serde_json::json!("pty-a");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload,
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        assert_eq!(result["decision"]["allowed"], true);
    }

    #[test]
    fn pty_auto_route_allows_write_inside_absolute_scratch_root() {
        let state = test_state("pty-auto-allow-absolute-scratch");
        let scratch_abs = state.workspace.join(".agentcall/workspaces/pty-a");
        let path = state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("routes.json");
        write_json_file(
            &path,
            &serde_json::json!({
                "route-pty": {
                    "route_id": "route-pty",
                    "recommended_runtime": "pty",
                    "session_name": "pty-a",
                    "result": {
                        "pty_workflow": "normal",
                        "workflow_status": "running",
                        "phase": "execute",
                        "permission_mode": "auto",
                        "containment": {
                            "mode": "enforced_readonly_bash",
                            "allowed_paths": ["src"],
                            "writable_paths": [".agentcall/workspaces/pty-a", "docs/report.md"],
                            "scratch_path": ".agentcall/workspaces/pty-a",
                            "roots": {
                                "process_cwd": state.workspace.display().to_string(),
                                "target_workspace": state.workspace.display().to_string(),
                                "scratch_root": scratch_abs.display().to_string()
                            },
                            "writable_roots": [
                                {
                                    "kind": "scratch",
                                    "display": ".agentcall/workspaces/pty-a",
                                    "abs": scratch_abs.display().to_string()
                                }
                            ],
                            "bash_write_policy": "readonly_only"
                        }
                    }
                }
            }),
        )
        .unwrap();
        let mut payload = write_payload(
            "claude-a",
            &scratch_abs.join("tmp.md").display().to_string(),
        );
        payload["wrapper_session"] = serde_json::json!("pty-a");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload,
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        assert_eq!(result["decision"]["allowed"], true);
    }

    #[test]
    fn policy_denial_records_path_diagnosis_with_materialized_roots() {
        let state = test_state("pty-policy-path-diagnosis");
        let scratch_abs = state.workspace.join(".agentcall/workspaces/pty-a");
        let path = state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("routes.json");
        write_json_file(
            &path,
            &serde_json::json!({
                "route-pty": {
                    "route_id": "route-pty",
                    "recommended_runtime": "pty",
                    "session_name": "pty-a",
                    "result": {
                        "pty_workflow": "normal",
                        "workflow_status": "running",
                        "phase": "execute",
                        "permission_mode": "auto",
                        "containment": {
                            "mode": "enforced_readonly_bash",
                            "allowed_paths": [],
                            "writable_paths": [],
                            "scratch_path": ".agentcall/workspaces/pty-a",
                            "roots": {
                                "process_cwd": state.workspace.display().to_string(),
                                "target_workspace": state.workspace.display().to_string(),
                                "scratch_root": scratch_abs.display().to_string()
                            },
                            "writable_roots": [
                                {
                                    "kind": "scratch",
                                    "display": ".agentcall/workspaces/pty-a",
                                    "abs": scratch_abs.display().to_string()
                                }
                            ],
                            "bash_write_policy": "readonly_only"
                        }
                    }
                }
            }),
        )
        .unwrap();
        let mut payload = write_payload(
            "claude-a",
            &state.workspace.join("src/lib.rs").display().to_string(),
        );
        payload["wrapper_session"] = serde_json::json!("pty-a");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload,
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        assert_eq!(result["decision"]["allowed"], false);
        let diagnosis = &result["decision"]["policy_block"]["path_diagnosis"];
        assert_eq!(diagnosis["target"]["root_class"], "target_workspace");
        assert_eq!(diagnosis["decision_expected"], "deny");
        assert_eq!(diagnosis["decision_actual"], "deny");
        assert_eq!(
            diagnosis["roots"]["target_workspace"],
            state.workspace.display().to_string()
        );
    }

    #[test]
    fn pty_read_only_route_denies_task_create_drift() {
        let state = test_state("pty-readonly-taskcreate-deny");
        install_pty_report_route(&state, "pty-a", "docs/reports/review.md", true);
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: serde_json::json!({
                    "session_id": "claude-a",
                    "wrapper_session": "pty-a",
                    "tool_name": "TaskCreate",
                    "tool_input": {"description": "spawn reviewer"}
                }),
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        assert_eq!(result["decision"]["allowed"], false);
        assert!(
            result["decision"]["reason"]
                .as_str()
                .unwrap()
                .contains("read-only route")
        );
    }

    #[test]
    fn post_tool_report_write_marks_route_and_projection_ready() {
        let state = test_state("pty-report-ready");
        install_pty_report_route(&state, "pty-a", "docs/reports/review.md", false);
        let mut payload = write_payload("claude-a", "docs/reports/review.md");
        payload["wrapper_session"] = serde_json::json!("pty-a");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PostToolUse".to_string(),
                payload,
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        assert_eq!(result["decision"]["report_ready"], true);
        assert_eq!(result["decision"]["route_id"], "route-report");

        let routes = read_json_file(
            &state
                .workspace
                .join(".agentcall")
                .join("state")
                .join("routes.json"),
            serde_json::json!({}),
        );
        assert_eq!(routes["route-report"]["status"], "report_ready");
        assert_eq!(
            routes["route-report"]["result"]["workflow_status"],
            "report_ready"
        );
        assert_eq!(routes["route-report"]["result"]["report_ready"], true);

        let projection = crate::projection::read_session_projection(&state, "pty-a").unwrap();
        assert!(projection.report_ready);
        assert_eq!(projection.attention_status, "report_ready");
        assert_eq!(
            projection.next_recommended_action,
            "accept_report_or_stop_worker"
        );
    }

    #[test]
    fn repeated_policy_denial_creates_block_and_injects_guidance_once() {
        let state = test_state("pty-policy-loop");
        install_pty_auto_route(&state, "pty-a", &["src"]);
        for _ in 0..2 {
            let mut payload = bash_payload(
                "claude-a",
                "git clone https://github.com/Holic75/KingmakerRebalance.git --depth 1",
            );
            payload["wrapper_session"] = serde_json::json!("pty-a");
            let result = ingest_hook(
                &state,
                HookIngestRequest {
                    event: "PreToolUse".to_string(),
                    payload,
                    runtime: Some("claude-code-session".to_string()),
                },
            )
            .unwrap();
            assert_eq!(result["decision"]["allowed"], false);
        }
        let denials = policy_denials_state(&state);
        assert_eq!(denials["pty-a"]["active"], true);
        assert_eq!(denials["pty-a"]["repeat_count"], 2);
        assert_eq!(denials["pty-a"]["category"], "dangerous_or_out_of_scope");

        let first = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PostToolBatch".to_string(),
                payload: serde_json::json!({
                    "session_id": "claude-a",
                    "wrapper_session": "pty-a"
                }),
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        assert!(
            first["context_injection"]
                .as_str()
                .unwrap()
                .contains("AgentCall Policy Block")
        );
        let second = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PostToolBatch".to_string(),
                payload: serde_json::json!({
                    "session_id": "claude-a",
                    "wrapper_session": "pty-a"
                }),
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        assert!(
            !second["context_injection"]
                .as_str()
                .unwrap()
                .contains("AgentCall Policy Block")
        );
    }

    #[test]
    fn distinct_policy_denials_in_short_window_create_blocker() {
        let state = test_state("pty-policy-distinct-loop");
        install_pty_auto_route(&state, "pty-a", &["src"]);
        for target in ["docs/a.md", "docs/b.md"] {
            let mut payload = write_payload("claude-a", target);
            payload["wrapper_session"] = serde_json::json!("pty-a");
            let result = ingest_hook(
                &state,
                HookIngestRequest {
                    event: "PreToolUse".to_string(),
                    payload,
                    runtime: Some("claude-code-session".to_string()),
                },
            )
            .unwrap();
            assert_eq!(result["decision"]["allowed"], false);
        }
        let denials = policy_denials_state(&state);
        assert_eq!(denials["pty-a"]["active"], true);
        assert_eq!(denials["pty-a"]["repeat_count"], 1);
        assert_eq!(denials["pty-a"]["recent_denial_count"], 2);

        let projection = crate::projection::read_session_projection(&state, "pty-a").unwrap();
        assert_eq!(projection.attention_status, "blocked_by_policy");
        assert!(projection.needs_attention);
    }

    #[test]
    fn exit_plan_mode_marks_route_plan_ready() {
        let state = test_state("pty-plan-ready");
        install_pty_plan_route(&state, "pty-a");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: serde_json::json!({
                    "session_id": "claude-a",
                    "wrapper_session": "pty-a",
                    "tool_name": "ExitPlanMode",
                    "tool_input": {"plan": "do the thing"}
                }),
                runtime: Some("claude-code-session".to_string()),
            },
        )
        .unwrap();
        assert_eq!(result["status"], "plan_ready");
        assert_eq!(result["decision"]["allowed"], true);
        let routes = read_json_file(
            &state
                .workspace
                .join(".agentcall")
                .join("state")
                .join("routes.json"),
            serde_json::json!({}),
        );
        assert_eq!(routes["route-pty"]["status"], "plan_ready");
        assert_eq!(
            routes["route-pty"]["result"]["workflow_status"],
            "plan_ready"
        );
    }

    #[test]
    fn daemon_hook_claims_conflict_on_same_file() {
        let state = test_state("claim-conflict");
        let first = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: write_payload("sess-a", "src/app.py"),
                runtime: None,
            },
        )
        .unwrap();
        assert_eq!(first["decision"]["allowed"], true);

        let second = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: write_payload("sess-b", "src/app.py"),
                runtime: None,
            },
        )
        .unwrap();
        assert_eq!(second["decision"]["allowed"], false);
        assert_eq!(second["decision"]["conflicts"][0]["file"], "src/app.py");
    }

    #[test]
    fn daemon_hook_read_does_not_create_write_claim() {
        let state = test_state("read-observe");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PostToolUse".to_string(),
                payload: serde_json::json!({
                    "session_id": "sess-a",
                    "tool_name": "Read",
                    "tool_input": {"file_path": "src/app.py"}
                }),
                runtime: None,
            },
        )
        .unwrap();
        assert_eq!(result["decision"]["reason"], "read observed");
        let claims = file_claims_state(&state);
        assert!(claims.as_object().unwrap().is_empty());
    }

    #[test]
    fn daemon_hook_missing_session_id_is_unmatched_not_unknown() {
        let state = test_state("unmatched");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: serde_json::json!({
                    "tool_name": "Read",
                    "tool_input": {"file_path": "src/app.py"}
                }),
                runtime: None,
            },
        )
        .unwrap();
        assert_eq!(result["unmatched"], true);
        assert_ne!(result["session_id"], "unknown-session");
        let unmatched = unmatched_hooks_state(&state);
        assert_eq!(unmatched.as_array().unwrap().len(), 1);
    }

    #[test]
    fn hook_status_semantics_keep_stop_benign_and_permission_distinct() {
        assert_eq!(infer_hook_status("Stop", &serde_json::json!({})), "idle");
        assert_eq!(
            infer_hook_status("SubagentStop", &serde_json::json!({})),
            "observed"
        );
        assert_eq!(
            infer_hook_status(
                "SubagentStop",
                &serde_json::json!({"checkpoint_request": true})
            ),
            "checkpoint_due"
        );
        assert_eq!(
            infer_hook_status(
                "Notification",
                &serde_json::json!({"message": "Permission required for Bash"})
            ),
            "needs_permission"
        );
        assert_eq!(
            infer_hook_status(
                "Notification",
                &serde_json::json!({"message": "Claude is waiting for input"})
            ),
            "waiting_input"
        );
    }

    #[test]
    fn hook_env_binding_creates_runtime_binding() {
        let state = test_state("env-binding");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: serde_json::json!({
                    "session_id": "claude-a",
                    "wrapper_session": "wrapper-a",
                    "transcript_path": "E:/tmp/a.jsonl",
                    "cwd": "E:/Project/AgentCall",
                    "tool_name": "Read"
                }),
                runtime: None,
            },
        )
        .unwrap();
        assert_eq!(result["wrapper_session"], "wrapper-a");
        assert_eq!(result["binding_source"], "env");
        let bindings = runtime_bindings_state(&state);
        assert_eq!(bindings["wrapper-a"]["claude_session_id"], "claude-a");
        assert_eq!(bindings["wrapper-a"]["binding_source"], "env");
    }

    #[test]
    fn hook_known_session_fallback_only_after_existing_binding() {
        let state = test_state("known-binding");
        ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: serde_json::json!({
                    "session_id": "claude-a",
                    "wrapper_session": "wrapper-a",
                    "transcript_path": "E:/tmp/a.jsonl",
                    "tool_name": "Read"
                }),
                runtime: None,
            },
        )
        .unwrap();
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "Stop".to_string(),
                payload: serde_json::json!({
                    "session_id": "claude-a",
                    "transcript_path": "E:/tmp/a.jsonl"
                }),
                runtime: None,
            },
        )
        .unwrap();
        assert_eq!(result["wrapper_session"], "wrapper-a");
        assert_eq!(result["binding_source"], "known_session");
        let bindings = runtime_bindings_state(&state);
        assert_eq!(bindings["wrapper-a"]["last_hook_status"], "idle");
        assert_eq!(bindings["wrapper-a"]["binding_source"], "known_session");
    }

    #[test]
    fn hook_without_env_or_known_session_stays_unbound() {
        let state = test_state("unbound-binding");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: serde_json::json!({
                    "session_id": "claude-a",
                    "transcript_path": "E:/tmp/a.jsonl",
                    "tool_name": "Read"
                }),
                runtime: None,
            },
        )
        .unwrap();
        assert_eq!(result["binding_source"], "unbound");
        assert!(result.get("wrapper_session").unwrap().is_null());
        let bindings = runtime_bindings_state(&state);
        assert!(bindings.as_object().unwrap().is_empty());
    }
}
