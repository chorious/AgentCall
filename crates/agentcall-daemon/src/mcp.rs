use crate::actor::submit_session_command;
use crate::commands::{CommandType, PreparedCommand, prepare_session_send_command};
use crate::confidence::attach_confidence_to_reports;
use crate::control::{
    control_summary_for_session, destructive_action_requires_control, validate_control_token,
};
use crate::crypto::sha256_hex;
use crate::errors::error_value;
use crate::events::EventEnvelopeV1;
use crate::hooks::{policy_denials_state, runtime_bindings_state};
use crate::projection::session_projection_summary;
use crate::prompt_gate::{
    DEFAULT_COMMIT_ACK_DEADLINE_MS, prompt_commit_attempt_id, prompt_gate_for_session,
    prompt_gate_from_route,
};
use crate::routes::{
    RouteRequest, checkpoint_session, handle_route_for_owner, patch_route_record,
    route_for_wrapper_session,
};
use crate::session::get_session;
use crate::state::{AppState, append_agent_event};
use crate::store::EventQuery;
use crate::summary::{
    board_owner_filter, board_state, clean_session_output, deprecated_clean_tail_value,
    session_plan_artifact, session_summary, terminal_snapshot_value,
};
use crate::util::now_ms;
use crate::worker_state::{WorkerStateKind, worker_snapshot_for_session, worker_state_for_session};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

const SUMMARY_VIEW_MAX_BYTES: usize = 8 * 1024;
const TUI_VIEW_MAX_BYTES: usize = 20 * 1024;
const EVENTS_VIEW_MAX_BYTES: usize = 48 * 1024;
const DEBUG_VIEW_MAX_BYTES: usize = 128 * 1024;
const STRING_PREVIEW_BYTES: usize = 160;
const REPORT_REQUEST_DEADLINE_MS: u64 = 5 * 60 * 1000;

#[derive(Deserialize)]
pub(crate) struct McpCallRequest {
    name: String,
    arguments: Option<Value>,
    client: Option<McpClientContext>,
}

#[derive(Deserialize)]
struct McpClientContext {
    owner_id: Option<String>,
}

pub(crate) fn mcp_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "agentcall_board",
            "description": "Return unified board state. Use compact/attention views for low-friction Codex control. PTY workers are asynchronous; inspect attention and patience hints before retrying or declaring a worker stuck.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "view": {"type": "string", "enum": ["full", "compact"], "default": "compact"},
                    "filter": {"type": "string", "enum": ["all", "attention"], "default": "attention"},
                    "section": {"type": "string", "enum": ["all", "sessions", "events", "reports", "claims", "transcripts", "routes"], "default": "all"},
                    "scope": {"type": "string", "enum": ["all", "mine"], "default": "mine"},
                    "owner_id": {"type": "string"}
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_route",
            "description": "Start a Claude Code PTY utility worker. PTY workers are asynchronous background workers, not synchronous function calls; after start, inspect the returned worker state or agentcall_session summary before taking the next action.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "objective": {"type": "string"},
                    "workspace": {"type": "string"},
                    "session_name": {"type": "string"},
                    "write_paths": {"type": "array", "items": {"type": "string"}, "description": "Paths the worker may modify, plus daemon-minted scratch/report paths."},
                    "reference_paths": {"type": "array", "items": {"type": "string"}, "description": "Recommended read/context paths for the worker. This is not a read permission boundary."},
                    "acceptance_criteria": {"type": "array", "items": {"type": "string"}},
                    "report_path": {"type": "string"}
                },
                "required": ["objective"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_session",
            "description": "Return one daemon PTY session view. Default view=summary is compact and projection-first, including state/why/can_wait/primary_action/available_actions/debug_actions/report/control/prompt_gate. Use view=tui for dashboard data, view=events for compact events, and view=debug/raw only for explicit inspection.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "name": {"type": "string"},
                    "view": {"type": "string", "enum": ["summary", "tui", "events", "debug", "raw"], "default": "summary"},
                    "detail": {"type": "string", "enum": ["compact", "debug", "raw"], "default": "compact"},
                    "include": {"type": "array", "items": {"type": "string", "enum": ["summary", "control", "clean_tail", "screen", "plan", "events", "artifacts", "policy", "metrics", "debug"]}, "default": ["summary"]},
                    "cursor": {"type": "integer", "minimum": 0},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 50},
                    "event_types": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["name"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_session_send",
            "description": "Send text or a high-level action to a daemon PTY session. Use the returned primary_action for normal flow; available_actions are explicit alternatives, and debug_actions are recovery-only. submit_pending_prompt is a debug/recovery signal, not the normal route path.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "name": {"type": "string"},
                    "action": {"type": "string", "enum": ["send", "continue", "request_report", "submit_pending_prompt", "select_option", "interrupt", "stop", "kill", "revise_plan", "approve_plan", "start_auto"], "default": "send"},
                    "text": {"type": "string"},
                    "control_token": {"type": "string", "description": "Short-lived daemon-minted control token from agentcall_session(summary). Required for destructive or phase-changing actions."},
                    "choice": {"type": "string", "description": "Menu/permission choice for select_option, such as 1, 2, or 3."},
                    "user_explicit_close": {"type": "boolean", "default": false, "description": "Set true only when the human explicitly wants to close/reclaim the worker before the patience window elapses."}
                },
                "required": ["name"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_report",
            "description": "Request or accept a report for a supervised session/task. Accept responses split confidence into overall/artifact/daemon_write/route_match; overall=high requires daemon-observed write or equivalent evidence.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "action": {"type": "string", "enum": ["request", "accept"], "default": "accept"},
                    "session_id": {"type": "string"},
                    "task_id": {"type": "string"}
                },
                "additionalProperties": false
            }
        }),
    ]
}

pub(crate) fn mcp_call(state: &Arc<AppState>, req: McpCallRequest) -> Result<Value, String> {
    let args = req.arguments.unwrap_or_else(|| json!({}));
    let result = match req.name.as_str() {
        "agentcall_board" => mcp_board(state, &args, req.client.as_ref()),
        "agentcall_route" => mcp_route(state, args.clone(), req.client.as_ref()),
        "agentcall_session" => mcp_session(state, &args, req.client.as_ref()),
        "agentcall_session_send" => mcp_session_send(state, &args, req.client.as_ref()),
        "agentcall_report" => mcp_report(state, &args, req.client.as_ref()),
        other => Err(format!("unknown daemon MCP tool: {other}")),
    };
    let status = if result.is_ok() { "ok" } else { "error" };
    let error = result.as_ref().err().map(|message| error_value(message));
    let message = result.as_ref().err().map(String::as_str).unwrap_or("");
    let event_message = if message.is_empty() {
        format!("MCP tool {} completed.", req.name)
    } else {
        format!("MCP tool {} failed.", req.name)
    };
    append_agent_event(
        state,
        "mcp.tool_called",
        &event_message,
        json!({
            "tool": req.name,
            "status": status,
            "arguments": redact_mcp_arguments(&args),
            "runtime": "daemon_mcp_bridge",
            "error": error.unwrap_or(Value::Null),
            "error_message": message,
        }),
    );
    result
}

fn mcp_board(
    state: &AppState,
    args: &Value,
    client: Option<&McpClientContext>,
) -> Result<Value, String> {
    let scope = args.get("scope").and_then(Value::as_str).unwrap_or("mine");
    let view = args
        .get("view")
        .and_then(Value::as_str)
        .unwrap_or("compact");
    let debug_global = view == "full" && scope == "all";
    let owner_for_non_debug = client_owner_id(client);
    let requested_owner = args.get("owner_id").and_then(Value::as_str);
    let owner_id = if debug_global {
        board_owner_filter(Some(scope), requested_owner)
    } else {
        board_owner_filter(Some("mine"), Some(&owner_for_non_debug))
    };
    Ok(board_state(
        state,
        Some(view),
        args.get("filter").and_then(Value::as_str),
        args.get("section").and_then(Value::as_str),
        owner_id.as_deref(),
        args.get("root")
            .or_else(|| args.get("workspace"))
            .and_then(Value::as_str),
    ))
}

fn mcp_route(
    state: &Arc<AppState>,
    args: Value,
    client: Option<&McpClientContext>,
) -> Result<Value, String> {
    let req: RouteRequest =
        serde_json::from_value(args).map_err(|err| format!("invalid route arguments: {err}"))?;
    let owner_id = client_owner_id(client);
    let route = handle_route_for_owner(state, req, &owner_id)?;
    Ok(route_mcp_response(state, &route))
}

fn client_owner_id(client: Option<&McpClientContext>) -> String {
    client_owner_id_optional(client).unwrap_or_else(|| "codex".to_string())
}

fn client_owner_id_optional(client: Option<&McpClientContext>) -> Option<String> {
    client
        .and_then(|client| client.owner_id.as_deref())
        .map(normalize_owner_id)
        .filter(|value| !value.is_empty())
}

fn normalize_owner_id(value: &str) -> String {
    let mut normalized = String::new();
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':') {
            normalized.push(ch);
        } else {
            normalized.push('-');
        }
        if normalized.len() >= 96 {
            break;
        }
    }
    normalized.trim_matches('-').to_string()
}

fn route_mcp_response(state: &AppState, route: &Value) -> Value {
    let route_id = route
        .get("route_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let worker_name = route
        .get("session_name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            route
                .pointer("/result/session/name")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let report = route.pointer("/result/report").cloned().unwrap_or_else(|| {
        let path = route_report_path(route)
            .map(Value::String)
            .unwrap_or(Value::Null);
        json!({"ready": false, "path": path})
    });
    if let Some(worker_name) = worker_name {
        let worker = worker_state_for_session(state, &worker_name);
        return json!({
            "schema_version": 2,
            "route_id": route_id,
            "worker": worker.worker,
            "state": worker.state.as_str(),
            "why": worker.why,
            "can_wait": worker.can_wait,
            "primary_action": worker.primary_action,
            "available_actions": worker.available_actions,
            "debug_actions": worker.debug_actions,
            "patience": worker.patience,
            "report": if report.get("path").is_some() { report } else { worker.report }
        });
    }
    json!({
        "schema_version": 2,
        "route_id": route_id,
        "worker": Value::Null,
        "state": "done",
        "why": "Route did not start a live PTY worker. Start a route without debug-only recommend mode for normal Codex flow.",
        "can_wait": false,
        "primary_action": {"kind": "start_worker"},
        "available_actions": [],
        "debug_actions": [],
        "report": report
    })
}

fn mcp_session(
    state: &AppState,
    args: &Value,
    client: Option<&McpClientContext>,
) -> Result<Value, String> {
    let name = required_str(args, "name")?;
    let include = string_array(args, "include");
    let owner_id = args
        .get("owner_id")
        .and_then(Value::as_str)
        .map(normalize_owner_id)
        .or_else(|| client_owner_id_optional(client));
    let view = args
        .get("view")
        .and_then(Value::as_str)
        .unwrap_or_else(|| legacy_session_view(&include));
    match view {
        "summary" if owner_id.is_some() || include.iter().any(|item| item == "control") => Ok(
            session_summary_view_for_owner(state, name, &include, owner_id.as_deref()),
        ),
        "summary" => Ok(session_summary_view(state, name, &include)),
        "tui" => session_tui_view(state, name, args),
        "events" => session_events(state, name, args, false),
        "debug" => session_debug_view(state, name, args, &include),
        "raw" => session_events(state, name, args, true),
        other => Err(format!("unknown session view: {other}")),
    }
}

fn legacy_session_view(include: &[String]) -> &'static str {
    if include
        .iter()
        .any(|item| matches!(item.as_str(), "clean_tail" | "plan" | "debug"))
    {
        "debug"
    } else if include.iter().any(|item| item == "events") {
        "events"
    } else {
        "summary"
    }
}

fn session_summary_view(state: &AppState, name: &str, include: &[String]) -> Value {
    session_summary_view_for_owner(state, name, include, None)
}

fn session_summary_view_for_owner(
    state: &AppState,
    name: &str,
    include: &[String],
    owner_id: Option<&str>,
) -> Value {
    let projection = session_projection_summary(state, name);
    let wants_screen = include.iter().any(|item| item == "screen");
    let wants_control = include.iter().any(|item| item == "control");
    if !wants_screen {
        let worker = worker_snapshot_for_session(state, name);
        let control = if wants_control {
            slim_control_summary(control_summary_for_session(state, name, owner_id))
        } else {
            no_token_control_summary(state, name)
        };
        return attach_budget(
            worker.to_summary_value(control),
            "summary",
            SUMMARY_VIEW_MAX_BYTES,
            json!({
                "raw_events": true,
                "tool_outputs": true,
                "tool_inputs": true,
                "terminal_tail": true,
                "legacy_projection_fields": true
            }),
        );
    }
    let attention_status = projection
        .get("attention_status")
        .and_then(Value::as_str)
        .unwrap_or("low_confidence");
    let last_progress_brief = projection
        .get("last_progress_brief")
        .cloned()
        .unwrap_or(Value::Null);
    let mut value = json!({
        "schema_version": 1,
        "view": "summary",
        "session": projection.get("session").cloned().unwrap_or_else(|| json!(name)),
        "runtime": projection.get("runtime").cloned().unwrap_or_else(|| json!("unknown")),
        "owner": projection.get("owner").cloned().unwrap_or(Value::Null),
        "liveness": projection.get("liveness_status").cloned().unwrap_or_else(|| json!("unknown")),
        "attention": attention_status,
        "needs_attention": projection.get("needs_attention").cloned().unwrap_or_else(|| json!(attention_status != "none")),
        "attention_reason": attention_reason_from_projection(&projection),
        "last_progress": {
            "brief": last_progress_brief,
            "event_id": Value::Null,
            "global_seq": projection.get("projection_last_global_seq").cloned().unwrap_or_else(|| json!(0)),
            "session_seq": projection.get("projection_last_session_seq").cloned().unwrap_or_else(|| json!(0)),
            "observed_at": projection.get("projection_last_updated_at").cloned().unwrap_or(Value::Null)
        },
        "primary_action": {
            "kind": projection.get("next_recommended_action").cloned().unwrap_or_else(|| json!("inspect_session"))
        },
        "report_ready": projection.get("report_ready").cloned().unwrap_or_else(|| json!(false)),
        "projection_seq": projection.get("projection_last_session_seq").cloned().unwrap_or_else(|| json!(0)),
        "projection_stale": projection.get("projection_stale").cloned().unwrap_or_else(|| json!(true)),
        "control": if wants_control {
            control_summary_for_session(state, name, owner_id)
        } else {
            no_token_control_summary(state, name)
        },
        "workspace": projection.get("workspace").cloned().unwrap_or(Value::Null),
        "claude_cwd": projection.get("claude_cwd").cloned().unwrap_or(Value::Null),
        "warnings": projection.get("warnings").cloned().unwrap_or_else(|| json!([]))
    });
    if wants_screen {
        let terminal = get_session(state, name)
            .map(|session| terminal_snapshot_value(&session, 40))
            .unwrap_or_else(|| {
                json!({
                    "screen_snapshot_available": false,
                    "raw_output_tail_available": false,
                    "reason": "session is not live; screen snapshot requires an in-memory PTY session"
                })
            });
        if let Some(object) = value.as_object_mut() {
            object.insert("terminal".to_string(), terminal);
        }
    }
    attach_budget(
        value,
        "summary",
        SUMMARY_VIEW_MAX_BYTES,
        json!({
            "raw_events": true,
            "tool_outputs": true,
            "tool_inputs": true,
            "terminal_tail": !wants_screen
        }),
    )
}

fn slim_control_summary(control: Value) -> Value {
    if control.get("available").and_then(Value::as_bool) != Some(true) {
        return control;
    }
    json!({
        "available": true,
        "token": control.get("token").cloned().unwrap_or(Value::Null),
        "token_included": control.get("token").and_then(Value::as_str).is_some(),
        "expires_at": control.get("expires_at").cloned().unwrap_or(Value::Null),
        "ttl_seconds": control.get("ttl_seconds").cloned().unwrap_or(Value::Null),
        "token_required_for": ["interrupt", "stop", "kill", "approve_plan", "start_auto"],
    })
}

fn no_token_control_summary(state: &AppState, name: &str) -> Value {
    let live = state.sessions.lock().unwrap().contains_key(name);
    json!({
        "available": live,
        "token_included": false,
        "token_required_for": ["interrupt", "stop", "kill", "approve_plan", "start_auto"]
    })
}

fn session_tui_view(state: &AppState, name: &str, args: &Value) -> Result<Value, String> {
    let projection = session_projection_summary(state, name);
    let attention = projection
        .get("attention_status")
        .and_then(Value::as_str)
        .unwrap_or("low_confidence");
    let liveness = projection
        .get("liveness_status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let events = recent_compact_events(state, name, args, 12)?;
    let policy_block = policy_denials_state(state)
        .get(name)
        .cloned()
        .unwrap_or(Value::Null);
    let route = route_for_wrapper_session(state, name).map(|(_route_id, route)| route);
    let bindings = runtime_bindings_state(state);
    let binding = bindings.get(name);
    let containment = route
        .as_ref()
        .and_then(|route| route.get("result"))
        .and_then(|result| result.get("containment"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let route_status = route
        .as_ref()
        .map(effective_route_status)
        .unwrap_or(Value::Null);
    let prompt_status = route
        .as_ref()
        .map(route_prompt_status)
        .unwrap_or(Value::Null);
    let current_blocker = current_blocker_from_projection(&projection, &policy_block);
    let value = json!({
        "schema_version": 1,
        "view": "tui",
        "session": name,
        "header": {
            "title": name,
            "runtime": projection.get("runtime").cloned().unwrap_or_else(|| json!("unknown")),
            "liveness": liveness,
            "attention": attention,
            "phase": route.as_ref().and_then(|route| route.pointer("/result/phase")).cloned().unwrap_or_else(|| json!("execute")),
            "age": {
                "last_event_seconds": projection.get("last_progress_age_seconds").cloned().unwrap_or_else(|| json!(0)),
                "last_progress_seconds": projection.get("last_progress_age_seconds").cloned().unwrap_or_else(|| json!(0))
            }
        },
        "status": {
            "liveness": liveness,
            "attention": attention,
            "needs_attention": projection.get("needs_attention").cloned().unwrap_or_else(|| json!(attention != "none")),
            "turn_status": projection.get("turn_status").cloned().unwrap_or_else(|| json!("unknown")),
            "patience": projection.get("patience_status").cloned().unwrap_or_else(|| json!("unknown")),
            "report_ready": projection.get("report_ready").cloned().unwrap_or_else(|| json!(false)),
            "route_status": route_status,
            "prompt": prompt_status,
            "binding": {
                "trusted": binding_is_trusted_value(binding),
                "source": binding.and_then(|binding| binding.get("binding_source")).cloned().unwrap_or_else(|| json!("unbound")),
                "last_hook": binding.and_then(|binding| binding.get("last_hook_event")).cloned().unwrap_or(Value::Null),
                "seen_hooks": binding.and_then(|binding| binding.get("seen_hooks")).cloned().unwrap_or_else(|| json!({}))
            }
        },
        "paths": {
            "process_cwd": projection.get("claude_cwd").cloned().unwrap_or(Value::Null),
            "target_workspace": projection.get("workspace").cloned().unwrap_or(Value::Null),
            "scratch_root": containment.get("scratch_root").or_else(|| containment.get("scratch_path")).cloned().unwrap_or(Value::Null),
            "report_path": route.as_ref().and_then(|route| route.pointer("/result/context_packet/report_path")).cloned().unwrap_or(Value::Null),
            "writable_roots": containment.get("writable_roots").or_else(|| containment.get("writable_paths")).cloned().unwrap_or_else(|| json!([])),
            "containment": containment
        },
        "activity": events.get("events").cloned().unwrap_or_else(|| json!([])),
        "current_blocker": current_blocker,
        "primary_action": {
            "kind": projection.get("next_recommended_action").cloned().unwrap_or_else(|| json!("inspect_session")),
            "safe_to_wait": attention == "none",
            "recommended_tool": if attention == "none" { "agentcall_session" } else { "agentcall_session_send" },
            "recommended_args": if attention == "none" {
                json!({"view": "summary"})
            } else {
                json!({"action": "interrupt"})
            }
        },
        "counters": {
            "files_written": projection.get("files_written_count").cloned().unwrap_or_else(|| json!(0)),
            "activity_items": events.get("event_count").cloned().unwrap_or_else(|| json!(0))
        },
        "cursors": {
            "event": events.get("next_cursor").cloned().unwrap_or_else(|| json!(0)),
            "session": projection.get("projection_last_session_seq").cloned().unwrap_or_else(|| json!(0))
        },
        "debug_refs": {
            "events_view": {"view": "events", "cursor": events.get("cursor").cloned().unwrap_or_else(|| json!(0))},
            "raw_view": {"view": "raw"}
        }
    });
    Ok(attach_budget(
        value,
        "tui",
        TUI_VIEW_MAX_BYTES,
        json!({
            "raw_events": true,
            "tool_outputs": true,
            "tool_inputs": true,
            "terminal_tail": true
        }),
    ))
}

fn effective_route_status(route: &Value) -> Value {
    let route_id = route
        .get("route_id")
        .and_then(Value::as_str)
        .unwrap_or("route");
    let gate = prompt_gate_from_route(route_id, route);
    if matches!(
        gate.state.as_str(),
        "prompt_missing" | "prompt_commit_unacknowledged"
    ) {
        return json!(gate.state.as_str());
    }
    json!(
        route
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    )
}

fn route_prompt_status(route: &Value) -> Value {
    let route_id = route
        .get("route_id")
        .and_then(Value::as_str)
        .unwrap_or("route");
    let gate = prompt_gate_from_route(route_id, route);
    json!({
        "state": gate.state.as_str(),
        "task_started": gate.task_started,
        "awaiting_hook": gate.awaiting_hook,
        "ack_deadline_ms": gate.ack_deadline_ms,
        "commit_ack_deadline_ms": gate.commit_ack_deadline_ms,
        "commit_attempts": gate.commit_attempts,
        "can_submit_pending_prompt": gate.can_submit_pending_prompt()
    })
}

fn session_debug_view(
    state: &AppState,
    name: &str,
    args: &Value,
    include: &[String],
) -> Result<Value, String> {
    let wants_clean_tail = include.iter().any(|item| item == "clean_tail")
        || args.get("detail").and_then(Value::as_str) == Some("debug");
    let wants_screen = include.iter().any(|item| item == "screen")
        || args.get("detail").and_then(Value::as_str) == Some("debug");
    let wants_plan = include.iter().any(|item| item == "plan");
    let wants_events = include.iter().any(|item| item == "events");
    let mut response = json!({
        "schema_version": 1,
        "view": "debug",
        "summary": session_summary_view(state, name, &[]),
    });
    let session = if wants_clean_tail || wants_screen || wants_plan {
        Some(get_session(state, name).ok_or_else(|| {
            "session is not live; clean_tail/screen/plan require an in-memory PTY session"
                .to_string()
        })?)
    } else {
        None
    };
    if wants_screen {
        let session = session.as_ref().unwrap();
        response["terminal"] = terminal_snapshot_value(session, 80);
    }
    if wants_clean_tail {
        let session = session.as_ref().unwrap();
        response["clean_tail"] = deprecated_clean_tail_value(session, 80);
    }
    if wants_plan {
        let session = session.as_ref().unwrap();
        response["plan"] = session_plan_artifact(state, session, true);
    }
    if wants_events {
        response["events"] = session_events(state, name, args, false)?;
    }
    Ok(attach_budget(
        response,
        "debug",
        DEBUG_VIEW_MAX_BYTES,
        json!({}),
    ))
}

fn session_events(
    state: &AppState,
    name: &str,
    args: &Value,
    include_raw: bool,
) -> Result<Value, String> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(50)
        .clamp(1, 200) as usize;
    let cursor = args.get("cursor").and_then(Value::as_u64);
    let event_types = string_array(args, "event_types");
    let events = state.store.get_events(EventQuery {
        session_id: Some(name.to_string()),
        after_global_seq: cursor,
        event_types,
        limit,
    })?;
    let next_cursor = events
        .last()
        .map(|event| event.global_seq)
        .unwrap_or_else(|| cursor.unwrap_or(0));
    let values: Vec<Value> = events
        .iter()
        .map(|event| compact_event(event, include_raw))
        .collect();
    let value = json!({
        "schema_version": 1,
        "view": if include_raw { "raw" } else { "events" },
        "session": name,
        "cursor": cursor.unwrap_or(0),
        "next_cursor": next_cursor,
        "limit": limit,
        "event_count": values.len(),
        "events": values,
    });
    Ok(attach_budget(
        value,
        if include_raw { "raw" } else { "events" },
        if include_raw {
            DEBUG_VIEW_MAX_BYTES
        } else {
            EVENTS_VIEW_MAX_BYTES
        },
        json!({
            "raw_events": !include_raw,
            "tool_outputs": !include_raw,
            "tool_inputs": !include_raw,
            "terminal_tail": true
        }),
    ))
}

fn recent_compact_events(
    state: &AppState,
    name: &str,
    args: &Value,
    default_limit: usize,
) -> Result<Value, String> {
    let mut args = args.clone();
    args["limit"] = json!(
        args.get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(default_limit as u64)
    );
    session_events(state, name, &args, false)
}

fn attention_reason_from_projection(projection: &Value) -> Value {
    projection
        .get("last_error_brief")
        .filter(|value| !value.is_null())
        .cloned()
        .or_else(|| projection.get("last_progress_brief").cloned())
        .unwrap_or(Value::Null)
}

fn binding_is_trusted_value(binding: Option<&Value>) -> bool {
    binding
        .and_then(|binding| binding.get("binding_source"))
        .and_then(Value::as_str)
        .is_some_and(|source| matches!(source, "env" | "known_session"))
}

fn current_blocker_from_projection(projection: &Value, policy_block: &Value) -> Value {
    if policy_block
        .get("active")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return json!({
            "kind": "policy",
            "severity": "warning",
            "reason": policy_block.get("reason").cloned().unwrap_or_else(|| json!("policy denial loop")),
            "tool": policy_block.get("tool_name").cloned().unwrap_or(Value::Null),
            "target": policy_block.get("target").cloned().unwrap_or(Value::Null),
            "repeat_count": policy_block.get("repeat_count").cloned().unwrap_or_else(|| json!(1)),
            "recommended_action": "fix_path_policy_or_interrupt",
            "path_diagnosis": policy_block.get("path_diagnosis").cloned().unwrap_or(Value::Null)
        });
    }
    let attention = projection
        .get("attention_status")
        .and_then(Value::as_str)
        .unwrap_or("none");
    if attention == "none" {
        return Value::Null;
    }
    json!({
        "kind": attention,
        "severity": if matches!(attention, "failed" | "terminal") { "critical" } else { "warning" },
        "reason": attention_reason_from_projection(projection),
        "recommended_action": projection.get("next_recommended_action").cloned().unwrap_or_else(|| json!("inspect_session"))
    })
}

fn compact_event(event: &EventEnvelopeV1, include_raw: bool) -> Value {
    let tool = event.payload.get("tool_name").and_then(Value::as_str);
    let kind = compact_event_kind(event, tool);
    let actionability = compact_actionability(&kind, &event.severity);
    let target = compact_event_target(event);
    let mut value = json!({
        "event_id": event.event_id,
        "global_seq": event.global_seq,
        "session_seq": event.session_seq,
        "event_type": event.event_type,
        "kind": kind,
        "severity": event.severity,
        "actionability": actionability,
        "ts": event.ts,
        "summary": compact_event_summary(event, tool, target.as_deref()),
        "tool": tool,
        "target": target,
        "raw_ref": event.event_id
    });
    if let Some((stdout_lines, stderr_lines)) = tool_output_line_counts(&event.payload) {
        value["output"] = json!({
            "stdout_lines": stdout_lines,
            "stderr_lines": stderr_lines,
            "artifact_ref": format!("{}:tool_response", event.event_id)
        });
    }
    if include_raw {
        value["data"] = event.payload.clone();
    }
    value
}

fn compact_event_kind(event: &EventEnvelopeV1, tool: Option<&str>) -> String {
    let decision_allowed = event
        .payload
        .get("decision")
        .and_then(|decision| decision.get("allowed"))
        .and_then(Value::as_bool);
    if decision_allowed == Some(false) || event.event_type.contains("denied") {
        return match tool.unwrap_or("") {
            "Write" | "Edit" | "MultiEdit" => "denied_write",
            "Bash" => "denied_bash",
            _ => "policy_denial",
        }
        .to_string();
    }
    match event.event_type.as_str() {
        "hook.UserPromptSubmit" => "prompt_submitted",
        "hook.PreToolUse" => "tool_start",
        "hook.PostToolUse" | "hook.PostToolBatch" => "tool_output",
        "pty.session_started" | "session.started" | "process.started" => "session_started",
        "process.exited" | "pty.session_ended" => "session_ended",
        "session.actor_failed"
        | "session.writer_failed"
        | "session.writer_closed"
        | "session.reader_failed" => "session_failed",
        event_type if event_type.contains("report") => "report_ready",
        _ => "event",
    }
    .to_string()
}

fn compact_actionability(kind: &str, severity: &str) -> &'static str {
    match kind {
        "session_failed" => "terminal",
        "denied_write" | "denied_bash" | "policy_denial" | "report_ready" => "requires_supervisor",
        "tool_start" | "tool_output" | "prompt_submitted" => "observe",
        _ if matches!(severity, "critical" | "error") => "terminal",
        _ if severity == "warning" => "requires_supervisor",
        _ => "none",
    }
}

fn compact_event_target(event: &EventEnvelopeV1) -> Option<String> {
    let candidates = [
        "/tool_input/file_path",
        "/tool_input/path",
        "/tool_input/target",
        "/decision/path",
        "/decision/target",
        "/path",
        "/target",
    ];
    for pointer in candidates {
        if let Some(value) = event.payload.pointer(pointer).and_then(Value::as_str) {
            return Some(truncate_utf8_owned(value, STRING_PREVIEW_BYTES));
        }
    }
    if let Some(command) = event
        .payload
        .pointer("/tool_input/command")
        .and_then(Value::as_str)
    {
        return Some(truncate_utf8_owned(command, STRING_PREVIEW_BYTES));
    }
    event
        .payload
        .pointer("/decision/files/0")
        .and_then(Value::as_str)
        .map(|value| truncate_utf8_owned(value, STRING_PREVIEW_BYTES))
}

fn compact_event_summary(
    event: &EventEnvelopeV1,
    tool: Option<&str>,
    target: Option<&str>,
) -> String {
    let kind = compact_event_kind(event, tool);
    match kind.as_str() {
        "denied_write" => format!(
            "Write denied{}",
            target.map(|value| format!(": {value}")).unwrap_or_default()
        ),
        "denied_bash" => "Bash denied by policy".to_string(),
        "prompt_submitted" => {
            let chars = event
                .payload
                .get("prompt")
                .or_else(|| event.payload.get("text"))
                .and_then(Value::as_str)
                .map(|text| text.chars().count())
                .unwrap_or(0);
            format!("Prompt submitted, {chars} chars")
        }
        "tool_output" => format!("{} completed", tool.unwrap_or("Tool")),
        "tool_start" => format!("{} started", tool.unwrap_or("Tool")),
        _ => truncate_utf8_owned(&event.message, STRING_PREVIEW_BYTES),
    }
}

fn tool_output_line_counts(payload: &Value) -> Option<(usize, usize)> {
    let response = payload
        .get("tool_response")
        .or_else(|| payload.get("response"))
        .or_else(|| payload.get("output"))?;
    let stdout = response
        .get("stdout")
        .or_else(|| response.get("content"))
        .and_then(Value::as_str)
        .map(str::lines)
        .map(Iterator::count)
        .unwrap_or(0);
    let stderr = response
        .get("stderr")
        .and_then(Value::as_str)
        .map(str::lines)
        .map(Iterator::count)
        .unwrap_or(0);
    Some((stdout, stderr))
}

const BUDGET_METADATA_RESERVE_BYTES: usize = 768;

fn attach_budget(mut value: Value, view: &str, max_bytes: usize, mut omitted: Value) -> Value {
    let original_bytes = json_size(&value);
    let compact_view = matches!(view, "summary" | "tui" | "events");
    let mut trimmed_for_budget = false;
    if compact_view && original_bytes > max_bytes {
        trimmed_for_budget = enforce_json_budget(
            &mut value,
            max_bytes.saturating_sub(BUDGET_METADATA_RESERVE_BYTES),
            &mut omitted,
        );
    }
    insert_budget(
        &mut value,
        view,
        max_bytes,
        original_bytes,
        trimmed_for_budget,
        omitted.clone(),
    );
    if compact_view && json_size(&value) > max_bytes {
        if let Some(object) = value.as_object_mut() {
            object.remove("budget");
        }
        trimmed_for_budget |= enforce_json_budget(
            &mut value,
            max_bytes.saturating_sub(BUDGET_METADATA_RESERVE_BYTES * 2),
            &mut omitted,
        );
        insert_budget(
            &mut value,
            view,
            max_bytes,
            original_bytes,
            trimmed_for_budget,
            omitted,
        );
    }
    if compact_view && json_size(&value) > max_bytes {
        value = json!({
            "schema_version": 1,
            "view": view,
            "budget_notice": "compact view exceeded hard cap after trimming; use view=debug/raw for full diagnostics"
        });
        insert_budget(
            &mut value,
            view,
            max_bytes,
            original_bytes,
            true,
            json!({"compact_payload": true}),
        );
    }
    value
}

fn insert_budget(
    value: &mut Value,
    view: &str,
    max_bytes: usize,
    original_bytes: usize,
    trimmed_for_budget: bool,
    omitted: Value,
) {
    let estimated_bytes = json_size(value);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "budget".to_string(),
            json!({
                "view": view,
                "max_bytes": max_bytes,
                "original_bytes": original_bytes,
                "estimated_bytes": estimated_bytes,
                "response_bytes": Value::Null,
                "truncated": trimmed_for_budget || original_bytes > estimated_bytes,
                "hard_cap_enforced": matches!(view, "summary" | "tui" | "events"),
                "omitted": omitted
            }),
        );
        let response_bytes = json_size(&Value::Object(object.clone()));
        if let Some(budget) = object.get_mut("budget") {
            budget["response_bytes"] = json!(response_bytes);
            budget["truncated"] = json!(
                budget
                    .get("truncated")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                    || response_bytes > max_bytes
            );
        }
    }
}

fn json_size(value: &Value) -> usize {
    serde_json::to_string(value)
        .map(|text| text.len())
        .unwrap_or(0)
}

fn enforce_json_budget(value: &mut Value, cap_bytes: usize, omitted: &mut Value) -> bool {
    let mut changed = false;
    let mut guard = 0;
    while json_size(value) > cap_bytes && guard < 4096 {
        guard += 1;
        if trim_first_array_item(value) {
            increment_omitted_counter(omitted, "budget_trimmed_items");
            changed = true;
            continue;
        }
        if trim_first_long_string(value) {
            increment_omitted_counter(omitted, "budget_trimmed_strings");
            changed = true;
            continue;
        }
        break;
    }
    changed
}

fn trim_first_array_item(value: &mut Value) -> bool {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key == "budget" {
                    continue;
                }
                if let Value::Array(items) = child {
                    if !items.is_empty() {
                        items.pop();
                        return true;
                    }
                }
                if trim_first_array_item(child) {
                    return true;
                }
            }
            false
        }
        Value::Array(items) => {
            if !items.is_empty() {
                items.pop();
                return true;
            }
            false
        }
        _ => false,
    }
}

fn trim_first_long_string(value: &mut Value) -> bool {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if key == "budget" {
                    continue;
                }
                if trim_first_long_string(child) {
                    return true;
                }
            }
            false
        }
        Value::Array(items) => {
            for child in items {
                if trim_first_long_string(child) {
                    return true;
                }
            }
            false
        }
        Value::String(text) if text.len() > STRING_PREVIEW_BYTES => {
            *text = format!(
                "{}...[budget trimmed]",
                truncate_utf8_owned(text, STRING_PREVIEW_BYTES)
            );
            true
        }
        _ => false,
    }
}

fn increment_omitted_counter(omitted: &mut Value, key: &str) {
    if !omitted.is_object() {
        *omitted = json!({});
    }
    let next = omitted.get(key).and_then(Value::as_u64).unwrap_or(0) + 1;
    omitted[key] = json!(next);
}

fn redact_mcp_arguments(value: &Value) -> Value {
    redact_value(value, None)
}

fn redact_value(value: &Value, key: Option<&str>) -> Value {
    match value {
        Value::Object(map) => {
            let mut redacted = serde_json::Map::new();
            for (child_key, child_value) in map {
                redacted.insert(
                    child_key.clone(),
                    redact_value(child_value, Some(child_key)),
                );
            }
            Value::Object(redacted)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| redact_value(item, key)).collect())
        }
        Value::String(text) if should_redact_string(key, text) => json!({
            "redacted": true,
            "chars": text.chars().count(),
            "preview": truncate_utf8_owned(text, STRING_PREVIEW_BYTES)
        }),
        _ => value.clone(),
    }
}

fn should_redact_string(key: Option<&str>, text: &str) -> bool {
    let key = key.unwrap_or("").to_ascii_lowercase();
    matches!(
        key.as_str(),
        "objective" | "text" | "prompt" | "content" | "tool_input" | "command" | "control_token"
    ) || text.len() > 1024
}

fn truncate_utf8_owned(text: &str, cap_bytes: usize) -> String {
    if text.len() <= cap_bytes {
        return text.to_string();
    }
    let mut end = cap_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

fn mcp_session_send(
    state: &AppState,
    args: &Value,
    client: Option<&McpClientContext>,
) -> Result<Value, String> {
    let name = required_str(args, "name")?;
    let action = args.get("action").and_then(Value::as_str).unwrap_or("send");
    let mut args = if args.is_object() {
        args.clone()
    } else {
        json!({})
    };
    if args.get("owner_id").and_then(Value::as_str).is_none() {
        if let Some(owner_id) = client_owner_id_optional(client) {
            args.as_object_mut()
                .unwrap()
                .insert("owner_id".to_string(), json!(owner_id));
        }
    }
    let args = session_send_args_with_default_idempotency(state, name, action, &args)?;
    let args = match session_send_args_with_control_token(state, name, action, &args) {
        Ok(args) => args,
        Err(value) => return Ok(value),
    };
    let user_explicit_close = args
        .get("user_explicit_close")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut command = match prepare_session_send_command(state, name, action, &args)? {
        PreparedCommand::Submit(command) => command,
        PreparedCommand::Deduped(value) => return Ok(value),
    };
    if action == "stop" || action == "kill" || action == "interrupt" {
        return submit_session_command(state, name, command);
    }
    if action == "submit_pending_prompt" {
        let gate = prompt_gate_for_session(state, name);
        if !gate.can_submit_pending_prompt() {
            return Ok(json!({
                "ok": false,
                "status": "prompt_commit_not_allowed",
                "state": gate.state.as_str(),
                "reason": "submit_pending_prompt is only allowed for prompt_missing or prompt_commit_unacknowledged states with attempts remaining.",
                "prompt_gate": gate.to_value(),
                "next_step": "refresh agentcall_session"
            }));
        }
        let route_id = gate
            .route_id
            .clone()
            .ok_or_else(|| "missing route_id for prompt gate".to_string())?;
        let attempt_index = gate.commit_attempts.saturating_add(1);
        let attempt_id = prompt_commit_attempt_id(&route_id, name, attempt_index);
        let sent_at_ms = now_ms();
        command.payload["text"] = json!(" ");
        command.payload["enter"] = json!(true);
        command.payload["attempt_id"] = json!(attempt_id.clone());
        command.payload["prompt_id"] = gate
            .prompt_id
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null);
        let actor_result = submit_session_command(state, name, command)?;
        let attempts = prompt_commit_attempts_for_session(state, name, &attempt_id, sent_at_ms);
        let _ = patch_route_record(
            state,
            &route_id,
            json!({
                "status": "prompt_commit_signal_sent",
                "updated_at": now_ms(),
                "result": {
                    "prompt": {
                        "state": "commit_signal_sent",
                        "task_started": false,
                        "active_commit_attempt_id": attempt_id,
                        "active_commit_sent_at_ms": sent_at_ms,
                        "commit_ack_deadline_ms": DEFAULT_COMMIT_ACK_DEADLINE_MS,
                        "awaiting_hook": "UserPromptSubmit",
                        "commit_attempts": attempts
                    },
                    "prompt_gate": {
                        "schema_version": 2,
                        "state": "commit_signal_sent",
                        "task_started": false,
                        "active_commit_attempt_id": attempt_id,
                        "active_commit_sent_at_ms": sent_at_ms,
                        "commit_ack_deadline_ms": DEFAULT_COMMIT_ACK_DEADLINE_MS,
                        "awaiting_hook": "UserPromptSubmit",
                        "commit_attempts": attempts
                    }
                }
            }),
        );
        return Ok(json!({
            "ok": true,
            "action": action,
            "status": "prompt_commit_signal_sent",
            "not_completed": true,
            "awaiting_hook": "UserPromptSubmit",
            "attempt_id": attempt_id,
            "ack_deadline_ms": DEFAULT_COMMIT_ACK_DEADLINE_MS,
            "next_observation": "agentcall_session(view=summary)",
            "actor_status": actor_result.get("status").cloned().unwrap_or(Value::Null)
        }));
    }
    if action == "approve_plan" || action == "start_auto" {
        if !is_plan_then_auto_session(state, name) {
            return Err("session is not a plan_then_auto PTY route".to_string());
        }
        command.payload["text"] = json!("1");
        command.payload["enter"] = json!(true);
        let result = submit_session_command(state, name, command)?;
        update_pty_workflow_route(
            state,
            name,
            "auto_running",
            "auto",
            "approved via session_send action",
        )?;
        return Ok(
            json!({"ok": true, "action": action, "workflow_status": "auto_running", "actor_result": result}),
        );
    }
    if action == "select_option" {
        let choice = menu_choice(&args)?;
        let session = get_session(state, name).ok_or_else(|| "session not found".to_string())?;
        let process_status = session.status.lock().unwrap().clone();
        if process_status != "running" {
            return Ok(json!({
                "ok": false,
                "status": "session_not_accepting_input",
                "process_status": process_status,
                "hint": "The PTY process is not running. Inspect session summary/report before selecting a menu option."
            }));
        }
        let summary = session_summary(state, &session);
        let attention_status = summary
            .get("attention_status")
            .and_then(Value::as_str)
            .unwrap_or("none");
        let clean_output = clean_session_output(&session);
        if attention_status != "needs_permission" && !looks_like_menu_prompt(&clean_output) {
            return Ok(json!({
                "ok": false,
                "status": "not_in_menu_prompt",
                "attention_status": attention_status,
                "hint": "select_option is only for visible PTY menus or permission prompts. Use send for normal natural-language input."
            }));
        }
        command.payload["text"] = json!(choice.clone());
        command.payload["enter"] = json!(true);
        let actor_result = submit_session_command(state, name, command)?;
        append_agent_event(
            state,
            "pty.menu_option_selected",
            "PTY menu option selected by supervisor.",
            json!({
                "name": name,
                "choice": choice,
                "attention_status": attention_status,
                "runtime": "pty"
            }),
        );
        return Ok(json!({
            "ok": true,
            "action": action,
            "status": "menu_option_selected",
            "choice": choice,
            "actor_result": actor_result,
            "hint": "Supervisor selected a visible PTY menu option; inspect session summary before sending further input."
        }));
    }
    let text = args
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| match action {
            "continue" => "Continue from the current state. If the task is complete, write the requested report now.".to_string(),
            "request_report" => "Stop new implementation work and write the requested report with exact changes, tests, failures, and remaining risks.".to_string(),
            "revise_plan" => "Revise the current plan according to the latest supervisor feedback. Stay in plan mode and use ExitPlanMode again when ready.".to_string(),
            _ => "".to_string(),
        });
    if text.is_empty() {
        return Err("missing text for send action".to_string());
    }
    let session = get_session(state, name).ok_or_else(|| "session not found".to_string())?;
    let process_status = session.status.lock().unwrap().clone();
    if process_status != "running" {
        return Ok(json!({
            "ok": false,
            "status": "session_not_accepting_input",
            "process_status": process_status,
            "hint": "The PTY process is not running. Inspect session summary/report before sending more input."
        }));
    }
    let worker_state = worker_state_for_session(state, name);
    if matches!(
        worker_state.state,
        WorkerStateKind::PromptPending
            | WorkerStateKind::PromptMissing
            | WorkerStateKind::PromptCommitUnacknowledged
    ) {
        return Ok(json!({
            "ok": false,
            "status": if worker_state.state == WorkerStateKind::PromptPending { "inside_patience_window" } else { "prompt_not_submitted" },
            "state": worker_state.state.as_str(),
            "reason": worker_state.why,
            "can_wait": worker_state.can_wait,
            "primary_action": worker_state.primary_action,
            "available_actions": worker_state.available_actions,
            "debug_actions": worker_state.debug_actions,
            "patience": worker_state.patience,
            "prompt_gate": worker_state.prompt_gate.to_value(),
            "hint": "The route prompt has not been acknowledged by UserPromptSubmit or worker progress. Do not queue supervisor text; follow primary_action unless using debug_actions for recovery."
        }));
    }
    let summary = session_summary(state, &session);
    if action == "request_report"
        && matches!(
            worker_state.state,
            WorkerStateKind::ReportRequested | WorkerStateKind::ReportDrafting
        )
    {
        return Ok(report_request_already_active(&worker_state));
    }
    if matches!(action, "request_report" | "continue") && !user_explicit_close {
        if let Some(response) = inside_patience_window_response(name, action, &worker_state) {
            return Ok(response);
        }
    }
    let liveness_status = summary
        .get("liveness_status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let attention_status = summary
        .get("attention_status")
        .and_then(Value::as_str)
        .unwrap_or("none");
    if attention_status == "needs_permission" {
        return Ok(json!({
            "ok": false,
            "status": "blocked_by_permission_prompt",
            "liveness_status": liveness_status,
            "attention_status": attention_status,
            "hint": "Claude Code is showing a permission prompt. Do not send natural-language input into the menu; resolve the permission prompt or use action=interrupt only to reclaim a drifting worker."
        }));
    }
    if attention_status == "blocked_by_policy" {
        return Ok(json!({
            "ok": false,
            "status": "blocked_by_policy",
            "liveness_status": liveness_status,
            "attention_status": attention_status,
            "policy_block": summary.get("policy_block").cloned().unwrap_or(Value::Null),
            "hint": "The worker is repeating a denied action. Do not wait or resend the same prompt; adjust write_paths or task scope, add reference_paths as context if useful, request a blocker report after interrupt, or stop the worker."
        }));
    }
    if liveness_status == "working" && attention_status == "none" {
        command.command_type = CommandType::QueueSupervisorInstruction;
        command.payload["text"] = json!(text);
        command.payload["action"] = json!(action);
        let mut actor_result = submit_session_command(state, name, command)?;
        if action == "request_report" {
            let report = mark_report_requested(state, name)?;
            actor_result["ok"] = json!(true);
            actor_result["action"] = json!(action);
            actor_result["status"] = json!("report_requested");
            actor_result["not_completed"] = json!(true);
            actor_result["awaiting"] = json!("report_write");
            actor_result["report"] = report;
        }
        let post_tool_batch_seen = session_has_seen_hook_event(state, name, "PostToolBatch");
        let warning = if post_tool_batch_seen {
            Value::Null
        } else {
            json!(
                "This session has not emitted PostToolBatch in recent events. Queued instructions may remain pending until the worker is restarted with updated D:\\guKimi hooks."
            )
        };
        actor_result["post_tool_batch_seen"] = json!(post_tool_batch_seen);
        actor_result["warning"] = warning;
        return Ok(actor_result);
    }
    command.payload["text"] = json!(text);
    if let Some(enter) = args.get("enter").and_then(Value::as_bool) {
        command.payload["enter"] = json!(enter);
    }
    let actor_result = submit_session_command(state, name, command)?;
    if action == "request_report" {
        let report = mark_report_requested(state, name)?;
        return Ok(json!({
            "ok": true,
            "action": action,
            "status": "report_requested",
            "not_completed": true,
            "awaiting": "report_write",
            "actor_result": actor_result,
            "report": report
        }));
    }
    if action == "revise_plan" {
        let _ = update_pty_workflow_route(
            state,
            name,
            "plan_revision_requested",
            "plan",
            "revision requested via session_send action",
        );
    }
    Ok(actor_result)
}

fn report_request_already_active(worker_state: &crate::worker_state::WorkerStateView) -> Value {
    json!({
        "ok": true,
        "action": "request_report",
        "status": worker_state.report.get("status").cloned().unwrap_or_else(|| json!("report_requested")),
        "not_completed": true,
        "awaiting": "report_write",
        "state": worker_state.state.as_str(),
        "why": worker_state.why,
        "primary_action": worker_state.primary_action,
        "available_actions": worker_state.available_actions,
        "debug_actions": worker_state.debug_actions,
        "report": worker_state.report,
        "hint": "A report request is already active. Do not issue another request_report; wait for report_ready or report_overdue."
    })
}

fn inside_patience_window_response(
    session_name: &str,
    action: &str,
    worker_state: &crate::worker_state::WorkerStateView,
) -> Option<Value> {
    if !matches!(
        worker_state.state,
        WorkerStateKind::Starting | WorkerStateKind::PromptSubmitted | WorkerStateKind::Working
    ) {
        return None;
    }
    if worker_state.patience.get("status").and_then(Value::as_str) != Some("inside_patience_window")
    {
        return None;
    }
    Some(json!({
        "ok": false,
        "action": action,
        "status": "inside_patience_window",
        "session": session_name,
        "state": worker_state.state.as_str(),
        "why": worker_state.why,
        "primary_action": worker_state.primary_action,
        "available_actions": worker_state.available_actions,
        "debug_actions": worker_state.debug_actions,
        "patience": worker_state.patience,
        "hint": "The worker is healthy and inside the daemon-enforced patience window. Wait instead of sending continue/request_report, unless the human explicitly asks to close with user_explicit_close=true."
    }))
}

fn mark_report_requested(state: &AppState, session_name: &str) -> Result<Value, String> {
    let Some((route_id, route)) = route_for_wrapper_session(state, session_name) else {
        return Err("cannot request report: session has no route record".to_string());
    };
    let now = now_ms();
    let deadline = now + REPORT_REQUEST_DEADLINE_MS;
    let request_id = format!(
        "report-request-{}-{}",
        route_id,
        stable_hash_hex(&format!("{session_name}:{now}"))
    );
    let report = report_block_from_route(&route);
    let patch_report = json!({
        "status": "report_requested",
        "ready": false,
        "request_id": request_id,
        "requested_at_ms": now,
        "deadline_at_ms": deadline,
        "requested_by": "agentcall_session_send",
        "path": report.get("path").cloned().unwrap_or(Value::Null),
        "rel_path": report.get("rel_path").cloned().unwrap_or_else(|| report.get("path").cloned().unwrap_or(Value::Null)),
        "abs_path": report.get("abs_path").cloned().unwrap_or(Value::Null),
        "target_workspace": report.get("target_workspace").cloned().unwrap_or(Value::Null),
        "source": report.get("source").cloned().unwrap_or(Value::Null)
    });
    patch_route_record(
        state,
        &route_id,
        json!({
            "status": "report_requested",
            "updated_at": now,
            "required_next_step": "wait_for_report_or_inspect_session",
            "result": {
                "report": patch_report,
                "report_request": {
                    "request_id": request_id,
                    "status": "report_requested",
                    "requested_at_ms": now,
                    "deadline_at_ms": deadline
                }
            }
        }),
    )?;
    Ok(patch_report)
}

fn report_block_from_route(route: &Value) -> Value {
    route.pointer("/result/report").cloned().unwrap_or_else(|| {
        let path = route_report_path(route)
            .map(Value::String)
            .unwrap_or(Value::Null);
        json!({
            "status": "report_not_requested",
            "ready": false,
            "path": path,
            "rel_path": path,
            "abs_path": Value::Null,
            "target_workspace": route.get("workspace").cloned().unwrap_or(Value::Null),
            "source": "unknown"
        })
    })
}

fn route_report_path(route: &Value) -> Option<String> {
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

fn prompt_commit_attempts_for_session(
    state: &AppState,
    session_name: &str,
    attempt_id: &str,
    sent_at_ms: u64,
) -> Value {
    let mut attempts = route_for_wrapper_session(state, session_name)
        .and_then(|(_, route)| {
            route
                .pointer("/result/prompt_gate/commit_attempts")
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default();
    let next_index = attempts.len() + 1;
    attempts.push(json!({
        "attempt_id": attempt_id,
        "kind": "manual_mcp",
        "state": "signal_sent",
        "sent_at_ms": sent_at_ms,
        "ack_deadline_ms": DEFAULT_COMMIT_ACK_DEADLINE_MS,
        "index": next_index,
    }));
    Value::Array(attempts)
}

fn session_send_args_with_control_token(
    state: &AppState,
    session: &str,
    action: &str,
    args: &Value,
) -> Result<Value, Value> {
    if !destructive_action_requires_control(action) {
        return Ok(args.clone());
    }
    let Some(token) = args
        .get("control_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(json!({
            "ok": false,
            "status": "missing_control_token",
            "reason": format!("action {action} requires a fresh daemon-minted control token"),
            "next_step": "call agentcall_session(view=summary), read control.token, then retry quickly",
            "current": {
                "session_id": session,
                "action": action,
                "control": "missing"
            }
        }));
    };
    let validated =
        validate_control_token(state, session, action, token).map_err(|err| err.to_value())?;
    let mut args = args.clone();
    if !args.is_object() {
        args = json!({});
    }
    let object = args.as_object_mut().unwrap();
    object.insert(
        "owner_id".to_string(),
        json!(validated.claims.owner_id.clone()),
    );
    object.insert(
        "owner_lease_id".to_string(),
        json!(validated.claims.owner_lease_id.clone()),
    );
    object.insert(
        "lease_generation".to_string(),
        json!(validated.claims.lease_generation),
    );
    object.insert(
        "control_epoch".to_string(),
        json!(validated.claims.control_epoch),
    );
    object.insert(
        "control_token_hash".to_string(),
        json!(validated.token_hash),
    );
    object.remove("control_token");
    Ok(args)
}

fn menu_choice(args: &Value) -> Result<String, String> {
    let choice = args
        .get("text")
        .or_else(|| args.get("choice"))
        .and_then(Value::as_str)
        .map(str::trim)
        .ok_or_else(|| {
            "select_option requires text to be one digit, such as \"1\", \"2\", or \"3\""
                .to_string()
        })?;
    if choice.len() == 1 && matches!(choice.as_bytes()[0], b'1'..=b'9') {
        Ok(choice.to_string())
    } else {
        Err("select_option text must be exactly one digit from 1 to 9".to_string())
    }
}

fn session_send_args_with_default_idempotency(
    state: &AppState,
    session: &str,
    action: &str,
    args: &Value,
) -> Result<Value, String> {
    let mut args = if args.is_object() {
        args.clone()
    } else {
        json!({})
    };
    let has_key = args
        .get("idempotency_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if has_key {
        return Ok(args);
    }
    let key = default_session_send_idempotency_key(state, session, action, &args);
    args.as_object_mut()
        .ok_or_else(|| "session_send arguments must be an object".to_string())?
        .insert("idempotency_key".to_string(), json!(key));
    Ok(args)
}

fn default_session_send_idempotency_key(
    state: &AppState,
    session: &str,
    action: &str,
    args: &Value,
) -> String {
    let projection = session_projection_summary(state, session);
    let projection_seq = projection
        .get("projection_last_session_seq")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut fingerprint_args = args.clone();
    if let Some(object) = fingerprint_args.as_object_mut() {
        object.remove("idempotency_key");
        object.remove("root");
    }
    let fingerprint = serde_json::to_string(&fingerprint_args).unwrap_or_default();
    format!(
        "mcp-{}-{}-{}-{}",
        idempotency_key_part(session, 40),
        idempotency_key_part(action, 24),
        projection_seq,
        stable_hash_hex(&fingerprint)
    )
}

fn idempotency_key_part(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for ch in value.chars().take(max_chars) {
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

fn stable_hash_hex(value: &str) -> String {
    sha256_hex(value)
}

fn looks_like_menu_prompt(clean_output: &str) -> bool {
    let tail = clean_output
        .chars()
        .rev()
        .take(4000)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .to_ascii_lowercase();
    tail.contains("run a dynamic workflow?")
        || tail.contains("yes, run it")
        || tail.contains("view raw script")
        || tail.contains("esc to cancel")
        || tail.contains("tab to amend")
}

fn session_has_seen_hook_event(state: &AppState, wrapper_session: &str, hook_event: &str) -> bool {
    runtime_bindings_state(state)
        .get(wrapper_session)
        .and_then(|binding| binding.get("seen_hooks"))
        .and_then(|seen| seen.get(hook_event))
        .and_then(Value::as_bool)
        == Some(true)
}

fn is_plan_then_auto_session(state: &AppState, session_name: &str) -> bool {
    let Some((_route_id, route)) = route_for_wrapper_session(state, session_name) else {
        return false;
    };
    route.get("recommended_runtime").and_then(Value::as_str) == Some("pty")
        && route
            .get("result")
            .and_then(|result| result.get("pty_workflow"))
            .and_then(Value::as_str)
            == Some("plan_then_auto")
}

fn update_pty_workflow_route(
    state: &AppState,
    session_name: &str,
    workflow_status: &str,
    permission_mode: &str,
    reason: &str,
) -> Result<(), String> {
    let Some((route_id, route)) = route_for_wrapper_session(state, session_name) else {
        return Ok(());
    };
    if route.get("recommended_runtime").and_then(Value::as_str) != Some("pty") {
        return Ok(());
    }
    patch_route_record(
        state,
        &route_id,
        json!({
            "status": workflow_status,
            "updated_at": crate::util::now_ms(),
            "result": {
                "workflow_status": workflow_status,
                "phase": if permission_mode == "auto" { "execute" } else { "plan" },
                "permission_mode": permission_mode,
                "mode_source": "session_send",
                "last_control_action": reason
            }
        }),
    )
}

fn mcp_report(
    state: &Arc<AppState>,
    args: &Value,
    _client: Option<&McpClientContext>,
) -> Result<Value, String> {
    match args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("accept")
    {
        "request" => {
            let session_id = required_str(args, "session_id")?;
            checkpoint_session(state, session_id)
        }
        "accept" => {
            if let Some(session_id) = args.get("session_id").and_then(Value::as_str) {
                return Ok(accept_report_for_session(state, session_id));
            }
            let reports = board_state(state, None, None, Some("reports"), None, None)
                .get("reports")
                .cloned()
                .unwrap_or_else(|| json!([]));
            if let Some(task_id) = args.get("task_id").and_then(Value::as_str) {
                let filtered = reports
                    .as_array()
                    .map(|items| {
                        items
                            .iter()
                            .filter(|item| {
                                item.get("task_id").and_then(Value::as_str) == Some(task_id)
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                Ok(attach_confidence_to_reports(state, json!(filtered)))
            } else {
                Ok(attach_confidence_to_reports(state, reports))
            }
        }
        other => Err(format!("unknown report action: {other}")),
    }
}

fn accept_report_for_session(state: &AppState, session_id: &str) -> Value {
    let route =
        route_for_wrapper_session(state, session_id).map(|(route_id, route)| (route_id, route));
    let target_workspace = route
        .as_ref()
        .and_then(|(_, route)| route.get("workspace"))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| state.workspace.clone());
    let report_path = route
        .as_ref()
        .and_then(|(_, route)| route_report_path(route));
    let report_abs = report_path.as_deref().map(|path| {
        let candidate = PathBuf::from(path);
        if candidate.is_absolute() {
            candidate
        } else {
            target_workspace.join(candidate)
        }
    });
    let exists = report_abs.as_ref().is_some_and(|path| path.exists());
    let non_empty = report_abs
        .as_ref()
        .and_then(|path| std::fs::metadata(path).ok())
        .is_some_and(|metadata| metadata.len() > 0);
    let matched_route_report_path = report_abs.is_some();
    let daemon_observed_write = route
        .as_ref()
        .and_then(|(_, route)| route.pointer("/result/report_ready"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let accepted = exists && non_empty && matched_route_report_path;
    let artifact_confidence = if exists && non_empty { "high" } else { "low" };
    let route_match_confidence = if matched_route_report_path {
        "high"
    } else {
        "low"
    };
    let daemon_write_confidence = if daemon_observed_write { "high" } else { "low" };
    let overall_confidence = if accepted && daemon_observed_write {
        "high"
    } else if accepted {
        "medium"
    } else {
        "low"
    };
    if accepted {
        if let Some((route_id, _)) = route.as_ref() {
            let _ = patch_route_record(
                state,
                route_id,
                json!({
                    "status": "report_accepted",
                    "updated_at": now_ms(),
                    "result": {
                        "report": {
                            "status": "report_accepted",
                            "accepted": true,
                            "accepted_at_ms": now_ms()
                        }
                    }
                }),
            );
        }
    }
    json!({
        "ok": accepted,
        "status": if accepted { "accepted" } else { "report_missing_or_empty" },
        "worker": session_id,
        "session_id": session_id,
        "route_id": route.as_ref().map(|(route_id, _)| route_id.clone()),
        "report_path": report_path,
        "report_rel_path": report_path,
        "target_workspace": target_workspace.display().to_string(),
        "report_abs_path": report_abs
            .as_ref()
            .map(|path| path.display().to_string()),
        "validation": {
            "exists": exists,
            "non_empty": non_empty,
            "matched_route_report_path": matched_route_report_path,
            "daemon_observed_write": daemon_observed_write,
            "report_abs_path": report_abs
                .as_ref()
                .map(|path| path.display().to_string())
        },
        "confidence": {
            "overall": overall_confidence,
            "artifact": artifact_confidence,
            "daemon_write": daemon_write_confidence,
            "route_match": route_match_confidence
        },
        "primary_action": {
            "kind": if accepted { "stop_worker" } else { "inspect_session_or_request_report" }
        }
    })
}

fn required_str<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required argument: {name}"))
}

fn string_array(args: &Value, name: &str) -> Vec<String> {
    args.get(name)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::{ActorControlCommand, ActorHandle};
    use std::sync::mpsc;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_workspace(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("agentcall-mcp-{name}-{nonce}"))
    }

    fn install_prompt_gate_route(
        state: &AppState,
        route_id: &str,
        session_name: &str,
        gate: Value,
    ) {
        let state_dir = state.workspace.join(".agentcall").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        crate::state::write_json_file(
            &state_dir.join("routes.json"),
            &json!({
                route_id: {
                    "route_id": route_id,
                    "owner_id": "codex",
                    "recommended_runtime": "pty",
                    "runtime": "pty",
                    "session_name": session_name,
                    "workspace": state.workspace.display().to_string(),
                    "status": gate.get("state").and_then(Value::as_str).unwrap_or("prompt_pending_ack"),
                    "result": {
                        "phase": "execute",
                        "prompt_gate": gate
                    }
                }
            }),
        )
        .unwrap();
    }

    fn install_ok_actor(state: &AppState, session_name: &str) {
        let (tx, rx) = mpsc::channel();
        state.actors.lock().unwrap().insert(
            session_name.to_string(),
            ActorHandle {
                session_id: session_name.to_string(),
                sender: tx,
            },
        );
        thread::spawn(move || {
            let ActorControlCommand::Submit(command, reply) = rx.recv().unwrap() else {
                return;
            };
            let _ = reply.send(Ok(json!({
                "ok": true,
                "status": "command_completed",
                "command_id": command.command_id,
                "command_type": format!("{:?}", command.command_type),
            })));
        });
    }

    #[test]
    fn daemon_mcp_tools_are_canonical_only() {
        let names: Vec<String> = mcp_tools()
            .into_iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "agentcall_board",
                "agentcall_route",
                "agentcall_session",
                "agentcall_session_send",
                "agentcall_report",
            ]
        );
    }

    #[test]
    fn daemon_session_tool_schema_allows_explicit_debug_includes() {
        let tools = mcp_tools();
        let session_tool = tools
            .iter()
            .find(|tool| tool["name"] == "agentcall_session")
            .unwrap();
        let include_enum = session_tool["inputSchema"]["properties"]["include"]["items"]["enum"]
            .as_array()
            .unwrap();
        assert!(include_enum.iter().any(|item| item == "control"));
        assert!(include_enum.iter().any(|item| item == "clean_tail"));
        assert!(include_enum.iter().any(|item| item == "plan"));
        assert!(include_enum.iter().any(|item| item == "debug"));
        assert!(include_enum.iter().any(|item| item == "policy"));
        assert!(
            session_tool["inputSchema"]["properties"]
                .get("cursor")
                .is_some()
        );
        assert!(
            session_tool["inputSchema"]["properties"]
                .get("event_types")
                .is_some()
        );
    }

    #[test]
    fn mcp_route_records_client_owner_id() {
        let state = Arc::new(AppState::test(test_workspace("mcp-route-owner")));

        let result = mcp_call(
            &state,
            McpCallRequest {
                name: "agentcall_route".to_string(),
                arguments: Some(json!({
                    "objective": "recommend a worker",
                    "mode": "recommend"
                })),
                client: Some(McpClientContext {
                    owner_id: Some("codex-thread-owner-a".to_string()),
                }),
            },
        )
        .unwrap();

        let route_id = result["route_id"].as_str().unwrap();
        let routes = crate::state::read_json_file(
            &state
                .workspace
                .join(".agentcall")
                .join("state")
                .join("routes.json"),
            json!({}),
        );
        assert_eq!(routes[route_id]["owner_id"], "codex-thread-owner-a");
    }

    #[test]
    fn mcp_board_compact_ignores_scope_all_without_debug_view() {
        let root = test_workspace("board-owner-safe-compact");
        let state = AppState::test(root.clone());
        let result = mcp_board(
            &state,
            &json!({"view": "compact", "filter": "attention", "scope": "all"}),
            Some(&McpClientContext {
                owner_id: Some("codex-owner-a".to_string()),
            }),
        )
        .unwrap();
        assert_eq!(result["view"], "compact");
        assert_eq!(result["owner"]["bound"], true);
        assert_eq!(result["owner"]["owner_id"], "codex-owner-a");
        assert_eq!(result["owner"]["scope"], "mine");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn mcp_board_full_scope_all_is_explicit_debug_global() {
        let root = test_workspace("board-owner-debug-global");
        let state = AppState::test(root.clone());
        let result = mcp_board(
            &state,
            &json!({"view": "full", "scope": "all", "section": "sessions"}),
            Some(&McpClientContext {
                owner_id: Some("codex-owner-a".to_string()),
            }),
        )
        .unwrap();
        assert_eq!(result["workspace_filter_applied"], false);
        assert!(result.get("runtime_sessions").is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn daemon_route_tool_schema_hides_debug_runtime_knobs() {
        let tools = mcp_tools();
        let route_tool = tools
            .iter()
            .find(|tool| tool["name"] == "agentcall_route")
            .unwrap();
        let properties = &route_tool["inputSchema"]["properties"];
        assert!(properties.get("objective").is_some());
        assert!(properties.get("workspace").is_some());
        assert!(properties.get("write_paths").is_some());
        assert!(properties.get("allowed_paths").is_none());
        assert!(properties.get("report_path").is_some());
        for hidden in [
            "mode",
            "runtime",
            "estimated_minutes",
            "estimated_files",
            "estimated_loc",
            "needs_continuity",
            "risk",
            "pty_workflow",
            "initial_permission_mode",
            "persist_context",
            "task_id",
            "call_id",
            "phase",
            "role",
            "read_only",
        ] {
            assert!(
                properties.get(hidden).is_none(),
                "{hidden} leaked into recommended route schema"
            );
        }
    }

    #[test]
    fn session_events_uses_store_cursor_and_filters() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-session-events-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        append_agent_event(
            &state,
            "hook.Notification",
            "permission",
            json!({"wrapper_session": "worker-a", "status": "needs_permission"}),
        );
        append_agent_event(
            &state,
            "hook.Stop",
            "idle",
            json!({"wrapper_session": "worker-a", "status": "idle"}),
        );
        let events = session_events(
            &state,
            "worker-a",
            &json!({"cursor": 0, "limit": 10, "event_types": ["hook.Notification"]}),
            false,
        )
        .unwrap();
        assert_eq!(events["event_count"], 1);
        assert_eq!(events["events"][0]["event_type"], "hook.Notification");
        assert_eq!(events["next_cursor"], 1);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn default_session_reads_projection_without_live_pty() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-session-projection-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        append_agent_event(
            &state,
            "hook.Notification",
            "permission",
            json!({"wrapper_session": "worker-a", "status": "needs_permission"}),
        );

        let summary = mcp_session(&state, &json!({"name": "worker-a"}), None).unwrap();
        assert_eq!(summary["view"], "summary");
        assert_eq!(summary["schema_version"], 2);
        assert_eq!(summary["worker"], "worker-a");
        assert_eq!(summary["state"], "done");
        assert_eq!(summary["can_wait"], false);
        assert_eq!(summary["control"]["token_included"], false);
        assert!(summary["control"].get("token").is_none());
        assert!(summary.get("clean_tail").is_none());
        assert!(summary.get("events").is_none());
        assert_eq!(summary["budget"]["view"], "summary");

        let summary_ignores_debug_include = mcp_session(
            &state,
            &json!({"name": "worker-a", "view": "summary", "include": ["clean_tail", "events"]}),
            None,
        )
        .unwrap();
        assert_eq!(summary_ignores_debug_include["view"], "summary");
        assert!(summary_ignores_debug_include.get("clean_tail").is_none());
        assert!(summary_ignores_debug_include.get("events").is_none());

        let err = mcp_session(
            &state,
            &json!({"name": "worker-a", "view": "debug", "include": ["clean_tail"]}),
            None,
        )
        .unwrap_err();
        assert!(err.contains("session is not live"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn session_send_mcp_generates_stable_idempotency_key_when_omitted() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-session-idempotency-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        append_agent_event(
            &state,
            "hook.SessionStart",
            "started",
            json!({"wrapper_session": "worker-a", "status": "working"}),
        );
        let args = json!({"name": "worker-a", "action": "continue"});

        let first =
            session_send_args_with_default_idempotency(&state, "worker-a", "continue", &args)
                .unwrap();
        let second =
            session_send_args_with_default_idempotency(&state, "worker-a", "continue", &args)
                .unwrap();

        let key = first["idempotency_key"].as_str().unwrap();
        assert!(key.starts_with("mcp-worker-a-continue-"));
        assert_eq!(first["idempotency_key"], second["idempotency_key"]);
        assert_ne!(key, "implicit-action");

        let with_root = session_send_args_with_default_idempotency(
            &state,
            "worker-a",
            "continue",
            &json!({"name": "worker-a", "action": "continue", "root": "E:/elsewhere"}),
        )
        .unwrap();
        assert_eq!(first["idempotency_key"], with_root["idempotency_key"]);

        append_agent_event(
            &state,
            "hook.Stop",
            "idle",
            json!({"wrapper_session": "worker-a", "status": "idle"}),
        );
        let after_progress =
            session_send_args_with_default_idempotency(&state, "worker-a", "continue", &args)
                .unwrap();
        assert_ne!(first["idempotency_key"], after_progress["idempotency_key"]);

        let explicit = session_send_args_with_default_idempotency(
            &state,
            "worker-a",
            "continue",
            &json!({"name": "worker-a", "action": "continue", "idempotency_key": "caller-key"}),
        )
        .unwrap();
        assert_eq!(explicit["idempotency_key"], "caller-key");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn submit_pending_prompt_returns_signal_sent_contract() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-submit-pending-v61-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        install_prompt_gate_route(
            &state,
            "route-pty",
            "worker-a",
            json!({
                "schema_version": 2,
                "state": "prompt_missing",
                "task_started": false,
                "prompt_id": "route_prompt:route-pty:worker-a",
                "prompt_written_at_ms": now_ms().saturating_sub(20_000),
                "awaiting_hook": "UserPromptSubmit",
                "ack_deadline_ms": 15_000,
                "commit_ack_deadline_ms": DEFAULT_COMMIT_ACK_DEADLINE_MS,
                "commit_attempts": []
            }),
        );
        install_ok_actor(&state, "worker-a");

        let result = mcp_session_send(
            &state,
            &json!({"name": "worker-a", "action": "submit_pending_prompt"}),
            None,
        )
        .unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["status"], "prompt_commit_signal_sent");
        assert_eq!(result["not_completed"], true);
        assert_eq!(result["awaiting_hook"], "UserPromptSubmit");
        assert!(
            result["attempt_id"]
                .as_str()
                .unwrap()
                .starts_with("prompt-commit-route-pty-worker-a-")
        );
        let rendered = serde_json::to_string(&result).unwrap();
        assert!(!rendered.contains("pending_prompt_commit_sent"));

        let routes = crate::state::read_json_file(
            &state
                .workspace
                .join(".agentcall")
                .join("state")
                .join("routes.json"),
            json!({}),
        );
        let route = routes.get("route-pty").unwrap();
        assert_eq!(route["status"], "prompt_commit_signal_sent");
        assert_eq!(
            route["result"]["prompt_gate"]["state"],
            "commit_signal_sent"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prompt_gate_refresh_auto_submits_missing_prompt() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-auto-submit-pending-v63-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        install_prompt_gate_route(
            &state,
            "route-pty",
            "worker-a",
            json!({
                "schema_version": 2,
                "state": "prompt_missing",
                "task_started": false,
                "prompt_id": "route_prompt:route-pty:worker-a",
                "prompt_written_at_ms": now_ms().saturating_sub(20_000),
                "awaiting_hook": "UserPromptSubmit",
                "ack_deadline_ms": 15_000,
                "commit_ack_deadline_ms": DEFAULT_COMMIT_ACK_DEADLINE_MS,
                "commit_attempts": []
            }),
        );
        install_ok_actor(&state, "worker-a");

        let gate = crate::prompt_gate::refresh_prompt_gate_timeouts_for_session(&state, "worker-a");

        assert_eq!(
            gate.state,
            crate::prompt_gate::PromptGateState::CommitSignalSent
        );
        let routes = crate::state::read_json_file(
            &state
                .workspace
                .join(".agentcall")
                .join("state")
                .join("routes.json"),
            json!({}),
        );
        let route = routes.get("route-pty").unwrap();
        assert_eq!(route["status"], "prompt_commit_signal_sent");
        assert_eq!(
            route["result"]["prompt_gate"]["commit_attempts"][0]["kind"],
            "daemon_auto"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prompt_gate_refresh_auto_submits_pending_prompt_after_short_grace() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-auto-submit-pending-ack-v66-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        install_prompt_gate_route(
            &state,
            "route-pty",
            "worker-a",
            json!({
                "schema_version": 2,
                "state": "prompt_pending_ack",
                "task_started": false,
                "prompt_id": "route_prompt:route-pty:worker-a",
                "prompt_written_at_ms": now_ms().saturating_sub(crate::prompt_gate::DEFAULT_AUTO_COMMIT_GRACE_MS + 100),
                "awaiting_hook": "UserPromptSubmit",
                "ack_deadline_ms": 15_000,
                "commit_ack_deadline_ms": DEFAULT_COMMIT_ACK_DEADLINE_MS,
                "commit_attempts": []
            }),
        );
        install_ok_actor(&state, "worker-a");

        let gate = crate::prompt_gate::refresh_prompt_gate_timeouts_for_session(&state, "worker-a");

        assert_eq!(
            gate.state,
            crate::prompt_gate::PromptGateState::CommitSignalSent
        );
        let routes = crate::state::read_json_file(
            &state
                .workspace
                .join(".agentcall")
                .join("state")
                .join("routes.json"),
            json!({}),
        );
        let route = routes.get("route-pty").unwrap();
        assert_eq!(route["status"], "prompt_commit_signal_sent");
        assert_eq!(
            route["result"]["prompt_gate"]["commit_attempts"][0]["kind"],
            "daemon_auto"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prompt_commit_signal_sent_expires_to_unacknowledged() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-commit-unack-v61-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        install_prompt_gate_route(
            &state,
            "route-pty",
            "worker-a",
            json!({
                "schema_version": 2,
                "state": "commit_signal_sent",
                "task_started": false,
                "prompt_id": "route_prompt:route-pty:worker-a",
                "prompt_written_at_ms": now_ms().saturating_sub(30_000),
                "active_commit_attempt_id": "prompt-commit-route-pty-worker-a-1",
                "active_commit_sent_at_ms": now_ms().saturating_sub(DEFAULT_COMMIT_ACK_DEADLINE_MS + 1000),
                "awaiting_hook": "UserPromptSubmit",
                "ack_deadline_ms": 15_000,
                "commit_ack_deadline_ms": DEFAULT_COMMIT_ACK_DEADLINE_MS,
                "commit_attempts": [{
                    "attempt_id": "prompt-commit-route-pty-worker-a-1",
                    "sent_at_ms": now_ms().saturating_sub(DEFAULT_COMMIT_ACK_DEADLINE_MS + 1000)
                }]
            }),
        );

        let tui = mcp_session(&state, &json!({"name": "worker-a", "view": "tui"}), None).unwrap();
        assert_eq!(
            tui["status"]["route_status"],
            "prompt_commit_unacknowledged"
        );
        assert_eq!(
            tui["status"]["prompt"]["state"],
            "prompt_commit_unacknowledged"
        );
        assert_eq!(tui["status"]["prompt"]["can_submit_pending_prompt"], true);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn destructive_session_send_without_control_token_returns_structured_refusal() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-missing-control-token-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());

        let result =
            mcp_session_send(&state, &json!({"name": "worker-a", "action": "stop"}), None).unwrap();

        assert_eq!(result["ok"], false);
        assert_eq!(result["status"], "missing_control_token");
        assert_eq!(
            result["next_step"],
            "call agentcall_session(view=summary), read control.token, then retry quickly"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn session_events_are_compact_unless_raw_view_requested() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-session-events-compact-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        append_agent_event(
            &state,
            "hook.UserPromptSubmit",
            "prompt",
            json!({
                "wrapper_session": "worker-a",
                "prompt": "please do a long and private task".repeat(20)
            }),
        );
        append_agent_event(
            &state,
            "hook.PreToolUse",
            "write denied",
            json!({
                "wrapper_session": "worker-a",
                "tool_name": "Write",
                "tool_input": {
                    "file_path": "src/lib.rs",
                    "content": "secret source content".repeat(20)
                },
                "decision": {"allowed": false, "reason": "denied"}
            }),
        );

        let events =
            mcp_session(&state, &json!({"name": "worker-a", "view": "events"}), None).unwrap();
        assert_eq!(events["view"], "events");
        assert_eq!(events["events"][1]["kind"], "denied_write");
        assert_eq!(events["events"][1]["actionability"], "requires_supervisor");
        assert!(events["events"][0].get("data").is_none());
        assert!(events["events"][1].get("data").is_none());
        let rendered = serde_json::to_string(&events).unwrap();
        assert!(!rendered.contains("secret source content"));
        assert!(!rendered.contains("private task"));

        let raw = mcp_session(&state, &json!({"name": "worker-a", "view": "raw"}), None).unwrap();
        assert_eq!(raw["view"], "raw");
        assert!(raw["events"][0].get("data").is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn compact_events_view_enforces_hard_budget() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-session-events-budget-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        for idx in 0..240 {
            append_agent_event(
                &state,
                "hook.PostToolUse",
                "tool output ".repeat(80).as_str(),
                json!({
                    "wrapper_session": "worker-a",
                    "tool_name": "Read",
                    "tool_response": {
                        "stdout": format!("line {idx}\n").repeat(80)
                    }
                }),
            );
        }

        let events = mcp_session(
            &state,
            &json!({"name": "worker-a", "view": "events", "limit": 200}),
            None,
        )
        .unwrap();
        let rendered = serde_json::to_string(&events).unwrap();
        assert!(
            rendered.len() <= EVENTS_VIEW_MAX_BYTES,
            "events view exceeded hard cap: {}",
            rendered.len()
        );
        assert_eq!(events["budget"]["hard_cap_enforced"], true);
        assert_eq!(events["budget"]["truncated"], true);
        assert!(
            events["budget"]["omitted"]["budget_trimmed_items"]
                .as_u64()
                .unwrap()
                > 0
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn tui_view_ignores_legacy_raw_includes() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-session-tui-contract-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        append_agent_event(
            &state,
            "hook.PreToolUse",
            "write denied",
            json!({
                "wrapper_session": "worker-a",
                "tool_name": "Write",
                "tool_input": {
                    "file_path": "src/lib.rs",
                    "content": "secret source content".repeat(20)
                },
                "decision": {"allowed": false, "reason": "denied"}
            }),
        );

        let tui = mcp_session(
            &state,
            &json!({"name": "worker-a", "view": "tui", "include": ["clean_tail", "events"]}),
            None,
        )
        .unwrap();
        assert_eq!(tui["view"], "tui");
        assert!(tui.get("clean_tail").is_none());
        assert!(tui.get("events").is_none());
        assert!(tui.get("data").is_none());
        let rendered = serde_json::to_string(&tui).unwrap();
        assert!(!rendered.contains("secret source content"));
        assert_eq!(tui["current_blocker"]["kind"], "blocked_by_policy");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn tui_view_enforces_hard_budget_when_route_paths_are_noisy() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-session-tui-budget-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        let state_dir = root.join(".agentcall").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let writable_roots: Vec<Value> = (0..800)
            .map(|idx| {
                json!({
                    "kind": "write_path",
                    "display": format!("very/long/generated/path/{idx}/{}", "x".repeat(80)),
                    "abs": format!("E:/Project/AgentCall/very/long/generated/path/{idx}/{}", "x".repeat(80))
                })
            })
            .collect();
        crate::state::write_json_file(
            &state_dir.join("routes.json"),
            &json!({
                "route-pty": {
                    "route_id": "route-pty",
                    "recommended_runtime": "pty",
                    "session_name": "worker-a",
                    "status": "prompt_submitted",
                    "result": {
                        "phase": "execute",
                        "containment": {
                            "write_paths_input": ["src"],
                            "writable_paths": ["src"],
                            "writable_roots": writable_roots,
                            "roots": {
                                "process_cwd": "D:/guKimi",
                                "target_workspace": "E:/Project/AgentCall",
                                "scratch_root": "D:/guKimi/.agentcall/workspaces/worker-a"
                            }
                        }
                    }
                }
            }),
        )
        .unwrap();

        let tui = mcp_session(&state, &json!({"name": "worker-a", "view": "tui"}), None).unwrap();
        let rendered = serde_json::to_string(&tui).unwrap();
        assert!(
            rendered.len() <= TUI_VIEW_MAX_BYTES,
            "tui view exceeded hard cap: {}",
            rendered.len()
        );
        assert_eq!(tui["budget"]["hard_cap_enforced"], true);
        assert_eq!(tui["budget"]["truncated"], true);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn tui_view_projects_prompt_missing_after_deadline() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-prompt-ack-missing-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        let state_dir = root.join(".agentcall").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        crate::state::write_json_file(
            &state_dir.join("routes.json"),
            &json!({
                "route-pty": {
                    "route_id": "route-pty",
                    "recommended_runtime": "pty",
                    "session_name": "worker-a",
                    "status": "prompt_pending_ack",
                    "created_at": now_ms().saturating_sub(20_000),
                    "result": {
                        "phase": "execute",
                        "prompt_gate": {
                            "schema_version": 2,
                            "state": "prompt_pending_ack",
                            "task_started": false,
                            "prompt_id": "route_prompt:route-pty:worker-a",
                            "prompt_written_at_ms": now_ms().saturating_sub(20_000),
                            "acknowledged": false,
                            "awaiting_hook": "UserPromptSubmit",
                            "ack_deadline_ms": 15_000,
                            "commit_ack_deadline_ms": crate::prompt_gate::DEFAULT_COMMIT_ACK_DEADLINE_MS,
                            "commit_attempts": []
                        }
                    }
                }
            }),
        )
        .unwrap();

        let tui = mcp_session(&state, &json!({"name": "worker-a", "view": "tui"}), None).unwrap();
        assert_eq!(tui["status"]["route_status"], "prompt_missing");
        assert_eq!(tui["status"]["prompt"]["state"], "prompt_missing");
        assert_eq!(tui["status"]["prompt"]["can_submit_pending_prompt"], true);
        assert_eq!(tui["status"]["prompt"]["awaiting_hook"], "UserPromptSubmit");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn mcp_tool_called_redacts_long_or_sensitive_arguments() {
        let redacted = redact_mcp_arguments(&json!({
            "objective": "x".repeat(400),
            "workspace": "E:/Project/AgentCall",
            "nested": {"content": "secret".repeat(100)}
        }));
        assert_eq!(redacted["workspace"], "E:/Project/AgentCall");
        assert_eq!(redacted["objective"]["redacted"], true);
        assert_eq!(redacted["nested"]["content"]["redacted"], true);
        assert!(redacted["objective"]["preview"].as_str().unwrap().len() <= STRING_PREVIEW_BYTES);
    }

    #[test]
    fn report_accept_attaches_deterministic_confidence() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-report-confidence-{}",
            std::process::id()
        ));
        let state = Arc::new(AppState::test(root.clone()));
        let reports_dir = root.join(".agentcall/tasks/task-a/reports");
        std::fs::create_dir_all(&reports_dir).unwrap();
        std::fs::write(
            reports_dir.join("report.json"),
            serde_json::to_string(&json!({
                "task_id": "task-a",
                "session_id": "worker-a",
                "status": "completed",
                "report_path": "docs/report.md"
            }))
            .unwrap(),
        )
        .unwrap();
        append_agent_event(
            &state,
            "hook.PostToolUse",
            "write observed",
            json!({
                "wrapper_session": "worker-a",
                "tool_name": "Write",
                "decision": {"reason": "write observed", "files": ["src/app.rs"]}
            }),
        );

        let reports = mcp_report(&state, &json!({"action": "accept"}), None).unwrap();
        assert_eq!(reports[0]["confidence"]["band"], "high");
        assert!(
            reports[0]["confidence"]["evidence"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["kind"] == "file_written")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn report_accept_resolves_relative_report_path_against_route_workspace() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-report-route-workspace-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        let target_workspace = root.join("target-workspace");
        let report_rel = ".agentcall/reports/worker-report.md";
        let report_abs = target_workspace.join(report_rel);
        std::fs::create_dir_all(report_abs.parent().unwrap()).unwrap();
        std::fs::write(&report_abs, "# Report\n\nstatus: completed\n").unwrap();

        let state_dir = root.join(".agentcall").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        crate::state::write_json_file(
            &state_dir.join("routes.json"),
            &json!({
                "route-a": {
                    "route_id": "route-a",
                    "recommended_runtime": "pty",
                    "runtime": "auto",
                    "session_name": "worker-a",
                    "workspace": target_workspace.display().to_string(),
                    "status": "started",
                    "result": {
                        "context_packet": {
                            "report_path": report_rel,
                            "workspace": target_workspace.display().to_string()
                        }
                    }
                }
            }),
        )
        .unwrap();

        let accepted = accept_report_for_session(&state, "worker-a");
        assert_eq!(accepted["ok"], true);
        assert_eq!(accepted["status"], "accepted");
        assert_eq!(accepted["validation"]["exists"], true);
        assert_eq!(accepted["validation"]["non_empty"], true);
        assert_eq!(accepted["validation"]["daemon_observed_write"], false);
        assert_eq!(accepted["confidence"]["overall"], "medium");
        assert_eq!(accepted["confidence"]["artifact"], "high");
        assert_eq!(accepted["confidence"]["daemon_write"], "low");
        assert_eq!(
            accepted["report_abs_path"],
            report_abs.display().to_string()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn request_report_patches_route_with_deadline_and_report_block() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-request-report-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        let target_workspace = root.join("target-workspace");
        let report_rel = ".agents/agentcall/route-1-worker-a.md";
        let state_dir = root.join(".agentcall").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        crate::state::write_json_file(
            &state_dir.join("routes.json"),
            &json!({
                "route-1": {
                    "route_id": "route-1",
                    "recommended_runtime": "pty",
                    "runtime": "pty",
                    "session_name": "worker-a",
                    "workspace": target_workspace.display().to_string(),
                    "status": "working",
                    "result": {
                        "report": {
                            "status": "report_not_requested",
                            "ready": false,
                            "path": report_rel,
                            "rel_path": report_rel,
                            "abs_path": target_workspace.join(report_rel).display().to_string(),
                            "target_workspace": target_workspace.display().to_string(),
                            "source": "daemon_minted"
                        }
                    }
                }
            }),
        )
        .unwrap();

        let report = mark_report_requested(&state, "worker-a").unwrap();
        assert_eq!(report["status"], "report_requested");
        assert_eq!(report["ready"], false);
        assert_eq!(report["path"], report_rel);
        assert_eq!(report["source"], "daemon_minted");
        assert!(
            report["request_id"]
                .as_str()
                .unwrap()
                .starts_with("report-request-route-1-")
        );
        assert!(
            report["deadline_at_ms"].as_u64().unwrap()
                > report["requested_at_ms"].as_u64().unwrap()
        );

        let routes = crate::state::read_json_file(&state_dir.join("routes.json"), json!({}));
        assert_eq!(routes["route-1"]["status"], "report_requested");
        assert_eq!(
            routes["route-1"]["required_next_step"],
            "wait_for_report_or_inspect_session"
        );
        assert_eq!(
            routes["route-1"]["result"]["report_request"]["status"],
            "report_requested"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn report_accept_high_requires_daemon_observed_write() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-report-route-write-confidence-{}",
            std::process::id()
        ));
        let state = AppState::test(root.clone());
        let report_rel = ".agentcall/reports/worker-report.md";
        let report_abs = root.join(report_rel);
        std::fs::create_dir_all(report_abs.parent().unwrap()).unwrap();
        std::fs::write(&report_abs, "# Report\n\nstatus: completed\n").unwrap();

        let state_dir = root.join(".agentcall").join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        crate::state::write_json_file(
            &state_dir.join("routes.json"),
            &json!({
                "route-a": {
                    "route_id": "route-a",
                    "recommended_runtime": "pty",
                    "runtime": "auto",
                    "session_name": "worker-a",
                    "workspace": root.display().to_string(),
                    "status": "report_ready",
                    "result": {
                        "report_ready": true,
                        "context_packet": {
                            "report_path": report_rel,
                            "workspace": root.display().to_string()
                        }
                    }
                }
            }),
        )
        .unwrap();

        let accepted = accept_report_for_session(&state, "worker-a");
        assert_eq!(accepted["ok"], true);
        assert_eq!(accepted["validation"]["daemon_observed_write"], true);
        assert_eq!(accepted["confidence"]["overall"], "high");
        assert_eq!(accepted["confidence"]["daemon_write"], "high");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn menu_choice_accepts_single_digit_only() {
        assert_eq!(menu_choice(&json!({"text": "1"})).unwrap(), "1");
        assert_eq!(menu_choice(&json!({"text": " 3 "})).unwrap(), "3");
        assert!(menu_choice(&json!({"text": "10"})).is_err());
        assert!(menu_choice(&json!({"text": "yes"})).is_err());
        assert!(menu_choice(&json!({})).is_err());
    }

    #[test]
    fn menu_prompt_detector_recognizes_dynamic_workflow_menu() {
        assert!(looks_like_menu_prompt(
            "Run a dynamic workflow?\n > 1. Yes, run it\n2. View raw script\n3. No"
        ));
        assert!(looks_like_menu_prompt("Esc to cancel · Tab to amend"));
        assert!(!looks_like_menu_prompt(
            "Claude is working normally and has no visible menu."
        ));
    }
}
