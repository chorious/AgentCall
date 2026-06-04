use crate::config::Config;
use crate::tools::{call_tool, list_tools};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "agentcall-mcp";
const SERVER_VERSION: &str = "2.3.0";

pub(crate) fn serve(config: Config) -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let line = line.trim_start_matches('\u{feff}');
        let response = match serde_json::from_str::<Value>(line) {
            Ok(request) => handle_message(&config, request),
            Err(err) => Some(error_response(
                json!(null),
                -32700,
                &format!("Parse error: {err}"),
            )),
        };
        if let Some(response) = response {
            writeln!(stdout, "{}", serde_json::to_string(&response).unwrap())?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn handle_message(config: &Config, request: Value) -> Option<Value> {
    let id = request.get("id").cloned().unwrap_or(json!(null));
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    match method {
        "initialize" => Some(success_response(
            id,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION}
            }),
        )),
        "notifications/initialized" => None,
        "ping" => Some(success_response(id, json!({}))),
        "tools/list" => Some(success_response(id, json!({ "tools": list_tools(config) }))),
        "tools/call" => Some(handle_tool_call(
            config,
            id,
            request.get("params").cloned().unwrap_or(json!({})),
        )),
        _ => Some(error_response(
            id,
            -32601,
            &format!("Method not found: {method}"),
        )),
    }
}

fn handle_tool_call(config: &Config, id: Value, params: Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match call_tool(config, name, args) {
        Ok(value) => success_response(id, tool_text(value)),
        Err(err) => success_response(id, tool_error(&err)),
    }
}

fn tool_text(value: Value) -> Value {
    json!({
        "content": [{"type": "text", "text": serde_json::to_string_pretty(&value).unwrap()}],
        "isError": false
    })
}

fn tool_error(message: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": message}],
        "isError": true
    })
}

fn success_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}
