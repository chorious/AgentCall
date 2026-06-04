use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub(crate) struct AcpInvocation {
    pub(crate) command: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) mode: String,
    pub(crate) prompt: String,
    pub(crate) timeout_seconds: u64,
}

pub(crate) fn run_acp_invocation(invocation: AcpInvocation) -> Result<Value, String> {
    if invocation.command.is_empty() {
        return Err("ACP command cannot be empty".to_string());
    }
    let timeout = Duration::from_secs(invocation.timeout_seconds.clamp(1, 900));
    let deadline = Instant::now() + timeout;
    let mut child = Command::new(&invocation.command[0])
        .args(&invocation.command[1..])
        .current_dir(&invocation.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to start ACP server: {err}"))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "ACP server stdin was not captured".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "ACP server stdout was not captured".to_string())?;
    let stderr = child.stderr.take();
    let stdout_rx = spawn_stdout_reader(stdout);
    let stderr_thread = thread::spawn(move || read_pipe(stderr));
    let mut client = JsonRpcClient::new(&mut stdin, stdout_rx);
    let mut updates = Vec::new();

    let initialize = client.call(
        &mut child,
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": {"readTextFile": false, "writeTextFile": false},
                "terminal": false
            },
            "clientInfo": {"name": "agentcall", "title": "AgentCall", "version": "0.8.1"}
        }),
        deadline,
        &mut updates,
    )?;
    let session = client.call(
        &mut child,
        "session/new",
        json!({"cwd": invocation.cwd.to_string_lossy(), "mcpServers": []}),
        deadline,
        &mut updates,
    )?;
    let session_id = session
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| "ACP session/new response did not include sessionId".to_string())?
        .to_string();
    let _ = client.call(
        &mut child,
        "session/set_mode",
        json!({"sessionId": session_id, "modeId": invocation.mode}),
        deadline,
        &mut updates,
    )?;
    let prompt_result = client.call(
        &mut child,
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": invocation.prompt}]
        }),
        deadline,
        &mut updates,
    )?;
    let text = agent_text(&updates);
    let stop_reason = prompt_result
        .get("stopReason")
        .and_then(Value::as_str)
        .map(str::to_string);

    drop(stdin);
    let process = wait_or_kill(&mut child, Duration::from_secs(3))?;
    let stderr = stderr_thread.join().unwrap_or_default();
    Ok(json!({
        "status": if process.get("success").and_then(Value::as_bool).unwrap_or(false) { "completed" } else { "completed_with_process_error" },
        "protocol": "acp-json-rpc-stdio",
        "command": invocation.command,
        "cwd": invocation.cwd,
        "session_id": session_id,
        "stop_reason": stop_reason,
        "initialize": initialize,
        "updates": updates,
        "update_count": updates.len(),
        "text": text,
        "stderr": stderr,
        "process": process,
    }))
}

struct JsonRpcClient<'a> {
    stdin: &'a mut dyn Write,
    stdout_rx: Receiver<Result<Value, String>>,
    next_id: u64,
}

impl<'a> JsonRpcClient<'a> {
    fn new(stdin: &'a mut dyn Write, stdout_rx: Receiver<Result<Value, String>>) -> Self {
        Self {
            stdin,
            stdout_rx,
            next_id: 0,
        }
    }

    fn call(
        &mut self,
        child: &mut Child,
        method: &str,
        params: Value,
        deadline: Instant,
        updates: &mut Vec<Value>,
    ) -> Result<Value, String> {
        let request_id = self.next_id;
        self.next_id += 1;
        self.write(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params
        }))?;
        loop {
            if Instant::now() >= deadline {
                kill_child(child);
                return Err(format!("ACP call {method} timed out"));
            }
            if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
                return Err(format!(
                    "ACP process exited during {method}: code={:?} success={}",
                    status.code(),
                    status.success()
                ));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let wait = remaining.min(Duration::from_millis(50));
            let message = match self.stdout_rx.recv_timeout(wait) {
                Ok(Ok(message)) => message,
                Ok(Err(err)) => return Err(err),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(format!("ACP stdout closed while waiting for {method}"));
                }
            };
            if is_agent_request(&message) {
                self.handle_agent_request(&message)?;
                continue;
            }
            if message.get("method").and_then(Value::as_str) == Some("session/update") {
                updates.push(message.get("params").cloned().unwrap_or_else(|| json!({})));
                continue;
            }
            if message.get("id").and_then(Value::as_u64) != Some(request_id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(format!("ACP {method} failed: {error}"));
            }
            return Ok(message.get("result").cloned().unwrap_or_else(|| json!({})));
        }
    }

    fn write(&mut self, message: Value) -> Result<(), String> {
        let line = serde_json::to_string(&message).map_err(|err| err.to_string())?;
        writeln!(self.stdin, "{line}")
            .map_err(|err| format!("failed to write ACP stdin: {err}"))?;
        self.stdin
            .flush()
            .map_err(|err| format!("failed to flush ACP stdin: {err}"))
    }

    fn handle_agent_request(&mut self, message: &Value) -> Result<(), String> {
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let request_id = message.get("id").cloned().unwrap_or_else(|| json!(null));
        if method == "session/request_permission" {
            let selected = select_permission_option(message);
            return self.write(json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "result": {"outcome": {"outcome": "selected", "optionId": selected}}
            }));
        }
        self.write(json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": -32601, "message": format!("Unsupported client method: {method}")}
        }))
    }
}

fn spawn_stdout_reader(stdout: impl Read + Send + 'static) -> Receiver<Result<Value, String>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let line = match line {
                Ok(line) => line,
                Err(err) => {
                    let _ = tx.send(Err(format!("failed to read ACP stdout: {err}")));
                    return;
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let value = match serde_json::from_str::<Value>(&line) {
                Ok(Value::Object(_)) => serde_json::from_str::<Value>(&line).unwrap(),
                Ok(value) => {
                    let _ = tx.send(Err(format!("ACP emitted non-object JSON message: {value}")));
                    return;
                }
                Err(err) => {
                    let _ = tx.send(Err(format!(
                        "ACP emitted invalid JSON line: {line:?}: {err}"
                    )));
                    return;
                }
            };
            if tx.send(Ok(value)).is_err() {
                return;
            }
        }
    });
    rx
}

fn read_pipe(pipe: Option<impl Read>) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut bytes = Vec::new();
    let _ = pipe.read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).to_string()
}

fn is_agent_request(message: &Value) -> bool {
    message.get("id").is_some() && message.get("method").is_some()
}

fn select_permission_option(message: &Value) -> Value {
    let options = message
        .get("params")
        .and_then(|params| params.get("options"))
        .and_then(Value::as_array);
    let Some(options) = options else {
        return Value::Null;
    };
    for option in options {
        if option
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .starts_with("allow")
        {
            return option.get("optionId").cloned().unwrap_or(Value::Null);
        }
    }
    options
        .first()
        .and_then(|option| option.get("optionId"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn agent_text(updates: &[Value]) -> String {
    let mut text = String::new();
    for item in updates {
        let update = item.get("update").unwrap_or(item);
        if update.get("sessionUpdate").and_then(Value::as_str) != Some("agent_message_chunk") {
            continue;
        }
        let content = update.get("content").unwrap_or(update);
        if content.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(chunk) = content.get("text").and_then(Value::as_str) {
                text.push_str(chunk);
            }
        }
    }
    text
}

fn wait_or_kill(child: &mut Child, wait: Duration) -> Result<Value, String> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|err| err.to_string())? {
            return Ok(
                json!({"kind": "exited", "code": status.code(), "success": status.success()}),
            );
        }
        if started.elapsed() >= wait {
            kill_child(child);
            let status = child.wait().map_err(|err| err.to_string())?;
            return Ok(
                json!({"kind": "killed_after_prompt", "code": status.code(), "success": false}),
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn kill_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_selects_allow_option_first() {
        let message = json!({
            "id": 7,
            "method": "session/request_permission",
            "params": {
                "options": [
                    {"optionId": "reject-once", "kind": "reject_once"},
                    {"optionId": "allow-once", "kind": "allow_once"}
                ]
            }
        });
        assert_eq!(select_permission_option(&message), json!("allow-once"));
    }

    #[test]
    fn agent_text_extracts_message_chunks() {
        let updates = vec![
            json!({"update": {"sessionUpdate": "current_mode_update", "modeId": "plan"}}),
            json!({"update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hello"}}}),
            json!({"update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": " world"}}}),
        ];
        assert_eq!(agent_text(&updates), "hello world");
    }
}
