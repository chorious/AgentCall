use crate::hooks::{pending_supervisor_instructions_state, runtime_bindings_state};
use crate::routes::routes_state;
use crate::session::{Session, list_sessions};
use crate::state::{AppState, read_events, read_json_file};
use crate::terminal::{clean_terminal_text, tail_lines};
use crate::util::now_ms;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub(crate) fn board_state(
    state: &AppState,
    view: Option<&str>,
    filter: Option<&str>,
    section: Option<&str>,
) -> serde_json::Value {
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
    let routes = routes_state(state);
    let live_daemon_sessions = list_sessions(state);
    let legacy_sessions = legacy_detached_sessions(&agent_dir.join("sessions"));
    let attention = attention_items(state);
    let runtime_health_value = runtime_health(state);

    let full = serde_json::json!({
        "workspace": state.workspace,
        "runtime_health": runtime_health_value,
        "pty_sessions": live_daemon_sessions,
        "live_daemon_sessions": list_sessions(state),
        "legacy_detached_sessions": legacy_sessions,
        "attention": attention,
        "active_sessions": active_sessions,
        "file_claims": file_claims,
        "transcripts": transcripts,
        "reports": reports,
        "routes": routes,
        "recent_events": events,
        "project_state": project_state
    });

    let selected = match section.unwrap_or("all") {
        "sessions" => serde_json::json!({
            "workspace": state.workspace,
            "live_daemon_sessions": full["live_daemon_sessions"],
            "legacy_detached_sessions": full["legacy_detached_sessions"],
        }),
        "events" => {
            serde_json::json!({"workspace": state.workspace, "recent_events": full["recent_events"]})
        }
        "reports" => serde_json::json!({"workspace": state.workspace, "reports": full["reports"]}),
        "routes" => serde_json::json!({"workspace": state.workspace, "routes": full["routes"]}),
        "claims" => {
            serde_json::json!({"workspace": state.workspace, "file_claims": full["file_claims"]})
        }
        "transcripts" => {
            serde_json::json!({"workspace": state.workspace, "transcripts": full["transcripts"]})
        }
        _ => full.clone(),
    };

    if filter == Some("attention") {
        return serde_json::json!({
            "workspace": state.workspace,
            "view": view.unwrap_or("full"),
            "filter": "attention",
            "attention": selected.get("attention").cloned().unwrap_or_else(|| attention_items(state)),
        });
    }
    if view == Some("compact") {
        return serde_json::json!({
            "workspace": state.workspace,
            "view": "compact",
            "runtime_health": selected.get("runtime_health").cloned().unwrap_or_else(|| runtime_health(state)),
            "live_daemon_sessions": selected.get("live_daemon_sessions").cloned().unwrap_or(serde_json::json!([])),
            "legacy_detached_sessions": selected.get("legacy_detached_sessions").cloned().unwrap_or(serde_json::json!([])),
            "attention": selected.get("attention").cloned().unwrap_or(serde_json::json!([])),
            "routes": recent_route_summaries(&full["routes"]),
            "reports": recent_report_summaries(&full["reports"]),
        });
    }
    selected
}

pub(crate) fn runtime_health(state: &AppState) -> serde_json::Value {
    let sessions = list_sessions(state);
    let running_sessions = sessions
        .iter()
        .filter(|session| session.status == "running")
        .count();
    let agent_dir = state.workspace.join(".agentcall");
    let stale_claims = stale_claim_count(&agent_dir.join("state").join("file_claims.json"));
    let runtime_bindings = runtime_bindings_state(state);
    let runtime_binding_count = runtime_bindings
        .as_object()
        .map(|items| items.len())
        .unwrap_or(0);
    let unbound_live_sessions = unbound_live_session_names(&sessions, &runtime_bindings);
    let claude_hook_config_status = claude_hook_config_status(state);
    let hook_warnings = claude_hook_config_status
        .get("warnings")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    serde_json::json!({
        "runtime": "agentcall-daemon",
        "workspace": state.workspace,
        "config_path": crate::config::config_path(&state.workspace),
        "config_error": state.config_error,
        "claude_workspace": state.config.claude_workspace,
        "missing_required_config": state.config.claude_workspace.is_none(),
        "state_writer": "daemon",
        "utf8_decoder": "streaming",
        "hook_aware_summary": true,
        "event_next": state.event_seq.load(Ordering::SeqCst),
        "active_pty_sessions": running_sessions,
        "live_daemon_sessions": running_sessions,
        "legacy_detached_sessions": legacy_detached_sessions(&agent_dir.join("sessions")).as_array().map(|items| items.len()).unwrap_or(0),
        "runtime_bindings": runtime_binding_count,
        "unbound_live_sessions": unbound_live_sessions,
        "restart_required_after_update": true,
        "stale_claims": stale_claims,
        "claude_hook_config_status": claude_hook_config_status,
        "warnings": hook_warnings,
        "status": if state.config.claude_workspace.is_some() { "ok" } else { "config_missing" }
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

const CLAUDE_HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolBatch",
    "Notification",
    "Stop",
    "SubagentStop",
    "PreCompact",
    "SessionEnd",
];

fn claude_hook_config_status(state: &AppState) -> serde_json::Value {
    let hook_script_path = state
        .workspace
        .join("scripts")
        .join("agentcall-claude-hook.py");
    let Some(claude_workspace) = state.config.claude_workspace.as_ref() else {
        return serde_json::json!({
            "settings_path": null,
            "has_agentcall_hooks": false,
            "missing_events": CLAUDE_HOOK_EVENTS,
            "post_tool_batch_enabled": false,
            "hook_script_path": hook_script_path,
            "hook_script_exists": hook_script_path.exists(),
            "python_command": null,
            "python_command_exists": false,
            "settings_mtime": null,
            "warnings": ["missing claude_workspace; cannot locate Claude hook settings"]
        });
    };
    let settings_path = claude_workspace.join(".claude").join("settings.local.json");
    let settings = read_json_file(&settings_path, serde_json::json!({}));
    let mut missing_events = vec![];
    let mut python_command: Option<String> = None;
    for event in CLAUDE_HOOK_EVENTS {
        if !event_has_agentcall_hook(&settings, event, &mut python_command) {
            missing_events.push((*event).to_string());
        }
    }
    let post_tool_batch_enabled = !missing_events.iter().any(|event| event == "PostToolBatch");
    let settings_mtime = fs::metadata(&settings_path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .map(|time| chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339());
    let python_command_exists = python_command
        .as_deref()
        .map(command_exists)
        .unwrap_or(false);
    let mut warnings = vec![];
    if !settings_path.exists() {
        warnings.push("Claude hook settings file is missing".to_string());
    }
    if !missing_events.is_empty() {
        warnings.push(format!(
            "Claude hook config is missing AgentCall events: {}",
            missing_events.join(", ")
        ));
    }
    if !post_tool_batch_enabled {
        warnings.push(
            "queued supervisor instructions may not be delivered because PostToolBatch is not installed".to_string(),
        );
    }
    if !hook_script_path.exists() {
        warnings.push("AgentCall Claude hook script is missing".to_string());
    }
    if python_command.is_some() && !python_command_exists {
        warnings.push("Configured Python command for Claude hook was not found".to_string());
    }
    serde_json::json!({
        "settings_path": settings_path,
        "has_agentcall_hooks": missing_events.is_empty(),
        "missing_events": missing_events,
        "post_tool_batch_enabled": post_tool_batch_enabled,
        "hook_script_path": hook_script_path,
        "hook_script_exists": hook_script_path.exists(),
        "python_command": python_command,
        "python_command_exists": python_command_exists,
        "settings_mtime": settings_mtime,
        "warnings": warnings
    })
}

fn event_has_agentcall_hook(
    settings: &serde_json::Value,
    event: &str,
    python_command: &mut Option<String>,
) -> bool {
    settings
        .get("hooks")
        .and_then(|hooks| hooks.get(event))
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .any(|entry| entry_contains_agentcall_hook(entry, python_command))
        })
        .unwrap_or(false)
}

fn entry_contains_agentcall_hook(
    entry: &serde_json::Value,
    python_command: &mut Option<String>,
) -> bool {
    entry
        .get("hooks")
        .and_then(serde_json::Value::as_array)
        .map(|hooks| {
            hooks.iter().any(|hook| {
                let command = hook
                    .get("command")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let args = hook
                    .get("args")
                    .and_then(serde_json::Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_default();
                let haystack = format!("{command} {args}");
                let is_agentcall = haystack.contains("agentcall-claude-hook.py");
                if is_agentcall && python_command.is_none() && !command.trim().is_empty() {
                    *python_command = Some(command.to_string());
                }
                is_agentcall
            })
        })
        .unwrap_or(false)
}

fn command_exists(command: &str) -> bool {
    let path = Path::new(command);
    if path.is_absolute() || command.contains('\\') || command.contains('/') {
        return path.exists();
    }
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    let extensions = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .map(str::to_string)
            .collect::<Vec<_>>()
    } else {
        vec!["".to_string()]
    };
    std::env::split_paths(&paths).any(|dir| {
        extensions.iter().any(|extension| {
            let candidate = if command
                .to_ascii_lowercase()
                .ends_with(&extension.to_ascii_lowercase())
            {
                dir.join(command)
            } else {
                dir.join(format!("{command}{extension}"))
            };
            candidate.exists()
        })
    })
}

pub(crate) fn session_summary(state: &AppState, session: &Arc<Session>) -> serde_json::Value {
    let status = session.status.lock().unwrap().clone();
    let clean_output = clean_session_output(session);
    let waiting_input = looks_like_waiting_for_input(&clean_output);
    let interrupted = clean_output.to_ascii_lowercase().contains("interrupted")
        || clean_output.contains("What should Claude do instead?");
    let reports = extract_reports(&clean_output);
    let report_source = if reports.is_empty() { "none" } else { "tui" };
    let report_ready = !reports.is_empty()
        || clean_output
            .to_ascii_lowercase()
            .contains("reports generated")
        || clean_output
            .to_ascii_lowercase()
            .contains("tasks completed");
    let agent_dir = state.workspace.join(".agentcall");
    let routes = routes_state(state);
    let route_result = route_result_for_session(&routes, &session.name);
    let bindings = runtime_bindings_state(state);
    let binding = binding_for_wrapper(&bindings, &session.name);
    let binding_source = binding
        .as_ref()
        .and_then(|value| value.get("binding_source"))
        .and_then(|value| value.as_str())
        .unwrap_or("unbound");
    let hook_session_id = binding
        .as_ref()
        .and_then(|value| value.get("claude_session_id"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let last_hook_event = binding
        .as_ref()
        .and_then(|value| value.get("last_hook_event"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let last_hook_status = binding
        .as_ref()
        .and_then(|value| value.get("last_hook_status"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let last_hook_at = binding
        .as_ref()
        .and_then(|value| value.get("last_seen"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());
    let hook_dimensions = last_hook_status.as_deref().map(hook_status_dimensions);
    let (mut liveness_status, mut attention_status, mut status_source) =
        lifecycle_dimensions(&status).unwrap_or_else(|| {
            if let Some((liveness, attention)) = hook_dimensions {
                (liveness, attention, "hook".to_string())
            } else if waiting_input || interrupted {
                (
                    "waiting_input".to_string(),
                    "waiting_input".to_string(),
                    "tui".to_string(),
                )
            } else if status == "running" {
                (
                    "working".to_string(),
                    "none".to_string(),
                    "daemon".to_string(),
                )
            } else {
                (
                    "unknown".to_string(),
                    "low_confidence".to_string(),
                    "unknown".to_string(),
                )
            }
        });
    if binding.is_none() && status == "running" {
        attention_status = "unbound".to_string();
        status_source = "unknown".to_string();
        if liveness_status == "unknown" {
            liveness_status = "working".to_string();
        }
    }
    if liveness_status == "failed" {
        attention_status = "failed".to_string();
    }
    if route_result
        .as_ref()
        .and_then(|result| result.get("workflow_status"))
        .and_then(|value| value.as_str())
        == Some("plan_ready")
    {
        liveness_status = "plan_ready".to_string();
        attention_status = "checkpoint_due".to_string();
        status_source = "route".to_string();
    }
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
                    let claim_session = claim.get("session_id").and_then(|value| value.as_str());
                    claim_session == Some(session.name.as_str())
                        || hook_session_id
                            .as_deref()
                            .is_some_and(|hook_id| claim_session == Some(hook_id))
                })
                .map(|(file, _)| file.clone())
                .collect()
        })
        .unwrap_or_default();
    let confidence = if attention_status == "unbound" {
        0.3
    } else if status_source == "hook" || status_source == "lifecycle" {
        0.9
    } else if waiting_input || interrupted || report_ready {
        0.55
    } else if clean_output.trim().is_empty() {
        0.2
    } else {
        0.55
    };
    if confidence < 0.5 && attention_status == "none" {
        attention_status = "low_confidence".to_string();
    }
    let needs_attention = attention_status != "none";
    let status_compat = if attention_status != "none" {
        attention_status.clone()
    } else {
        liveness_status.clone()
    };
    let needs_user_input =
        attention_status == "waiting_input" || attention_status == "needs_permission";
    let last_progress_age_seconds =
        now_ms().saturating_sub(session.updated_at.load(Ordering::Relaxed)) / 1000;
    let patience = patience_contract(
        &liveness_status,
        &attention_status,
        last_progress_age_seconds,
    );
    let hint_source = if waiting_input || interrupted || report_ready {
        Some("tui")
    } else {
        None
    };
    let last_tool = binding
        .as_ref()
        .and_then(|value| value.get("last_tool"))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let plan = lightweight_plan_from_route(route_result.as_ref());
    let pending_supervisor_instructions =
        pending_supervisor_instruction_count(state, &session.name);
    let last_supervisor_instruction_injected_at =
        last_supervisor_instruction_injected_at(state, &session.name);
    let has_post_tool_batch = session_has_seen_hook_event(state, &session.name, "PostToolBatch");
    let mut payload = serde_json::json!({
        "session": session.name,
        "project": state.workspace.file_name().and_then(|name| name.to_str()).unwrap_or("workspace"),
        "transport": "pty",
        "status": status_compat,
        "liveness_status": liveness_status,
        "attention_status": attention_status,
        "report_ready": report_ready,
        "report_source": if report_ready { report_source } else { "none" },
        "status_source": status_source,
        "hint_source": hint_source,
        "binding": binding,
        "binding_source": binding_source,
        "hook_session_id": hook_session_id,
        "last_hook_event": last_hook_event,
        "last_hook_status": last_hook_status,
        "last_hook_at": last_hook_at,
        "headline": headline(&clean_output),
        "current_task": current_task(&clean_output),
        "reports": reports,
        "tokens": extract_after_marker(&clean_output, "tokens"),
        "context_used": extract_context_used(&clean_output),
        "mode": route_result
            .as_ref()
            .and_then(|result| result.get("permission_mode"))
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| extract_mode(&clean_output)),
        "pty_workflow": route_result
            .as_ref()
            .and_then(|result| result.get("pty_workflow"))
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        "workflow_status": route_result
            .as_ref()
            .and_then(|result| result.get("workflow_status"))
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        "containment": route_result
            .as_ref()
            .and_then(|result| result.get("containment"))
            .and_then(|containment| containment.get("mode"))
            .and_then(|value| value.as_str())
            .unwrap_or("prompt_only"),
        "last_error": last_error(&clean_output),
        "needs_attention": needs_attention,
        "confidence": confidence,
        "decode_health": session.decode_health.lock().unwrap().clone(),
        "workspace": state.workspace,
        "cwd": session.cwd,
        "claude_workspace": state.config.claude_workspace,
        "last_tool": last_tool,
        "claimed_files": claimed_files,
        "files_written": [],
        "report": null,
        "needs_user_input": needs_user_input,
        "warnings": [],
        "conflicts": [],
        "pending_supervisor_instructions": pending_supervisor_instructions,
        "last_supervisor_instruction_injected_at": last_supervisor_instruction_injected_at,
        "post_tool_batch_seen": has_post_tool_batch
    });
    if pending_supervisor_instructions > 0 && !has_post_tool_batch {
        if let Some(warnings) = payload
            .get_mut("warnings")
            .and_then(|value| value.as_array_mut())
        {
            warnings.push(serde_json::json!("queued supervisor instructions may not be delivered until this Claude session emits PostToolBatch; restart the worker after hook install if this remains false"));
        }
    }
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "plan_ready".to_string(),
            plan.get("ready")
                .cloned()
                .unwrap_or(serde_json::Value::Bool(false)),
        );
        object.insert(
            "plan_source".to_string(),
            plan.get("source")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        );
        object.insert(
            "plan_path".to_string(),
            plan.get("path").cloned().unwrap_or(serde_json::Value::Null),
        );
        object.insert(
            "plan_excerpt".to_string(),
            plan.get("excerpt")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        );
        object.insert("patience_hint".to_string(), patience["hint"].clone());
        object.insert("patience_status".to_string(), patience["status"].clone());
        object.insert(
            "last_progress_age_seconds".to_string(),
            serde_json::json!(last_progress_age_seconds),
        );
        object.insert(
            "suggested_wait_seconds".to_string(),
            patience["suggested_wait_seconds"].clone(),
        );
        object.insert(
            "do_not_retry_before_seconds".to_string(),
            patience["do_not_retry_before_seconds"].clone(),
        );
        object.insert(
            "stall_threshold_seconds".to_string(),
            patience["stall_threshold_seconds"].clone(),
        );
    }
    payload
}

fn pending_supervisor_instruction_count(state: &AppState, wrapper_session: &str) -> usize {
    pending_supervisor_instructions_state(state)
        .get(wrapper_session)
        .and_then(serde_json::Value::as_array)
        .map(|items| items.len())
        .unwrap_or(0)
}

fn last_supervisor_instruction_injected_at(
    state: &AppState,
    wrapper_session: &str,
) -> Option<String> {
    let events = read_events(&state.workspace.join(".agentcall").join("events.ndjson"));
    events.iter().rev().find_map(|event| {
        if event.get("type").and_then(serde_json::Value::as_str)
            != Some("supervisor_instruction.injected")
        {
            return None;
        }
        let same_wrapper = event
            .get("data")
            .and_then(|data| data.get("wrapper_session"))
            .and_then(serde_json::Value::as_str)
            == Some(wrapper_session);
        if same_wrapper {
            event
                .get("ts")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        } else {
            None
        }
    })
}

fn session_has_seen_hook_event(state: &AppState, wrapper_session: &str, hook_event: &str) -> bool {
    let expected_type = format!("hook.{hook_event}");
    read_events(&state.workspace.join(".agentcall").join("events.ndjson"))
        .iter()
        .rev()
        .any(|event| {
            event.get("type").and_then(serde_json::Value::as_str) == Some(expected_type.as_str())
                && event
                    .get("data")
                    .and_then(|data| data.get("wrapper_session"))
                    .and_then(serde_json::Value::as_str)
                    == Some(wrapper_session)
        })
}

pub(crate) fn session_plan_artifact(
    state: &AppState,
    session: &Arc<Session>,
    include_content: bool,
) -> serde_json::Value {
    let bindings = runtime_bindings_state(state);
    let binding = binding_for_wrapper(&bindings, &session.name);
    let clean_output = clean_session_output(session);
    plan_artifact_from_binding(&binding, &clean_output, include_content)
}

pub(crate) fn clean_session_output(session: &Arc<Session>) -> String {
    let text = session.clean_replay.lock().unwrap().clone();
    tail_lines(&clean_terminal_text(&text), 120)
}

fn patience_contract(
    liveness_status: &str,
    attention_status: &str,
    last_progress_age_seconds: u64,
) -> serde_json::Value {
    let suggested_wait_seconds = 45u64;
    let do_not_retry_before_seconds = 60u64;
    let stall_threshold_seconds = 180u64;
    if attention_status != "none" {
        return serde_json::json!({
            "status": "attention_required",
            "hint": "Attention status is active; inspect summary/report before waiting longer.",
            "suggested_wait_seconds": 0,
            "do_not_retry_before_seconds": 0,
            "stall_threshold_seconds": stall_threshold_seconds
        });
    }
    if matches!(liveness_status, "working" | "idle" | "unknown")
        && last_progress_age_seconds < do_not_retry_before_seconds
    {
        return serde_json::json!({
            "status": "inside_patience_window",
            "hint": "Worker recently started or produced output. Wait before retrying, nudging, or declaring it stuck.",
            "suggested_wait_seconds": suggested_wait_seconds,
            "do_not_retry_before_seconds": do_not_retry_before_seconds,
            "stall_threshold_seconds": stall_threshold_seconds
        });
    }
    if matches!(liveness_status, "working" | "unknown")
        && last_progress_age_seconds >= stall_threshold_seconds
    {
        return serde_json::json!({
            "status": "inspect_progress",
            "hint": "No recent progress past the stall threshold. Inspect clean_tail or request a concise status before restarting.",
            "suggested_wait_seconds": 0,
            "do_not_retry_before_seconds": 0,
            "stall_threshold_seconds": stall_threshold_seconds
        });
    }
    serde_json::json!({
        "status": "normal",
        "hint": "Use board/session summary before nudging; Claude Code PTY work can be quiet while reading or thinking.",
        "suggested_wait_seconds": suggested_wait_seconds,
        "do_not_retry_before_seconds": do_not_retry_before_seconds,
        "stall_threshold_seconds": stall_threshold_seconds
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

pub(crate) fn legacy_detached_sessions(sessions_dir: &Path) -> serde_json::Value {
    let Ok(entries) = fs::read_dir(sessions_dir) else {
        return serde_json::json!([]);
    };
    let mut sessions = vec![];
    for entry in entries.flatten() {
        let path = entry.path().join("state.json");
        if !path.exists() {
            continue;
        }
        let mut value = read_json_file(&path, serde_json::json!({}));
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "runtime".to_string(),
                serde_json::json!("legacy_python_pty"),
            );
            object.insert(
                "status_class".to_string(),
                serde_json::json!("legacy_detached"),
            );
            object.insert("live".to_string(), serde_json::json!(false));
        }
        sessions.push(value);
    }
    sessions.sort_by(|a, b| {
        a.get("name")
            .and_then(|value| value.as_str())
            .cmp(&b.get("name").and_then(|value| value.as_str()))
    });
    serde_json::json!(sessions)
}

fn attention_items(state: &AppState) -> serde_json::Value {
    let mut items = vec![];
    let live_sessions: Vec<Arc<Session>> =
        state.sessions.lock().unwrap().values().cloned().collect();
    for session in live_sessions {
        let summary = session_summary(state, &session);
        let attention_status = summary
            .get("attention_status")
            .and_then(|value| value.as_str())
            .unwrap_or("none");
        if matches!(
            attention_status,
            "needs_permission" | "checkpoint_due" | "waiting_input" | "unbound" | "failed"
        ) {
            items.push(serde_json::json!({
                "kind": "daemon_session_attention",
                "session": summary.get("session").cloned().unwrap_or(serde_json::Value::Null),
                "liveness_status": summary.get("liveness_status").cloned().unwrap_or(serde_json::Value::Null),
                "attention_status": attention_status,
                "status_source": summary.get("status_source").cloned().unwrap_or(serde_json::Value::Null),
                "binding_source": summary.get("binding_source").cloned().unwrap_or(serde_json::Value::Null),
                "patience_status": summary.get("patience_status").cloned().unwrap_or(serde_json::Value::Null),
                "patience_hint": summary.get("patience_hint").cloned().unwrap_or(serde_json::Value::Null),
                "last_progress_age_seconds": summary.get("last_progress_age_seconds").cloned().unwrap_or(serde_json::Value::Null),
                "needs_attention": true,
            }));
        }
    }
    serde_json::json!(items)
}

fn recent_route_summaries(routes: &serde_json::Value) -> serde_json::Value {
    let mut items = routes.as_array().cloned().unwrap_or_default();
    items.sort_by(|a, b| {
        a.get("updated_at")
            .and_then(|value| value.as_u64())
            .cmp(&b.get("updated_at").and_then(|value| value.as_u64()))
    });
    let summaries: Vec<serde_json::Value> = items
        .into_iter()
        .rev()
        .take(8)
        .map(|route| {
            serde_json::json!({
                "route_id": route.get("route_id").cloned().unwrap_or(serde_json::Value::Null),
                "runtime": route.get("recommended_runtime").cloned().unwrap_or(serde_json::Value::Null),
                "status": route.get("status").cloned().unwrap_or(serde_json::Value::Null),
                "session_name": route.get("session_name").cloned().unwrap_or(serde_json::Value::Null),
                "worker_kind": route.get("result").and_then(|result| result.get("worker_kind")).cloned().unwrap_or(serde_json::Value::Null),
                "workflow_status": route.get("result").and_then(|result| result.get("workflow_status")).cloned().unwrap_or(serde_json::Value::Null),
                "required_next_step": route.get("required_next_step").cloned().unwrap_or(serde_json::Value::Null),
                "suggested_wait_seconds": route.get("suggested_wait_seconds").cloned().unwrap_or(serde_json::Value::Null),
                "do_not_retry_before_seconds": route.get("do_not_retry_before_seconds").cloned().unwrap_or(serde_json::Value::Null),
                "updated_at": route.get("updated_at").cloned().unwrap_or(serde_json::Value::Null),
            })
        })
        .collect();
    serde_json::json!(summaries)
}

fn recent_report_summaries(reports: &serde_json::Value) -> serde_json::Value {
    let items = reports.as_array().cloned().unwrap_or_default();
    let summaries: Vec<serde_json::Value> = items
        .into_iter()
        .rev()
        .take(8)
        .map(|report| {
            serde_json::json!({
                "task_id": report.get("task_id").cloned().unwrap_or(serde_json::Value::Null),
                "status": report.get("status").cloned().unwrap_or(serde_json::Value::Null),
                "report_path": report.get("report_path").or_else(|| report.get("path")).cloned().unwrap_or(serde_json::Value::Null),
                "summary": report.get("summary").cloned().unwrap_or(serde_json::Value::Null),
            })
        })
        .collect();
    serde_json::json!(summaries)
}

fn binding_for_wrapper(
    bindings: &serde_json::Value,
    wrapper_session: &str,
) -> Option<serde_json::Value> {
    bindings.get(wrapper_session).cloned()
}

fn lifecycle_dimensions(status: &str) -> Option<(String, String, String)> {
    if status.starts_with("error") {
        Some((
            "failed".to_string(),
            "failed".to_string(),
            "lifecycle".to_string(),
        ))
    } else if status.starts_with("exited") {
        Some((
            "completed".to_string(),
            "none".to_string(),
            "lifecycle".to_string(),
        ))
    } else {
        None
    }
}

fn hook_status_dimensions(status: &str) -> (String, String) {
    match status {
        "needs_permission" => (
            "needs_permission".to_string(),
            "needs_permission".to_string(),
        ),
        "waiting_input" => ("waiting_input".to_string(), "waiting_input".to_string()),
        "checkpoint_due" => ("checkpoint_due".to_string(), "checkpoint_due".to_string()),
        "plan_ready" => ("plan_ready".to_string(), "checkpoint_due".to_string()),
        "idle" => ("idle".to_string(), "none".to_string()),
        "completed" | "ended" => ("completed".to_string(), "none".to_string()),
        "failed" => ("failed".to_string(), "failed".to_string()),
        "working" | "running" | "observed" | "notified" => {
            ("working".to_string(), "none".to_string())
        }
        _ => ("unknown".to_string(), "low_confidence".to_string()),
    }
}

fn route_result_for_session(
    routes: &serde_json::Value,
    session_name: &str,
) -> Option<serde_json::Value> {
    if let Some(object) = routes.as_object() {
        return object
            .values()
            .find_map(|route| route_result_match(route, session_name));
    }
    routes.as_array().and_then(|items| {
        items
            .iter()
            .find_map(|route| route_result_match(route, session_name))
    })
}

fn route_result_match(route: &serde_json::Value, session_name: &str) -> Option<serde_json::Value> {
    let session_match =
        route.get("session_name").and_then(|value| value.as_str()) == Some(session_name);
    let plan_match = route
        .get("result")
        .and_then(|result| result.get("plan_session_name"))
        .and_then(|value| value.as_str())
        == Some(session_name);
    let auto_match = route
        .get("result")
        .and_then(|result| result.get("auto_session_name"))
        .and_then(|value| value.as_str())
        == Some(session_name);
    if session_match || plan_match || auto_match {
        route.get("result").cloned()
    } else {
        None
    }
}

fn unbound_live_session_names(
    sessions: &[crate::session::SessionInfo],
    bindings: &serde_json::Value,
) -> Vec<String> {
    sessions
        .iter()
        .filter(|session| session.status == "running")
        .filter(|session| binding_for_wrapper(bindings, &session.name).is_none())
        .map(|session| session.name.clone())
        .collect()
}

fn headline(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && !line.contains("Auto-update failed")
                && !line.contains("for shortcuts")
                && !line.starts_with('>')
        })
        .map(|line| line.chars().take(240).collect())
}

fn current_task(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| line.contains("task") || line.contains("Task") || line.contains("v2."))
        .map(|line| line.chars().take(240).collect())
}

fn extract_reports(text: &str) -> Vec<String> {
    let mut reports = vec![];
    for token in text.split(|ch: char| {
        ch.is_whitespace() || ch == '"' || ch == '\'' || ch == ',' || ch == ':' || ch == ';'
    }) {
        let trimmed = token.trim_matches(|ch: char| {
            !ch.is_ascii_alphanumeric() && ch != '_' && ch != '-' && ch != '.'
        });
        if trimmed.starts_with("report_")
            && trimmed.ends_with(".md")
            && !reports.iter().any(|item| item == trimmed)
        {
            reports.push(trimmed.to_string());
        }
    }
    reports
}

fn extract_after_marker(text: &str, marker: &str) -> Option<String> {
    text.lines()
        .rev()
        .find(|line| line.contains(marker))
        .map(|line| line.trim().chars().take(120).collect())
}

fn extract_context_used(text: &str) -> Option<String> {
    text.lines().rev().find_map(|line| {
        line.split_once("context used")
            .map(|_| line.trim().chars().take(80).collect())
    })
}

fn extract_mode(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    if lower.contains("auto mode on") {
        "auto".to_string()
    } else if lower.contains("plan mode") {
        "plan".to_string()
    } else {
        "unknown".to_string()
    }
}

fn last_error(text: &str) -> Option<String> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| {
            let lower = line.to_ascii_lowercase();
            (lower.contains("error") || lower.contains("failed"))
                && !line.contains("Auto-update failed")
        })
        .map(|line| line.chars().take(240).collect())
}

fn plan_artifact_from_binding(
    binding: &Option<Value>,
    clean_output: &str,
    include_content: bool,
) -> Value {
    let transcript_path = binding
        .as_ref()
        .and_then(|value| value.get("transcript_path"))
        .and_then(Value::as_str);
    let mut plan_path: Option<String> = None;
    let mut plan_exists = false;
    let mut plan_mode_seen = false;
    let mut exit_plan_mode_seen = false;
    let mut allowed_prompts = Value::Null;
    let mut transcript_plan_text: Option<String> = None;
    let mut source = "none".to_string();
    let mut content: Option<String> = None;

    if let Some(path) = transcript_path {
        let transcript = Path::new(path);
        if let Ok(text) = fs::read_to_string(transcript) {
            for line in text.lines() {
                let Ok(value) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if let Some(attachment) = value.get("attachment") {
                    if attachment.get("type").and_then(Value::as_str) == Some("plan_mode") {
                        plan_mode_seen = true;
                        if let Some(path) = attachment.get("planFilePath").and_then(Value::as_str) {
                            plan_path = Some(path.to_string());
                        }
                    }
                }
                if value
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind != "assistant")
                {
                    continue;
                }
                let Some(message) = value.get("message") else {
                    continue;
                };
                if message.get("role").and_then(Value::as_str) != Some("assistant") {
                    continue;
                }
                if let Some(items) = message.get("content").and_then(Value::as_array) {
                    for item in items {
                        if item.get("type").and_then(Value::as_str) == Some("tool_use")
                            && item.get("name").and_then(Value::as_str) == Some("ExitPlanMode")
                        {
                            exit_plan_mode_seen = true;
                            allowed_prompts = item
                                .get("input")
                                .and_then(|input| input.get("allowedPrompts"))
                                .cloned()
                                .unwrap_or(Value::Null);
                            if let Some(text) = extract_plan_text_from_value(item.get("input")) {
                                transcript_plan_text = Some(text);
                                source = "exit_plan_mode".to_string();
                            }
                        }
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            if (plan_mode_seen || exit_plan_mode_seen) && looks_like_plan_text(text)
                            {
                                transcript_plan_text = Some(text.to_string());
                                if source == "none" {
                                    source = "transcript_text".to_string();
                                }
                            }
                        }
                    }
                } else if let Some(text) = message.get("content").and_then(Value::as_str) {
                    if (plan_mode_seen || exit_plan_mode_seen) && looks_like_plan_text(text) {
                        transcript_plan_text = Some(text.to_string());
                        if source == "none" {
                            source = "transcript_text".to_string();
                        }
                    }
                }
            }
        }
    }

    if let Some(path) = plan_path.as_deref() {
        let path_ref = Path::new(path);
        if path_ref.exists() {
            plan_exists = true;
            if let Ok(text) = fs::read_to_string(path_ref) {
                if !text.trim().is_empty() {
                    source = "plan_file".to_string();
                    content = Some(text);
                }
            }
        }
    }
    if content.is_none() {
        content = transcript_plan_text;
    }
    if content.is_none()
        && (plan_mode_seen || exit_plan_mode_seen)
        && looks_like_plan_text(clean_output)
    {
        source = "clean_tail".to_string();
        content = Some(clean_output.to_string());
    }

    let excerpt = content
        .as_deref()
        .map(|text| clip_chars(text.trim(), 2400))
        .unwrap_or_default();
    let ready = exit_plan_mode_seen || content.is_some() || plan_exists;
    let mut value = serde_json::json!({
        "ready": ready,
        "source": source,
        "path": plan_path,
        "path_exists": plan_exists,
        "transcript_path": transcript_path,
        "excerpt": excerpt,
        "allowed_prompts": allowed_prompts
    });
    if include_content {
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "content".to_string(),
                content
                    .map(|text| Value::String(clip_chars(&text, 20000)))
                    .unwrap_or(Value::Null),
            );
        }
    }
    value
}

fn lightweight_plan_from_route(route_result: Option<&Value>) -> Value {
    let Some(result) = route_result else {
        return empty_plan_artifact();
    };
    let is_plan_workflow = result.get("pty_workflow").and_then(Value::as_str)
        == Some("plan_then_auto")
        || result.get("permission_mode").and_then(Value::as_str) == Some("plan")
        || result.get("plan_session_name").is_some();
    if !is_plan_workflow {
        return empty_plan_artifact();
    }
    let workflow_status = result.get("workflow_status").and_then(Value::as_str);
    serde_json::json!({
        "ready": workflow_status == Some("plan_ready"),
        "source": if workflow_status == Some("plan_ready") { "route" } else { "none" },
        "path": null,
        "path_exists": false,
        "transcript_path": null,
        "excerpt": "",
        "allowed_prompts": null
    })
}

fn empty_plan_artifact() -> Value {
    serde_json::json!({
        "ready": false,
        "source": "none",
        "path": null,
        "path_exists": false,
        "transcript_path": null,
        "excerpt": "",
        "allowed_prompts": null
    })
}

fn extract_plan_text_from_value(value: Option<&Value>) -> Option<String> {
    let value = value?;
    for key in ["plan", "plan_text", "content", "text", "summary"] {
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            if looks_like_plan_text(text) {
                return Some(text.to_string());
            }
        }
    }
    None
}

fn looks_like_plan_text(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.chars().count() < 400 {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    let strong_markers = [
        "# ",
        "## ",
        "verification criteria",
        "completion criteria",
        "planned files",
        "awaiting supervisor",
    ];
    let weak_markers = ["goals", "steps", "risks", "acceptance criteria"];
    let mut strong_score = 0;
    let mut weak_score = 0;
    for marker in strong_markers {
        if lower.contains(marker) {
            strong_score += 1;
        }
    }
    for marker in weak_markers {
        if lower.contains(marker) {
            weak_score += 1;
        }
    }
    if trimmed.contains("计划") {
        strong_score += 1;
    }
    strong_score > 0 && strong_score + weak_score >= 2
}

fn clip_chars(text: &str, max_chars: usize) -> String {
    let mut clipped: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        clipped.push_str("\n...[truncated]");
    }
    clipped
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;

    #[test]
    fn extracts_reports_and_ignores_auto_update_as_error() {
        let text = "All tasks completed\nReports created:\n- report_v2.0.md\n- report_v2.1.md\nAuto-update failed · Run /doctor";
        assert_eq!(
            extract_reports(text),
            vec!["report_v2.0.md", "report_v2.1.md"]
        );
        assert_eq!(last_error(text), None);
    }

    #[test]
    fn plan_text_detection_rejects_prompt_but_accepts_presented_plan() {
        let prompt = "Plan-only smoke test for AgentCall wrapper plan extraction. Do not modify files. Produce a short plan with goal, steps, risks, and acceptance criteria, then use ExitPlanMode and wait for supervisor approval. This is only to verify AgentCall can extract Claude Code plan output through agentcall_session include=plan.\n".repeat(5);
        assert!(!looks_like_plan_text(&prompt));

        let plan = "# GGMYS v0.4 Frontend Log / Feedback Plan\n\n## Goals\nSeparate player-facing gameplay log from technical diagnostics log.\n\n## Planned Files\n- client/scripts/LoginScene.cs\n\n## Risks\nExisting smoke tests may expect raw event tags. Existing UI update hooks may need coordination with runtime and equipment workers.\n\n## Verification Criteria\nC# build passes and manual playthrough confirms log layers. Battle, equip, refresh, and technical diagnostics remain readable.\n\nAwaiting supervisor approve_plan or start_auto signal before writing files.";
        assert!(looks_like_plan_text(plan));
    }

    #[test]
    fn plan_artifact_extracts_presented_plan_from_transcript_without_plan_file() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-plan-transcript-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let transcript = root.join("session.jsonl");
        let plan_path = root.join("missing-plan.md");
        let plan = "# GGMYS v0.4 Frontend Log / Feedback Plan\n\n## Goals\nSeparate player-facing gameplay log from technical diagnostics log.\n\n## Planned Files\n- client/scripts/LoginScene.cs\n\n## Risks\nExisting smoke tests may expect raw event tags. Existing UI update hooks may need coordination with runtime and equipment workers.\n\n## Verification Criteria\nC# build passes and manual playthrough confirms log layers. Battle, equip, refresh, and technical diagnostics remain readable.\n\nAwaiting supervisor approve_plan or start_auto signal before writing files.";
        let lines = vec![
            serde_json::json!({
                "type": "attachment",
                "attachment": {
                    "type": "plan_mode",
                    "planFilePath": plan_path
                }
            }),
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "text",
                        "text": plan
                    }]
                }
            }),
        ];
        fs::write(
            &transcript,
            lines
                .into_iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let binding = Some(serde_json::json!({
            "transcript_path": transcript,
        }));
        let artifact = plan_artifact_from_binding(&binding, "", true);
        assert_eq!(artifact["ready"], true);
        assert_eq!(artifact["source"], "transcript_text");
        assert_eq!(artifact["path_exists"], false);
        assert!(
            artifact["content"]
                .as_str()
                .unwrap()
                .contains("Frontend Log / Feedback Plan")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn plan_artifact_ignores_long_markdown_without_plan_signal() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-non-plan-transcript-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let transcript = root.join("session.jsonl");
        let long_reply = "# Architecture Notes\n\n## Goals\nThis is a long ordinary assistant answer discussing goals, steps, risks, and acceptance criteria. It is not a Claude Code plan-mode artifact and should not be promoted to plan_ready. ".repeat(8);
        fs::write(
            &transcript,
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "content": [{
                        "type": "text",
                        "text": long_reply
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();
        let binding = Some(serde_json::json!({
            "transcript_path": transcript,
        }));
        let artifact = plan_artifact_from_binding(&binding, "", true);
        assert_eq!(artifact["ready"], false);
        assert_eq!(artifact["source"], "none");
        assert_eq!(artifact["content"], serde_json::Value::Null);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lightweight_plan_summary_uses_route_without_transcript_scan() {
        let plan_running = serde_json::json!({
            "pty_workflow": "plan_then_auto",
            "workflow_status": "plan_running",
            "permission_mode": "plan"
        });
        let artifact = lightweight_plan_from_route(Some(&plan_running));
        assert_eq!(artifact["ready"], false);
        assert_eq!(artifact["source"], "none");

        let plan_ready = serde_json::json!({
            "pty_workflow": "plan_then_auto",
            "workflow_status": "plan_ready",
            "permission_mode": "plan"
        });
        let artifact = lightweight_plan_from_route(Some(&plan_ready));
        assert_eq!(artifact["ready"], true);
        assert_eq!(artifact["source"], "route");
    }

    #[test]
    fn board_marks_python_sessions_as_legacy_detached() {
        let root =
            std::env::temp_dir().join(format!("agentcall-board-test-{}", std::process::id()));
        let session_dir = root.join(".agentcall").join("sessions").join("legacy-one");
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            session_dir.join("state.json"),
            r#"{"name":"legacy-one","status":"running","worker_pid":123,"child_pid":456}"#,
        )
        .unwrap();
        let state = AppState::test(root.clone());
        let board = board_state(&state, Some("compact"), None, None);
        let legacy = board["legacy_detached_sessions"].as_array().unwrap();
        assert_eq!(legacy[0]["name"], "legacy-one");
        assert_eq!(legacy[0]["status_class"], "legacy_detached");
        assert_eq!(legacy[0]["live"], false);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn route_result_for_session_supports_board_array_shape() {
        let routes = serde_json::json!([
            {
                "route_id": "route-1",
                "session_name": "pty-a",
                "result": {
                    "pty_workflow": "plan_then_auto",
                    "workflow_status": "plan_running",
                    "permission_mode": "plan"
                }
            }
        ]);
        let result = route_result_for_session(&routes, "pty-a").unwrap();
        assert_eq!(result["pty_workflow"], "plan_then_auto");
        assert_eq!(result["permission_mode"], "plan");
    }
}
