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
        json!({"name": name, "arguments": args}),
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
                "wait_seconds": {"type": "integer", "minimum": 0, "maximum": 30, "default": 10}
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
                "scope": {"type": "string", "enum": ["all", "mine"], "default": "all"},
                "owner_id": {"type": "string"}
            },
            "additionalProperties": false
        }
    })
}

fn route_tool() -> Value {
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
    })
}

fn session_tool() -> Value {
    json!({
        "name": "agentcall_session",
        "description": "Return one daemon PTY session view. Default view=summary is compact and projection-first; use view=tui for dashboard data, view=events for compact events, and view=debug/raw only for explicit inspection.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "root": {"type": "string"},
                "name": {"type": "string"},
                "view": {"type": "string", "enum": ["summary", "tui", "events", "debug", "raw"], "default": "summary"},
                "detail": {"type": "string", "enum": ["compact", "debug", "raw"], "default": "compact"},
                "include": {"type": "array", "items": {"type": "string", "enum": ["summary", "clean_tail", "plan", "events", "artifacts", "policy", "metrics", "debug"]}, "default": ["summary"]},
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
        "description": "Send text or a high-level nudge action to a daemon PTY session. Avoid repeated continue nudges while the session is still inside its patience window unless attention_status requires intervention.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "root": {"type": "string"},
                "name": {"type": "string"},
                "action": {"type": "string", "enum": ["send", "continue", "stop", "kill", "request_report", "revise_plan", "approve_plan", "start_auto", "select_option", "interrupt"], "default": "send"},
                "text": {"type": "string"},
                "enter": {"type": "boolean", "default": true},
                "idempotency_key": {"type": "string"},
                "precondition": {"type": "object"},
                "owner_id": {"type": "string"},
                "owner_lease_id": {"type": "string"},
                "lease_generation": {"type": "integer", "minimum": 0}
            },
            "required": ["name"],
            "additionalProperties": false
        }
    })
}

fn report_tool() -> Value {
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
    }

    #[test]
    fn session_tool_schema_allows_explicit_debug_includes() {
        let tool = session_tool();
        let include_enum = tool["inputSchema"]["properties"]["include"]["items"]["enum"]
            .as_array()
            .unwrap();
        assert!(include_enum.iter().any(|item| item == "clean_tail"));
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
    fn session_send_tool_exposes_safety_fields() {
        let tool = session_send_tool();
        let properties = &tool["inputSchema"]["properties"];
        assert!(properties.get("idempotency_key").is_some());
        assert!(properties.get("precondition").is_some());
        let actions = properties["action"]["enum"].as_array().unwrap();
        assert!(actions.contains(&json!("kill")));
    }
}
