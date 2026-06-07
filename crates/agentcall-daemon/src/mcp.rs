use crate::hooks::queue_supervisor_instruction;
use crate::routes::{
    RouteRequest, checkpoint_session, handle_route, patch_route_record, route_for_wrapper_session,
};
use crate::session::{InputRequest, get_session, interrupt_session, write_input};
use crate::state::{AppState, append_agent_event, read_events};
use crate::summary::{board_state, clean_session_output, session_plan_artifact, session_summary};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Deserialize)]
pub(crate) struct McpCallRequest {
    name: String,
    arguments: Option<Value>,
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
                    "view": {"type": "string", "enum": ["full", "compact"], "default": "full"},
                    "filter": {"type": "string", "enum": ["all", "attention"], "default": "all"},
                    "section": {"type": "string", "enum": ["all", "sessions", "events", "reports", "claims", "transcripts", "routes"], "default": "all"}
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_route",
            "description": "Recommend or start a Claude Code PTY utility worker. PTY workers are asynchronous background workers, not synchronous function calls; after start, wait for prompt_gate/hooks/session summary before retrying. Use pty_workflow=plan_then_auto only when the supervisor explicitly wants a plan gate.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "objective": {"type": "string"},
                    "workspace": {"type": "string"},
                    "mode": {"type": "string", "enum": ["recommend", "start"], "default": "recommend"},
                    "runtime": {"type": "string", "enum": ["auto", "pty"], "default": "auto"},
                    "estimated_minutes": {"type": "integer", "minimum": 0},
                    "estimated_files": {"type": "integer", "minimum": 0},
                    "estimated_loc": {"type": "integer", "minimum": 0},
                    "needs_continuity": {"type": "boolean", "default": false},
                    "risk": {"type": "string", "enum": ["low", "medium", "high"], "default": "medium"},
                    "session_name": {"type": "string"},
                    "command": {"type": "array", "items": {"type": "string"}},
                    "task_id": {"type": "string"},
                    "call_id": {"type": "string"},
                    "phase": {"type": "string", "default": "execute"},
                    "role": {"type": "string", "default": "executor"},
                    "allowed_paths": {"type": "array", "items": {"type": "string"}},
                    "acceptance_criteria": {"type": "array", "items": {"type": "string"}},
                    "report_path": {"type": "string"},
                    "read_only": {"type": "boolean", "default": false},
                    "pty_workflow": {"type": "string", "enum": ["normal", "plan_then_auto"], "default": "normal"},
                    "initial_permission_mode": {"type": "string", "enum": ["plan", "auto", "default"]},
                    "persist_context": {"type": "boolean", "default": true}
                },
                "required": ["objective"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_session",
            "description": "Return one daemon PTY session llm_summary, with optional clean output tail. Prefer summary patience_hint, last_progress_age_seconds, and attention_status over impatient raw-terminal polling.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "name": {"type": "string"},
                    "include": {"type": "array", "items": {"type": "string", "enum": ["summary", "clean_tail", "plan"]}, "default": ["summary"]}
                },
                "required": ["name"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_session_send",
            "description": "Send text or a high-level nudge action to a daemon PTY session. Avoid repeated continue nudges while the session is still inside its patience window unless attention_status requires intervention.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "name": {"type": "string"},
                    "action": {"type": "string", "enum": ["send", "continue", "stop", "request_report", "revise_plan", "approve_plan", "start_auto", "select_option", "interrupt"], "default": "send"},
                    "text": {"type": "string"},
                    "enter": {"type": "boolean", "default": true}
                },
                "required": ["name"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_report",
            "description": "Request or accept a report for a supervised session/task.",
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
        "agentcall_board" => mcp_board(state, &args),
        "agentcall_route" => mcp_route(state, args.clone()),
        "agentcall_session" => mcp_session(state, &args),
        "agentcall_session_send" => mcp_session_send(state, &args),
        "agentcall_report" => mcp_report(state, &args),
        other => Err(format!("unknown daemon MCP tool: {other}")),
    };
    let status = if result.is_ok() { "ok" } else { "error" };
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
            "arguments": args,
            "runtime": "daemon_mcp_bridge",
            "error": message,
        }),
    );
    result
}

fn mcp_board(state: &AppState, args: &Value) -> Result<Value, String> {
    Ok(board_state(
        state,
        args.get("view").and_then(Value::as_str),
        args.get("filter").and_then(Value::as_str),
        args.get("section").and_then(Value::as_str),
    ))
}

fn mcp_route(state: &Arc<AppState>, args: Value) -> Result<Value, String> {
    let req: RouteRequest =
        serde_json::from_value(args).map_err(|err| format!("invalid route arguments: {err}"))?;
    handle_route(state, req)
}

fn mcp_session(state: &AppState, args: &Value) -> Result<Value, String> {
    let name = required_str(args, "name")?;
    let session = get_session(state, name).ok_or_else(|| "session not found".to_string())?;
    let summary = session_summary(state, &session);
    let include = string_array(args, "include");
    let include_clean_tail = include.iter().any(|item| item == "clean_tail");
    let include_plan = include.iter().any(|item| item == "plan");
    if include_clean_tail || include_plan {
        let mut response = json!({
            "summary": summary,
        });
        if include_clean_tail {
            response["clean_tail"] = json!({
                    "session": name,
                    "clean_output": clean_session_output(&session),
                    "decode_health": session.decode_health.lock().unwrap().clone()
            });
        }
        if include_plan {
            response["plan"] = session_plan_artifact(state, &session, true);
        }
        Ok(response)
    } else {
        Ok(summary)
    }
}

fn mcp_session_send(state: &AppState, args: &Value) -> Result<Value, String> {
    let name = required_str(args, "name")?;
    let action = args.get("action").and_then(Value::as_str).unwrap_or("send");
    if action == "stop" {
        return crate::session::stop_session(state, name).map(|_| json!({"ok": true}));
    }
    if action == "interrupt" {
        let text = args.get("text").and_then(Value::as_str).map(str::to_string);
        interrupt_session(state, name, text)?;
        return Ok(json!({
            "ok": true,
            "action": "interrupt",
            "status": "interrupt_sent",
            "warning": "Use interrupt only when the worker is drifting, doing the wrong thing, or must be reclaimed immediately."
        }));
    }
    if action == "approve_plan" || action == "start_auto" {
        if !is_plan_then_auto_session(state, name) {
            return Err("session is not a plan_then_auto PTY route".to_string());
        }
        write_input(
            state,
            name,
            InputRequest {
                text: "1".to_string(),
                enter: Some(true),
            },
        )?;
        update_pty_workflow_route(
            state,
            name,
            "auto_running",
            "auto",
            "approved via session_send action",
        )?;
        return Ok(json!({"ok": true, "action": action, "workflow_status": "auto_running"}));
    }
    if action == "select_option" {
        let choice = menu_choice(args)?;
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
        write_input(
            state,
            name,
            InputRequest {
                text: choice.clone(),
                enter: Some(true),
            },
        )?;
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
    let summary = session_summary(state, &session);
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
            "hint": "The worker is repeating a denied action. Do not wait or resend the same prompt; update allowed_paths/task, request a blocker report after interrupt, or stop the worker."
        }));
    }
    if liveness_status == "working" && attention_status == "none" {
        let queued = queue_supervisor_instruction(state, name, action, &text)?;
        let post_tool_batch_seen = session_has_seen_hook_event(state, name, "PostToolBatch");
        let warning = if post_tool_batch_seen {
            Value::Null
        } else {
            json!(
                "This session has not emitted PostToolBatch in recent events. Queued instructions may remain pending until the worker is restarted with updated D:\\guKimi hooks."
            )
        };
        return Ok(json!({
            "ok": true,
            "status": "queued_until_next_hook_injection",
            "delivery": "PostToolBatch_or_next_context_hook",
            "instruction": queued,
            "post_tool_batch_seen": post_tool_batch_seen,
            "warning": warning,
            "hint": "Claude Code does not reliably accept new prompts mid-turn. AgentCall queued this instruction for hook additionalContext instead of blindly typing into the PTY."
        }));
    }
    write_input(
        state,
        name,
        InputRequest {
            text,
            enter: args.get("enter").and_then(Value::as_bool),
        },
    )?;
    if action == "revise_plan" {
        let _ = update_pty_workflow_route(
            state,
            name,
            "plan_revision_requested",
            "plan",
            "revision requested via session_send action",
        );
    }
    Ok(json!({"ok": true}))
}

fn menu_choice(args: &Value) -> Result<String, String> {
    let choice = args
        .get("text")
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
    let expected_type = format!("hook.{hook_event}");
    read_events(&state.workspace.join(".agentcall").join("events.ndjson"))
        .iter()
        .rev()
        .any(|event| {
            event.get("type").and_then(Value::as_str) == Some(expected_type.as_str())
                && event
                    .get("data")
                    .and_then(|data| data.get("wrapper_session"))
                    .and_then(Value::as_str)
                    == Some(wrapper_session)
        })
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

fn mcp_report(state: &Arc<AppState>, args: &Value) -> Result<Value, String> {
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
            let reports = board_state(state, None, None, Some("reports"))
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
                Ok(json!(filtered))
            } else {
                Ok(reports)
            }
        }
        other => Err(format!("unknown report action: {other}")),
    }
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
