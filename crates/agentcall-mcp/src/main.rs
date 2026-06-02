use serde_json::{json, Value};
use std::env;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "agentcall-mcp";
const SERVER_VERSION: &str = "0.4.0";

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
            "description": "Run the small-project bounded parent/child workflow through Claude ACP by default.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string", "description": "Workspace root. Defaults to the server workspace."},
                    "driver": {"type": "string", "enum": ["acp", "scripted"], "default": "acp"},
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
        json!({
            "name": "agentcall_route_task",
            "description": "Recommend whether a task should use ACP agents-as-tools or a Claude Code handoff session.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "objective": {"type": "string"},
                    "task_type": {"type": "string"},
                    "estimated_files": {"type": "integer", "minimum": 0},
                    "needs_continuity": {"type": "boolean", "default": false}
                },
                "required": ["objective"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_context_packet_create",
            "description": "Create a v0.4 context packet and optionally persist call input artifacts.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "task_id": {"type": "string"},
                    "call_id": {"type": "string"},
                    "phase": {"type": "string", "default": "execute"},
                    "role": {"type": "string", "default": "executor"},
                    "runtime": {"type": "string", "default": "acp"},
                    "objective": {"type": "string"},
                    "allowed_paths": {"type": "array", "items": {"type": "string"}},
                    "acceptance_criteria": {"type": "array", "items": {"type": "string"}},
                    "persist": {"type": "boolean", "default": true}
                },
                "required": ["task_id", "call_id", "objective"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_delegate_acp",
            "description": "Run the bounded small-project workflow through the ACP runtime.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "max_turns": {"type": "integer", "minimum": 1, "default": 1}
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_events_tail",
            "description": "Return recent AgentCall events as JSON.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "task_id": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "default": 50}
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_reports_list",
            "description": "Return structured child reports as JSON.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "task_id": {"type": "string"}
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_board",
            "description": "Return the unified v0.4 task/session/report board state.",
            "inputSchema": {
                "type": "object",
                "properties": {"root": {"type": "string"}},
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_hook_ingest",
            "description": "Ingest a Claude Code hook payload into AgentCall state/events.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "event": {"type": "string"},
                    "payload": {"type": "object"}
                },
                "required": ["event"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_session_spawn",
            "description": "Spawn a named PTY-backed handoff/control session.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "name": {"type": "string"},
                    "command": {"type": "array", "items": {"type": "string"}},
                    "cols": {"type": "integer", "default": 100},
                    "rows": {"type": "integer", "default": 40}
                },
                "required": ["name", "command"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_session_list",
            "description": "List PTY sessions known to AgentCall.",
            "inputSchema": {
                "type": "object",
                "properties": {"root": {"type": "string"}},
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_session_status",
            "description": "Show one PTY session status.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "name": {"type": "string"}
                },
                "required": ["name"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_session_tail",
            "description": "Return recent output from a PTY session.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "name": {"type": "string"},
                    "lines": {"type": "integer", "default": 80},
                    "plain": {"type": "boolean", "default": true}
                },
                "required": ["name"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_session_send",
            "description": "Send text to a PTY session.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "name": {"type": "string"},
                    "text": {"type": "string"},
                    "enter": {"type": "boolean", "default": true}
                },
                "required": ["name", "text"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_checkpoint_request",
            "description": "Mark a Claude Code handoff session as needing a checkpoint report.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "session_id": {"type": "string"}
                },
                "required": ["session_id"],
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
        "agentcall_route_task" => route_task_tool(config, args),
        "agentcall_context_packet_create" => context_packet_create(config, args),
        "agentcall_delegate_acp" => delegate_acp(config, args),
        "agentcall_events_tail" => events_tail(config, args),
        "agentcall_reports_list" => reports_list(config, args),
        "agentcall_board" => board(config, args),
        "agentcall_hook_ingest" => hook_ingest(config, args),
        "agentcall_session_spawn" => session_spawn(config, args),
        "agentcall_session_list" => session_list(config, args),
        "agentcall_session_status" => session_status(config, args),
        "agentcall_session_tail" => session_tail(config, args),
        "agentcall_session_send" => session_send(config, args),
        "agentcall_checkpoint_request" => checkpoint_request(config, args),
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
            "driver_discovery": true,
            "route_task": true,
            "context_packet_create": true,
            "hook_ingest": true,
            "checkpoint_request": true,
            "board": true,
            "session_control": true
        },
        "drivers": [
            {"kind": "acp", "available": true, "live_model": true, "costs_tokens": true, "default": true, "transport": "stdio-json-rpc"},
            {"kind": "scripted", "available": true, "live_model": false, "costs_tokens": false, "test_only": true}
        ],
        "tools": [
            "agentcall_capabilities",
            "agentcall_report_schema",
            "agentcall_workflow_simulate",
            "agentcall_workflow_inspect",
            "agentcall_route_task",
            "agentcall_context_packet_create",
            "agentcall_delegate_acp",
            "agentcall_events_tail",
            "agentcall_reports_list",
            "agentcall_board",
            "agentcall_hook_ingest",
            "agentcall_session_spawn",
            "agentcall_session_list",
            "agentcall_session_status",
            "agentcall_session_tail",
            "agentcall_session_send",
            "agentcall_checkpoint_request"
        ]
    })
}

fn workflow_simulate(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let driver = args.get("driver").and_then(Value::as_str).unwrap_or("acp");
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

fn route_task_tool(config: &Config, args: Value) -> Result<Value, String> {
    let objective = args
        .get("objective")
        .and_then(Value::as_str)
        .ok_or("missing required argument: objective")?;
    let mut command = vec!["route".to_string(), objective.to_string()];
    if let Some(task_type) = args.get("task_type").and_then(Value::as_str) {
        command.push("--task-type".to_string());
        command.push(task_type.to_string());
    }
    if let Some(estimated_files) = args.get("estimated_files").and_then(Value::as_i64) {
        command.push("--estimated-files".to_string());
        command.push(estimated_files.to_string());
    }
    if args.get("needs_continuity").and_then(Value::as_bool).unwrap_or(false) {
        command.push("--needs-continuity".to_string());
    }
    let output = run_agentcall_owned(config, &config.workspace, command)?;
    serde_json::from_str(&output).map_err(|err| format!("invalid route JSON: {err}"))
}

fn context_packet_create(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let task_id = required_str(&args, "task_id")?;
    let call_id = required_str(&args, "call_id")?;
    let objective = required_str(&args, "objective")?;
    let mut command = vec![
        "context".to_string(),
        "create".to_string(),
        "--task-id".to_string(),
        task_id.to_string(),
        "--call-id".to_string(),
        call_id.to_string(),
        "--phase".to_string(),
        args.get("phase").and_then(Value::as_str).unwrap_or("execute").to_string(),
        "--role".to_string(),
        args.get("role").and_then(Value::as_str).unwrap_or("executor").to_string(),
        "--runtime".to_string(),
        args.get("runtime").and_then(Value::as_str).unwrap_or("acp").to_string(),
        "--objective".to_string(),
        objective.to_string(),
    ];
    for item in string_array(&args, "allowed_paths") {
        command.push("--allowed-path".to_string());
        command.push(item);
    }
    for item in string_array(&args, "acceptance_criteria") {
        command.push("--acceptance-criterion".to_string());
        command.push(item);
    }
    if args.get("persist").and_then(Value::as_bool).unwrap_or(true) {
        command.push("--persist".to_string());
    }
    let output = run_agentcall_owned(config, &root, command)?;
    serde_json::from_str(&output).map_err(|err| format!("invalid context packet JSON: {err}"))
}

fn delegate_acp(config: &Config, args: Value) -> Result<Value, String> {
    let mut object = args.as_object().cloned().unwrap_or_default();
    object.insert("driver".to_string(), json!("acp"));
    workflow_simulate(config, Value::Object(object))
}

fn events_tail(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let mut command = vec!["events".to_string(), "--json".to_string()];
    if let Some(limit) = args.get("limit").and_then(Value::as_i64) {
        command.push("--limit".to_string());
        command.push(limit.to_string());
    }
    if let Some(task_id) = args.get("task_id").and_then(Value::as_str) {
        command.push(task_id.to_string());
    }
    let output = run_agentcall_owned(config, &root, command)?;
    serde_json::from_str(&output).map_err(|err| format!("invalid events JSON: {err}"))
}

fn reports_list(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let mut command = vec!["reports".to_string()];
    if let Some(task_id) = args.get("task_id").and_then(Value::as_str) {
        command.push(task_id.to_string());
    }
    let output = run_agentcall_owned(config, &root, command)?;
    serde_json::from_str(&output).map_err(|err| format!("invalid reports JSON: {err}"))
}

fn board(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let output = run_agentcall_owned(config, &root, vec!["board".to_string(), "--json".to_string()])?;
    serde_json::from_str(&output).map_err(|err| format!("invalid board JSON: {err}"))
}

fn hook_ingest(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let event = required_str(&args, "event")?;
    let payload = args.get("payload").cloned().unwrap_or(json!({}));
    let output = run_agentcall_owned(
        config,
        &root,
        vec![
            "hook".to_string(),
            "ingest".to_string(),
            event.to_string(),
            "--payload-json".to_string(),
            serde_json::to_string(&payload).unwrap(),
        ],
    )?;
    serde_json::from_str(&output).map_err(|err| format!("invalid hook JSON: {err}"))
}

fn session_spawn(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let name = required_str(&args, "name")?;
    let command_args = string_array(&args, "command");
    if command_args.is_empty() {
        return Err("session command cannot be empty".to_string());
    }
    let cols = args.get("cols").and_then(Value::as_i64).unwrap_or(100);
    let rows = args.get("rows").and_then(Value::as_i64).unwrap_or(40);
    let mut command = vec![
        "session".to_string(),
        "start".to_string(),
        "--cols".to_string(),
        cols.to_string(),
        "--rows".to_string(),
        rows.to_string(),
        name.to_string(),
        "--".to_string(),
    ];
    command.extend(command_args);
    let output = run_agentcall_owned(config, &root, command)?;
    Ok(json!({"root": root.to_string_lossy(), "output": output, "summary": parse_tabbed_status(&output)}))
}

fn session_list(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let output = run_agentcall_owned(config, &root, vec!["session".to_string(), "list".to_string()])?;
    Ok(json!({"root": root.to_string_lossy(), "output": output}))
}

fn session_status(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let name = required_str(&args, "name")?;
    let output = run_agentcall_owned(
        config,
        &root,
        vec!["session".to_string(), "status".to_string(), name.to_string()],
    )?;
    Ok(json!({"root": root.to_string_lossy(), "name": name, "output": output, "summary": parse_key_value_output(&output)}))
}

fn session_tail(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let name = required_str(&args, "name")?;
    let lines = args.get("lines").and_then(Value::as_i64).unwrap_or(80);
    let mut command = vec![
        "session".to_string(),
        "tail".to_string(),
        name.to_string(),
        "--lines".to_string(),
        lines.to_string(),
    ];
    if args.get("plain").and_then(Value::as_bool).unwrap_or(true) {
        command.push("--plain".to_string());
    }
    let output = run_agentcall_owned(config, &root, command)?;
    Ok(json!({"root": root.to_string_lossy(), "name": name, "output": output}))
}

fn session_send(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let name = required_str(&args, "name")?;
    let text = required_str(&args, "text")?;
    let mut command = vec![
        "session".to_string(),
        "send".to_string(),
        name.to_string(),
        text.to_string(),
    ];
    if !args.get("enter").and_then(Value::as_bool).unwrap_or(true) {
        command.push("--no-enter".to_string());
    }
    let output = run_agentcall_owned(config, &root, command)?;
    Ok(json!({"root": root.to_string_lossy(), "name": name, "output": output, "summary": parse_tabbed_status(&output)}))
}

fn checkpoint_request(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let session_id = required_str(&args, "session_id")?;
    let output = run_agentcall_owned(
        config,
        &root,
        vec!["checkpoint".to_string(), "request".to_string(), session_id.to_string()],
    )?;
    serde_json::from_str(&output).map_err(|err| format!("invalid checkpoint JSON: {err}"))
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

fn run_agentcall_owned(config: &Config, root: &Path, args: Vec<String>) -> Result<String, String> {
    let mut command = Command::new(&config.python);
    command
        .arg("-m")
        .arg("agentcall")
        .arg("--root")
        .arg(root)
        .args(args)
        .current_dir(&config.workspace);
    command.env("PYTHONPATH", config.workspace.join("src"));
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

fn parse_tabbed_status(output: &str) -> Value {
    let mut object = serde_json::Map::new();
    for segment in output.split('\t') {
        if let Some((key, value)) = segment.split_once('=') {
            object.insert(key.trim().to_string(), json!(value.trim()));
        }
    }
    Value::Object(object)
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
        assert!(names.contains(&"agentcall_route_task".to_string()));
        assert!(names.contains(&"agentcall_context_packet_create".to_string()));
        assert!(names.contains(&"agentcall_hook_ingest".to_string()));
        assert!(names.contains(&"agentcall_board".to_string()));
        assert!(names.contains(&"agentcall_session_spawn".to_string()));
        assert!(names.contains(&"agentcall_session_send".to_string()));
    }

    #[test]
    fn parses_key_value_output() {
        let parsed = parse_key_value_output("task_id: task-0001\nstatus: accepted\nreports: 2");
        assert_eq!(parsed["task_id"], "task-0001");
        assert_eq!(parsed["status"], "accepted");
        assert_eq!(parsed["reports"], "2");
    }
}
