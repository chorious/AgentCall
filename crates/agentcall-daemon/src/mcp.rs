use crate::routes::{
    RouteRequest, checkpoint_session, handle_route, patch_route_record, route_for_wrapper_session,
};
use crate::session::{InputRequest, get_session, write_input};
use crate::state::{AppState, append_agent_event};
use crate::summary::{board_state, clean_session_output, session_summary};
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
            "description": "Return unified board state. Use compact/attention views for low-friction Codex control.",
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
            "description": "Recommend or start a Claude Code PTY utility worker. Use pty_workflow=plan_then_auto only when the supervisor explicitly wants a plan gate.",
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
            "description": "Return one daemon PTY session llm_summary, with optional clean output tail.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "name": {"type": "string"},
                    "include": {"type": "array", "items": {"type": "string", "enum": ["summary", "clean_tail"]}, "default": ["summary"]}
                },
                "required": ["name"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_session_send",
            "description": "Send text or a high-level nudge action to a daemon PTY session.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "name": {"type": "string"},
                    "action": {"type": "string", "enum": ["send", "continue", "stop", "request_report", "revise_plan", "approve_plan", "start_auto"], "default": "send"},
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
    if include.iter().any(|item| item == "clean_tail") {
        Ok(json!({
            "summary": summary,
            "clean_tail": {
                "session": name,
                "clean_output": clean_session_output(&session),
                "decode_health": session.decode_health.lock().unwrap().clone()
            }
        }))
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
}
