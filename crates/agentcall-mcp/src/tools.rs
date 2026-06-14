use crate::bootstrap::daemon_control;
use crate::config::Config;
use crate::daemon_client::daemon_post_json;
use serde_json::{Value, json};

pub(crate) fn list_tools(_config: &Config) -> Vec<Value> {
    canonical_tools()
}

pub(crate) fn call_tool(config: &Config, name: &str, args: Value) -> Result<Value, String> {
    if name == "agentcall_daemon" {
        return daemon_control(config, args);
    }
    daemon_post_json(
        config,
        "/api/mcp/call",
        json!({
            "name": name,
            "arguments": args,
            "client": {
                "owner_id": config.owner_id,
            }
        }),
    )
}

fn daemon_tool() -> Value {
    json!({
        "name": "agentcall_daemon",
        "description": "Bootstrap or inspect the AgentCall daemon. Use action=start before board/route when the daemon is not running.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["status", "start"], "default": "status"},
                "wait_seconds": {"type": "integer", "minimum": 0, "maximum": 30, "default": 10},
                "debug": {"type": "boolean", "default": false, "description": "Return full daemon health including global worker counts. Default status is owner/session-safe."}
            },
            "additionalProperties": false
        }
    })
}

fn board_tool() -> Value {
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
    })
}

fn route_tool() -> Value {
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
    })
}

fn session_tool() -> Value {
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
    })
}

fn session_send_tool() -> Value {
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
    })
}

fn report_tool() -> Value {
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
    })
}

fn canonical_tools() -> Vec<Value> {
    vec![
        daemon_tool(),
        board_tool(),
        route_tool(),
        session_tool(),
        session_send_tool(),
        report_tool(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_tools_are_static_even_without_daemon() {
        let names: Vec<String> = canonical_tools()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            vec![
                "agentcall_daemon".to_string(),
                "agentcall_board".to_string(),
                "agentcall_route".to_string(),
                "agentcall_session".to_string(),
                "agentcall_session_send".to_string(),
                "agentcall_report".to_string()
            ]
        );
    }

    #[test]
    fn board_tool_defaults_to_compact_attention() {
        let tool = board_tool();
        assert_eq!(
            tool["inputSchema"]["properties"]["view"]["default"],
            "compact"
        );
        assert_eq!(
            tool["inputSchema"]["properties"]["filter"]["default"],
            "attention"
        );
        assert_eq!(
            tool["inputSchema"]["properties"]["scope"]["default"],
            "mine"
        );
    }

    #[test]
    fn daemon_tool_defaults_to_owner_safe_status() {
        let tool = daemon_tool();
        assert_eq!(tool["inputSchema"]["properties"]["debug"]["default"], false);
    }

    #[test]
    fn route_tool_hides_debug_runtime_knobs() {
        let tool = route_tool();
        let properties = &tool["inputSchema"]["properties"];
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
    fn session_tool_schema_allows_explicit_debug_includes() {
        let tool = session_tool();
        let include_enum = tool["inputSchema"]["properties"]["include"]["items"]["enum"]
            .as_array()
            .unwrap();
        assert!(include_enum.iter().any(|item| item == "clean_tail"));
        assert!(include_enum.iter().any(|item| item == "control"));
        assert!(include_enum.iter().any(|item| item == "plan"));
        assert!(include_enum.iter().any(|item| item == "debug"));
        assert!(include_enum.iter().any(|item| item == "policy"));
        let properties = &tool["inputSchema"]["properties"];
        assert_eq!(properties["view"]["default"], "summary");
        assert!(
            properties["view"]["enum"]
                .as_array()
                .unwrap()
                .contains(&json!("tui"))
        );
        assert!(
            properties["view"]["enum"]
                .as_array()
                .unwrap()
                .contains(&json!("events"))
        );
        assert!(properties.get("cursor").is_some());
        assert!(properties.get("event_types").is_some());
    }

    #[test]
    fn session_send_tool_hides_daemon_owned_safety_fields() {
        let tool = session_send_tool();
        let properties = &tool["inputSchema"]["properties"];
        assert!(properties.get("idempotency_key").is_none());
        assert!(properties.get("precondition").is_none());
        assert!(properties.get("owner_lease_id").is_none());
        assert!(properties.get("lease_generation").is_none());
        assert!(properties.get("choice").is_some());
        assert!(properties.get("control_token").is_some());
        let actions = properties["action"]["enum"].as_array().unwrap();
        assert!(actions.contains(&json!("kill")));
        assert!(actions.contains(&json!("submit_pending_prompt")));
    }
}
