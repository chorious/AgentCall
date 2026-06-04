use crate::bootstrap::daemon_control;
use crate::config::Config;
use crate::daemon_client::{daemon_get, daemon_post_json};
use serde_json::{Value, json};

pub(crate) fn list_tools(config: &Config) -> Vec<Value> {
    let mut tools = vec![daemon_tool()];
    if let Ok(Value::Array(mut daemon_tools)) = daemon_get(config, "/api/mcp/tools") {
        tools.append(&mut daemon_tools);
    }
    tools
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_tool_is_the_only_local_tool() {
        assert_eq!(daemon_tool()["name"].as_str().unwrap(), "agentcall_daemon");
    }
}
