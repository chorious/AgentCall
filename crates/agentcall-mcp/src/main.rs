use serde_json::{json, Value};
use std::env;
use std::io::{self, BufRead, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Command;

const PROTOCOL_VERSION: &str = "2024-11-05";
const SERVER_NAME: &str = "agentcall-mcp";
const SERVER_VERSION: &str = "0.5.0";
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
    daemon_url: String,
}

impl Config {
    fn from_args(args: Vec<String>) -> Result<Self, String> {
        let mut workspace = env::current_dir().map_err(|err| err.to_string())?;
        let mut python = "python".to_string();
        let mut daemon_url = "http://127.0.0.1:3293".to_string();
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
                "--daemon-url" => {
                    index += 1;
                    daemon_url = args
                        .get(index)
                        .ok_or("missing --daemon-url value")?
                        .to_string();
                }
                "--help" | "-h" => {
                    return Err(
                        "usage: agentcall-mcp [--workspace PATH] [--python PYTHON] [--daemon-url URL]".to_string(),
                    );
                }
                other => return Err(format!("unknown argument: {other}")),
            }
            index += 1;
        }
        Ok(Self { workspace, python, daemon_url })
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
            "name": "agentcall_runtime_health",
            "description": "Return AgentCall daemon health and state-writer status.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        }),
        json!({
            "name": "agentcall_project_sessions",
            "description": "Return projects and sessions known by the AgentCall daemon.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        }),
        json!({
            "name": "agentcall_session_summary",
            "description": "Return event-first summary for one PTY session from the AgentCall daemon.",
            "inputSchema": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_concurrency_probe",
            "description": "Return daemon-side concurrency diagnostics. This is diagnostic only, not an acceptance test.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        }),
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
                    "claude_workspace": {"type": "string", "description": "Claude ACP/headless working directory. Defaults to AGENTCALL_CLAUDE_WORKSPACE or current directory."},
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
                    "needs_continuity": {"type": "boolean", "default": false},
                    "risk": {"type": "string"},
                    "phase": {"type": "string"},
                    "expected_minutes": {"type": "integer", "minimum": 0},
                    "parallel_children": {"type": "integer", "minimum": 0}
                },
                "required": ["objective"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_codex_preflight",
            "description": "Codex turn-start preflight: inspect AgentCall state and return required next checks/actions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "objective": {"type": "string"},
                    "phase": {"type": "string", "default": "turn_start"}
                },
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
                    "claude_workspace": {"type": "string", "description": "Claude ACP working directory. Defaults to AGENTCALL_CLAUDE_WORKSPACE or current directory."},
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
            "name": "agentcall_file_claims",
            "description": "Return current AgentCall file claims and conflict ownership.",
            "inputSchema": {
                "type": "object",
                "properties": {"root": {"type": "string"}},
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_transcript_index",
            "description": "Index one Claude Code transcript JSONL file into AgentCall state.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "root": {"type": "string"},
                    "path": {"type": "string"},
                    "session_id": {"type": "string"}
                },
                "required": ["path"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "agentcall_transcripts_list",
            "description": "Return transcript summaries indexed by AgentCall.",
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
                    "cwd": {"type": "string", "default": "D:\\guKimi", "description": "PTY child working directory. Defaults to the Claude Code workspace."},
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
    let event_args = args.clone();
    let result = match name {
        "agentcall_runtime_health" => daemon_get(config, "/api/runtime/health"),
        "agentcall_project_sessions" => daemon_get(config, "/api/projects"),
        "agentcall_session_summary" => session_summary(config, args),
        "agentcall_concurrency_probe" => concurrency_probe(config),
        "agentcall_capabilities" => Ok(capabilities(config)),
        "agentcall_report_schema" => python_json(config, &config.workspace, &["-c", REPORT_SCHEMA_SNIPPET]),
        "agentcall_workflow_simulate" => workflow_simulate(config, args),
        "agentcall_workflow_inspect" => workflow_inspect(config, args),
        "agentcall_codex_preflight" => codex_preflight(config, args),
        "agentcall_route_task" => route_task_tool(config, args),
        "agentcall_context_packet_create" => context_packet_create(config, args),
        "agentcall_delegate_acp" => delegate_acp(config, args),
        "agentcall_events_tail" => events_tail(config, args),
        "agentcall_reports_list" => reports_list(config, args),
        "agentcall_board" => board(config, args),
        "agentcall_file_claims" => file_claims(config, args),
        "agentcall_transcript_index" => transcript_index(config, args),
        "agentcall_transcripts_list" => transcripts_list(config, args),
        "agentcall_hook_ingest" => hook_ingest(config, args),
        "agentcall_session_spawn" => session_spawn(config, args),
        "agentcall_session_list" => session_list(config, args),
        "agentcall_session_status" => session_status(config, args),
        "agentcall_session_tail" => session_tail(config, args),
        "agentcall_session_send" => session_send(config, args),
        "agentcall_checkpoint_request" => checkpoint_request(config, args),
        _ => Err(format!("unknown tool: {name}")),
    };
    let status = if result.is_ok() { "ok" } else { "error" };
    let message = result.as_ref().err().map(String::as_str).unwrap_or("");
    emit_mcp_event(config, name, &event_args, status, message);
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
        "daemon_url": config.daemon_url,
        "claude_workspace": claude_workspace_from_args(&json!({})),
        "features": {
            "workflow_simulate": true,
            "workflow_inspect": true,
            "report_schema": true,
            "driver_discovery": true,
            "route_task": true,
            "context_packet_create": true,
            "hook_ingest": true,
            "codex_preflight": true,
            "checkpoint_request": true,
            "board": true,
            "session_control": true,
            "file_claims": true,
            "transcripts": true
            ,
            "runtime_health": true,
            "session_summary": true
        },
        "drivers": [
            {"kind": "acp", "available": true, "live_model": true, "costs_tokens": true, "default": true, "transport": "stdio-json-rpc"},
            {"kind": "scripted", "available": true, "live_model": false, "costs_tokens": false, "test_only": true}
        ],
        "tools": [
            "agentcall_runtime_health",
            "agentcall_project_sessions",
            "agentcall_session_summary",
            "agentcall_concurrency_probe",
            "agentcall_capabilities",
            "agentcall_report_schema",
            "agentcall_workflow_simulate",
            "agentcall_workflow_inspect",
            "agentcall_codex_preflight",
            "agentcall_route_task",
            "agentcall_context_packet_create",
            "agentcall_delegate_acp",
            "agentcall_events_tail",
            "agentcall_reports_list",
            "agentcall_board",
            "agentcall_file_claims",
            "agentcall_transcript_index",
            "agentcall_transcripts_list",
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

fn session_summary(config: &Config, args: Value) -> Result<Value, String> {
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .ok_or("missing name")?;
    daemon_get(config, &format!("/api/sessions/{}/summary", url_encode(name)))
}

fn concurrency_probe(config: &Config) -> Result<Value, String> {
    let health = daemon_get(config, "/api/runtime/health")?;
    Ok(json!({
        "diagnostic_only": true,
        "acceptance_source": "L0/L1/L2/L3 tests, not this probe",
        "daemon": health
    }))
}

fn daemon_get(config: &Config, path: &str) -> Result<Value, String> {
    daemon_request(config, "GET", path, None)
}

fn daemon_post_json(config: &Config, path: &str, body: Value) -> Result<Value, String> {
    daemon_request(config, "POST", path, Some(body))
}

fn daemon_request(config: &Config, method: &str, path: &str, body: Option<Value>) -> Result<Value, String> {
    let (host, port) = parse_daemon_url(&config.daemon_url)?;
    let mut stream = TcpStream::connect((host.as_str(), port))
        .map_err(|err| format!("failed to connect daemon {}: {err}", config.daemon_url))?;
    let body_text = body
        .map(|value| serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string()))
        .unwrap_or_default();
    let request = if method == "POST" {
        format!(
            "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body_text.as_bytes().len(),
            body_text
        )
    } else {
        format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n")
    };
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("failed to write daemon request: {err}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|err| format!("failed to read daemon response: {err}"))?;
    let Some((head, body)) = response.split_once("\r\n\r\n") else {
        return Err("invalid daemon response".to_string());
    };
    if !head.starts_with("HTTP/1.1 200") {
        return Err(format!("daemon returned non-200 response: {}", head.lines().next().unwrap_or(head)));
    }
    serde_json::from_str(body).map_err(|err| format!("invalid daemon JSON: {err}"))
}

fn parse_daemon_url(url: &str) -> Result<(String, u16), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or("daemon-url must start with http://")?;
    let host_port = rest.trim_end_matches('/');
    let (host, port) = host_port
        .rsplit_once(':')
        .ok_or("daemon-url must include a port")?;
    let port = port
        .parse::<u16>()
        .map_err(|err| format!("invalid daemon port: {err}"))?;
    Ok((host.to_string(), port))
}

fn url_encode(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

fn workflow_simulate(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let driver = args.get("driver").and_then(Value::as_str).unwrap_or("acp");
    let max_turns = args.get("max_turns").and_then(Value::as_i64).unwrap_or(1).to_string();
    let mut command = vec![
        "workflow".to_string(),
        "simulate".to_string(),
        "--driver".to_string(),
        driver.to_string(),
        "--max-turns".to_string(),
        max_turns,
    ];
    if driver != "scripted" {
        command.push("--claude-workspace".to_string());
        command.push(claude_workspace_from_args(&args));
    }
    let output = run_agentcall_owned(config, &root, command)?;
    Ok(json!({
        "root": root.to_string_lossy(),
        "claude_workspace": claude_workspace_from_args(&args),
        "output": output,
        "summary": parse_key_value_output(&output)
    }))
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

fn codex_preflight(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let phase = args
        .get("phase")
        .and_then(Value::as_str)
        .unwrap_or("turn_start");
    let objective = args
        .get("objective")
        .and_then(Value::as_str)
        .unwrap_or("");
    let board = board(config, json!({"root": root.to_string_lossy()}))?;
    let active_sessions = board
        .get("active_sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let file_claims = board
        .get("file_claims")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let reports = board
        .get("reports")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let active_claims: Vec<Value> = file_claims
        .iter()
        .filter(|claim| claim.get("status").and_then(Value::as_str) == Some("active"))
        .cloned()
        .collect();
    let sessions_needing_attention: Vec<Value> = active_sessions
        .iter()
        .filter(|session| {
            matches!(
                session.get("status").and_then(Value::as_str),
                Some("needs_permission" | "checkpoint_requested" | "checkpoint_due" | "blocked")
            )
        })
        .cloned()
        .collect();

    let mut required_checks = vec!["agentcall_board"];
    let mut next_actions = Vec::new();
    let mut warnings = Vec::new();

    if !objective.is_empty() && phase == "turn_start" {
        required_checks.push("agentcall_route_task");
        next_actions.push(json!({
            "tool": "agentcall_route_task",
            "reason": "choose ACP bounded child call vs PTY handoff route before delegating",
            "objective": objective,
        }));
    }

    if !active_claims.is_empty() || phase == "before_edit" {
        required_checks.push("agentcall_file_claims");
        next_actions.push(json!({
            "tool": "agentcall_file_claims",
            "reason": "avoid cross-agent edits to claimed files",
        }));
    }

    if phase == "before_final" || !sessions_needing_attention.is_empty() {
        required_checks.push("agentcall_reports_list");
        required_checks.push("agentcall_events_tail");
        next_actions.push(json!({
            "tool": "agentcall_reports_list",
            "reason": "accept clean child reports directly; write review only when drift/blockers need revision",
        }));
    }

    if !sessions_needing_attention.is_empty() {
        warnings.push(json!({
            "kind": "session_attention",
            "message": "Some child/handoff sessions need permission, checkpoint, or blocker handling.",
            "sessions": sessions_needing_attention,
        }));
    }
    if !active_claims.is_empty() {
        warnings.push(json!({
            "kind": "active_file_claims",
            "message": "There are active file claims; inspect before editing overlapping files.",
            "claims": active_claims,
        }));
    }

    required_checks.sort();
    required_checks.dedup();

    Ok(json!({
        "root": root.to_string_lossy(),
        "phase": phase,
        "objective": objective,
        "required_checks": required_checks,
        "next_actions": next_actions,
        "warnings": warnings,
        "summary": {
            "active_sessions": active_sessions.len(),
            "file_claims": file_claims.len(),
            "reports": reports.len(),
        },
        "route_model": {
            "claude_workspace": claude_workspace_from_args(&args),
            "shared_lifecycle": [
                "context_packet",
                "bounded_execution_or_handoff",
                "events_hooks",
                "structured_report",
                "parent_validation",
                "board_update"
            ],
            "only_difference": "route/runtime adapter: acp for agents-as-tools, pty for visible handoff/debug"
        }
    }))
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
    for (json_key, cli_key) in [
        ("risk", "--risk"),
        ("phase", "--phase"),
        ("expected_minutes", "--expected-minutes"),
        ("parallel_children", "--parallel-children"),
    ] {
        if let Some(value) = args.get(json_key) {
            if let Some(text) = value.as_str() {
                command.push(cli_key.to_string());
                command.push(text.to_string());
            } else if let Some(number) = value.as_i64() {
                command.push(cli_key.to_string());
                command.push(number.to_string());
            }
        }
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
    let _ = args;
    daemon_get(config, "/api/board")
}

fn file_claims(config: &Config, args: Value) -> Result<Value, String> {
    let _ = args;
    daemon_get(config, "/api/file-claims")
}

fn transcript_index(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let path = required_str(&args, "path")?;
    let mut command = vec!["transcript".to_string(), "index".to_string(), path.to_string()];
    if let Some(session_id) = args.get("session_id").and_then(Value::as_str) {
        command.push("--session-id".to_string());
        command.push(session_id.to_string());
    }
    let output = run_agentcall_owned(config, &root, command)?;
    serde_json::from_str(&output).map_err(|err| format!("invalid transcript index JSON: {err}"))
}

fn transcripts_list(config: &Config, args: Value) -> Result<Value, String> {
    let root = root_from_args(config, &args);
    let output = run_agentcall_owned(config, &root, vec!["transcript".to_string(), "list".to_string()])?;
    serde_json::from_str(&output).map_err(|err| format!("invalid transcripts JSON: {err}"))
}

fn hook_ingest(config: &Config, args: Value) -> Result<Value, String> {
    let event = required_str(&args, "event")?;
    let payload = args.get("payload").cloned().unwrap_or(json!({}));
    daemon_post_json(config, "/api/hooks/ingest", json!({"event": event, "payload": payload}))
}

fn session_spawn(config: &Config, args: Value) -> Result<Value, String> {
    let name = required_str(&args, "name")?;
    let command_args = string_array(&args, "command");
    if command_args.is_empty() {
        return Err("session command cannot be empty".to_string());
    }
    let cols = args.get("cols").and_then(Value::as_i64).unwrap_or(100);
    let rows = args.get("rows").and_then(Value::as_i64).unwrap_or(40);
    let cwd = args
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| claude_workspace_from_args(&args));
    let effective_cwd = if is_claude_command(&command_args) {
        claude_workspace_from_args(&args)
    } else {
        cwd.clone()
    };
    let response = daemon_post_json(
        config,
        "/api/sessions",
        json!({
            "name": name,
            "command": command_args,
            "cwd": effective_cwd,
            "cols": cols,
            "rows": rows
        }),
    )?;
    Ok(json!({
        "cwd": effective_cwd,
        "requested_cwd": cwd,
        "cwd_policy": if is_claude_command(&string_array(&args, "command")) { "force_claude_workspace" } else { "requested_or_default" },
        "session": response
    }))
}

fn session_list(config: &Config, args: Value) -> Result<Value, String> {
    let _ = args;
    daemon_get(config, "/api/sessions")
}

fn session_status(config: &Config, args: Value) -> Result<Value, String> {
    let name = required_str(&args, "name")?;
    daemon_get(config, &format!("/api/sessions/{}/summary", url_encode(name)))
}

fn session_tail(config: &Config, args: Value) -> Result<Value, String> {
    let _ = config;
    let _ = args;
    Err("agentcall_session_tail is disabled in v0.6 daemon-single-writer mode; use session_summary or the viewer stream".to_string())
}

fn session_send(config: &Config, args: Value) -> Result<Value, String> {
    let name = required_str(&args, "name")?;
    let text = required_str(&args, "text")?;
    let enter = args.get("enter").and_then(Value::as_bool).unwrap_or(true);
    daemon_post_json(
        config,
        &format!("/api/sessions/{}/input", url_encode(name)),
        json!({"text": text, "enter": enter}),
    )
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

fn claude_workspace_from_args(args: &Value) -> String {
    args.get("claude_workspace")
        .or_else(|| args.get("cwd"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| env::var("AGENTCALL_CLAUDE_WORKSPACE").ok())
        .or_else(|| env::current_dir().ok().map(|path| path.to_string_lossy().to_string()))
        .unwrap_or_else(|| ".".to_string())
}

fn is_claude_command(command: &[String]) -> bool {
    command
        .first()
        .map(|program| {
            let name = Path::new(program)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(program)
                .to_ascii_lowercase();
            name == "claude" || name == "claude.exe"
        })
        .unwrap_or(false)
}

fn emit_mcp_event(config: &Config, tool_name: &str, args: &Value, status: &str, message: &str) {
    let data = json!({
        "tool": tool_name,
        "status": status,
        "arguments": args,
        "claude_workspace": claude_workspace_from_args(args),
        "runtime": "mcp",
        "error": message,
    });
    let event_message = if message.is_empty() {
        format!("MCP tool {tool_name} completed.")
    } else {
        format!("MCP tool {tool_name} failed.")
    };
    if let Err(err) = daemon_post_json(
        config,
        "/api/events",
        json!({
            "event_type": "mcp.tool_called",
            "message": event_message,
            "data": data
        }),
    ) {
        eprintln!("agentcall-mcp: failed to emit event: {err}");
    }
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
        assert!(names.contains(&"agentcall_codex_preflight".to_string()));
        assert!(names.contains(&"agentcall_route_task".to_string()));
        assert!(names.contains(&"agentcall_context_packet_create".to_string()));
        assert!(names.contains(&"agentcall_hook_ingest".to_string()));
        assert!(names.contains(&"agentcall_board".to_string()));
        assert!(names.contains(&"agentcall_session_spawn".to_string()));
        assert!(names.contains(&"agentcall_session_send".to_string()));
        assert!(names.contains(&"agentcall_file_claims".to_string()));
        assert!(names.contains(&"agentcall_transcript_index".to_string()));
        assert!(names.contains(&"agentcall_transcripts_list".to_string()));
    }

    #[test]
    fn parses_key_value_output() {
        let parsed = parse_key_value_output("task_id: task-0001\nstatus: accepted\nreports: 2");
        assert_eq!(parsed["task_id"], "task-0001");
        assert_eq!(parsed["status"], "accepted");
        assert_eq!(parsed["reports"], "2");
    }
}
