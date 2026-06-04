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
                "view": {"type": "string", "enum": ["full", "compact"], "default": "full"},
                "filter": {"type": "string", "enum": ["all", "attention"], "default": "all"},
                "section": {"type": "string", "enum": ["all", "sessions", "events", "reports", "claims", "transcripts", "routes"], "default": "all"}
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
        "description": "Return one daemon PTY session llm_summary, with optional clean output tail. Prefer summary patience_hint, last_progress_age_seconds, and attention_status over impatient raw-terminal polling.",
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
                "action": {"type": "string", "enum": ["send", "continue", "stop", "request_report", "revise_plan", "approve_plan", "start_auto"], "default": "send"},
                "text": {"type": "string"},
                "enter": {"type": "boolean", "default": true}
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
}
