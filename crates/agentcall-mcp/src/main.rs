use serde_json::{json, Value};
use std::env;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "agentcall-mcp";
const SERVER_VERSION: &str = "0.3.0";

fn main() {
    let config = match Config::from_args(env::args().skip(1).collect()) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(2);
        }
    };
    if let Err(err) = serve(config) {
        eprintln!("agentcall-mcp: {err}");
        std::process::exit(1);
    }
}

#[derive(Clone)]
struct Config {
    workspace: PathBuf,
    python: String,
}

impl Config {
    fn from_args(args: Vec<String>) -> Result<Self, String> {
        let mut workspace = env::current_dir().map_err(|err| err.to_string())?;
        let mut python = "python".to_string();
        let mut index = 0;
        while index < args.len() {
            match args[index].as_str() {
                "--workspace" => {
                    index += 1;
                    workspace = PathBuf::from(
                        args.get(index)
                            .ok_or("missing --workspace value")?
                            .to_string(),
                    );
                }
                "--python" => {
                    index += 1;
                    python = args
                        .get(index)
                        .ok_or("missing --python value")?
                        .to_string();
                }
                "--help" | "-h" => {
                    return Err(
                        "usage: agentcall-mcp [--workspace PATH] [--python PYTHON]".to_string(),
                    );
                }
                other => return Err(format!("unknown argument: {other}")),
            }
            index += 1;
        }
        Ok(Self { workspace, python })
    }
}

fn serve(config: Config) -> io::Result<()> {
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
            Err(err) => Some(error_response(json!(null), -32700, &format!("Parse error: {err}"))),
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
        "tools/list" => Some(success_response(id, json!({ "tools": tools() }))),
        "tools/call" => Some(handle_tool_call(config, id, request.get("params").cloned().unwrap_or(json!({})))),
        _ => Some(error_response(id, -32601, &format!("Method not found: {method}"))),
    }
}

fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "agentcall_capabilities",
            "description": "Discover AgentCall v3 MCP capabilities, drivers, endpoints, and workspace.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        }),
        json!({
            "name": "agentcall_report_schema",
            "description": "Return the v2 ChildReport JSON schema used by AgentCall workflows.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        }),
        json!({
            "name": "agentcall_workflow_simulate",
            "description": "Run the small-project bounded parent/child workflow simulation.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string", "description": "Workspace root. Defaults to the server workspace."},
                    "driver": {"type": "string", "enum": ["scripted", "headless-json", "acp"], "default": "scripted"},
                    "max_turns": {"type": "integer", "minimum": 1, "default": 1}
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_workflow_inspect",
            "description": "Inspect a v2 workflow task and return report/state evidence.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string", "description": "Workspace root. Defaults to the server workspace."},
                    "task_id": {"type": "string"}
                },
                "required": ["task_id"],
                "additionalProperties": false
            }
        }),
    ]
}

fn handle_tool_call(config: &Config, id: Value, params: Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    let result = match name {
        "agentcall_capabilities" => Ok(capabilities(config)),
        "agentcall_report_schema" => python_json(config, &config.workspace, &["-c", REPORT_SCHEMA_SNIPPET]),
        "agentcall_workflow_simulate" => workflow_simulate(config, args),
        "agentcall_workflow_inspect" => workflow_inspect(config, args),
        _ => Err(format!("unknown tool: {name}")),
    };
    match result {
        Ok(value) => success_response(id, tool_text(value)),
        Err(err) => success_response(id, tool_error(&err)),
    }
}

fn capabilities(config: &Config) -> Value {
    json!({
        "service": "agentcall",
        "server": SERVER_NAME,
        "version": SERVER_VERSION,
        "protocol_version": "v3-mcp",
        "workspace": config.workspace.to_string_lossy(),
        "features": {
            "workflow_simulate": true,
            "workflow_inspect": true,
            "report_schema": true,
            "driver_discovery": true
        },
        "drivers": [
            {"kind": "scripted", "available": true, "live_model": false, "costs_tokens": false},
            {"kind": "headless-json", "available": true, "live_model": true, "costs_tokens": true},
            {"kind": "acp", "available": true, "live_model": true, "costs_tokens": true}
        ],
        "tools": [
            "agentcall_capabilities",
            "agentcall_report_schema",
            "agentcall_workflow_simulate",
            "agentcall_workflow_inspect"
        ]
    })
}

fn workflow_simulate(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let driver = args.get("driver").and_then(Value::as_str).unwrap_or("scripted");
    let max_turns = args.get("max_turns").and_then(Value::as_i64).unwrap_or(1).to_string();
    let output = run_agentcall(
        config,
        &root,
        &[
            "workflow",
            "simulate",
            "--driver",
            driver,
            "--max-turns",
            &max_turns,
        ],
    )?;
    Ok(json!({"root": root.to_string_lossy(), "output": output, "summary": parse_key_value_output(&output)}))
}

fn workflow_inspect(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let task_id = args
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or("missing required argument: task_id")?;
    let output = run_agentcall(config, &root, &["workflow", "inspect", task_id])?;
    Ok(json!({"root": root.to_string_lossy(), "task_id": task_id, "output": output, "summary": parse_key_value_output(&output)}))
}

fn root_from_args(config: &Config, args: &Value) -> PathBuf {
    args.get("root")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| config.workspace.clone())
}

fn run_agentcall(config: &Config, root: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new(&config.python);
    command
        .arg("-m")
        .arg("agentcall")
        .arg("--root")
        .arg(root)
        .args(args)
        .current_dir(&config.workspace);
    let python_path = config.workspace.join("src");
    command.env("PYTHONPATH", python_path);
    let output = command
        .output()
        .map_err(|err| format!("failed to run python agentcall: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "agentcall exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn python_json(config: &Config, root: &Path, args: &[&str]) -> Result<Value, String> {
    let mut command = Command::new(&config.python);
    command.args(args).current_dir(root);
    command.env("PYTHONPATH", config.workspace.join("src"));
    let output = command
        .output()
        .map_err(|err| format!("failed to run python: {err}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    serde_json::from_slice(&output.stdout).map_err(|err| format!("invalid JSON from python: {err}"))
}

const REPORT_SCHEMA_SNIPPET: &str =
    "import json; from agentcall.v2 import REPORT_JSON_SCHEMA; print(json.dumps(REPORT_JSON_SCHEMA))";

fn parse_key_value_output(output: &str) -> Value {
    let mut object = serde_json::Map::new();
    for line in output.lines() {
        if let Some((key, value)) = line.split_once(':') {
            object.insert(key.trim().to_string(), json!(value.trim()));
        }
    }
    Value::Object(object)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_agentcall_tools() {
        let names: Vec<String> = tools()
            .into_iter()
            .map(|tool| tool["name"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"agentcall_capabilities".to_string()));
        assert!(names.contains(&"agentcall_workflow_simulate".to_string()));
        assert!(names.contains(&"agentcall_workflow_inspect".to_string()));
    }

    #[test]
    fn parses_key_value_output() {
        let parsed = parse_key_value_output("task_id: task-0001\nstatus: accepted\nreports: 2");
        assert_eq!(parsed["task_id"], "task-0001");
        assert_eq!(parsed["status"], "accepted");
        assert_eq!(parsed["reports"], "2");
    }
}
