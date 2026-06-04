use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone)]
pub(crate) struct AcpInvocation {
    pub(crate) command: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) wrapper_session: String,
    pub(crate) mode: String,
    pub(crate) prompt: String,
    pub(crate) timeout_seconds: u64,
    pub(crate) permission_policy: Option<AcpPermissionPolicy>,
    pub(crate) progress: Option<AcpProgressCallback>,
}

pub(crate) type AcpProgressCallback = Arc<dyn Fn(Value) + Send + Sync>;

#[derive(Clone, Debug)]
pub(crate) struct AcpPermissionPolicy {
    pub(crate) template: String,
    pub(crate) target_files: Vec<String>,
    pub(crate) allowed_paths: Vec<String>,
    pub(crate) report_path: Option<String>,
}

pub(crate) fn run_acp_invocation(invocation: AcpInvocation) -> Result<Value, String> {
    if invocation.command.is_empty() {
        return Err("ACP command cannot be empty".to_string());
    }
    let timeout = Duration::from_secs(invocation.timeout_seconds.clamp(1, 900));
    let deadline = Instant::now() + timeout;
    let mut command = Command::new(&invocation.command[0]);
    command.args(&invocation.command[1..]).current_dir(&invocation.cwd);
    for (key, value) in acp_env_vars(&invocation.wrapper_session) {
        command.env(key, value);
    }
    let mut child = command
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
    let mut updates = Vec::new();

    let protocol_result = {
        let mut client = JsonRpcClient::new(
            &mut stdin,
            stdout_rx,
            invocation.permission_policy.clone(),
            invocation.progress.clone(),
        );
        run_acp_protocol(&invocation, &mut child, &mut client, deadline, &mut updates)
    };
    drop(stdin);

    match protocol_result {
        Ok(mut result) => {
            let process = wait_or_kill(&mut child, Duration::from_secs(3))?;
            let stderr = stderr_thread.join().unwrap_or_default();
            let status = if process
                .get("success")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                "completed"
            } else {
                "completed_with_process_error"
            };
            if let Some(object) = result.as_object_mut() {
                object.insert("status".to_string(), json!(status));
                object.insert("stderr".to_string(), json!(stderr));
                object.insert("process".to_string(), process);
            }
            Ok(result)
        }
        Err(err) => {
            let process = wait_or_kill(&mut child, Duration::from_secs(3))
                .unwrap_or_else(|wait_err| json!({"kind": "wait_error", "error": wait_err}));
            let stderr = stderr_thread.join().unwrap_or_default();
            Err(format!(
                "{err}; process={}; stderr_tail={:?}; command={:?}; cwd={}; wrapper_session={}",
                compact_json(&process),
                stderr_tail(&stderr, 2000),
                invocation.command,
                invocation.cwd.display(),
                invocation.wrapper_session
            ))
        }
    }
}

fn run_acp_protocol(
    invocation: &AcpInvocation,
    child: &mut Child,
    client: &mut JsonRpcClient<'_>,
    deadline: Instant,
    updates: &mut Vec<Value>,
) -> Result<Value, String> {
    let initialize = client.call(
        child,
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": {"readTextFile": false, "writeTextFile": false},
                "terminal": false
            },
            "clientInfo": {"name": "agentcall", "title": "AgentCall", "version": "2.2.0"}
        }),
        deadline,
        updates,
    )?;
    let session = client.call(
        child,
        "session/new",
        json!({"cwd": invocation.cwd.to_string_lossy(), "mcpServers": []}),
        deadline,
        updates,
    )?;
    let session_id = session
        .get("sessionId")
        .and_then(Value::as_str)
        .ok_or_else(|| "ACP session/new response did not include sessionId".to_string())?
        .to_string();
    let _ = client.call(
        child,
        "session/set_mode",
        json!({"sessionId": session_id, "modeId": invocation.mode}),
        deadline,
        updates,
    )?;
    let prompt_result = client.call(
        child,
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": invocation.prompt}]
        }),
        deadline,
        updates,
    )?;
    let text = agent_text(updates);
    let stop_reason = prompt_result
        .get("stopReason")
        .and_then(Value::as_str)
        .map(str::to_string);
    Ok(json!({
        "protocol": "acp-json-rpc-stdio",
        "command": invocation.command,
        "cwd": invocation.cwd,
        "wrapper_session": invocation.wrapper_session,
        "session_id": session_id,
        "stop_reason": stop_reason,
        "initialize": initialize,
        "updates": updates,
        "update_count": updates.len(),
        "text": text,
    }))
}

struct JsonRpcClient<'a> {
    stdin: &'a mut dyn Write,
    stdout_rx: Receiver<Result<Value, String>>,
    next_id: u64,
    permission_policy: Option<AcpPermissionPolicy>,
    progress: Option<AcpProgressCallback>,
}

impl<'a> JsonRpcClient<'a> {
    fn new(
        stdin: &'a mut dyn Write,
        stdout_rx: Receiver<Result<Value, String>>,
        permission_policy: Option<AcpPermissionPolicy>,
        progress: Option<AcpProgressCallback>,
    ) -> Self {
        Self {
            stdin,
            stdout_rx,
            next_id: 0,
            permission_policy,
            progress,
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
                let update = message.get("params").cloned().unwrap_or_else(|| json!({}));
                if let Some(progress) = &self.progress {
                    progress(update.clone());
                }
                updates.push(update);
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
            let selected = select_permission_option(message, self.permission_policy.as_ref());
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

fn select_permission_option(message: &Value, policy: Option<&AcpPermissionPolicy>) -> Value {
    let options = message
        .get("params")
        .and_then(|params| params.get("options"))
        .and_then(Value::as_array);
    let Some(options) = options else {
        return Value::Null;
    };
    if let Some(policy) = policy {
        let allowed = permission_request_allowed(message, policy);
        let desired = if allowed { "allow" } else { "reject" };
        for option in options {
            if option
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .starts_with(desired)
            {
                return option.get("optionId").cloned().unwrap_or(Value::Null);
            }
        }
        if !allowed {
            for option in options {
                let text = serde_json::to_string(option).unwrap_or_default().to_ascii_lowercase();
                if text.contains("deny") || text.contains("reject") {
                    return option.get("optionId").cloned().unwrap_or(Value::Null);
                }
            }
        }
    }
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

fn permission_request_allowed(message: &Value, policy: &AcpPermissionPolicy) -> bool {
    let _template_name = policy.template.as_str();
    let haystack = serde_json::to_string(message)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let tool = extract_tool_name(message).unwrap_or_default().to_ascii_lowercase();
    if tool.contains("bash") || haystack.contains("\"bash\"") || haystack.contains("\"command\"") {
        return bash_request_allowed(message);
    }
    if tool.contains("write")
        || tool.contains("edit")
        || haystack.contains("multiedit")
        || haystack.contains("write")
    {
        let Some(report_path) = policy.report_path.as_deref() else {
            return false;
        };
        let paths = collect_path_strings(message);
        return !paths.is_empty()
            && paths
                .iter()
                .all(|path| same_normalized_path(path, report_path));
    }
    if tool.contains("read") || tool.contains("grep") || tool.contains("glob") {
        let paths = collect_path_strings(message);
        if paths.is_empty() {
            return true;
        }
        return paths.iter().all(|path| {
            policy
                .target_files
                .iter()
                .any(|allowed| same_normalized_path(path, allowed))
                || policy
                    .allowed_paths
                    .iter()
                    .any(|allowed| path_within(path, allowed))
            });
    }
    let paths = collect_path_strings(message);
    if !paths.is_empty() {
        return paths.iter().all(|path| {
            policy
                .target_files
                .iter()
                .any(|allowed| same_normalized_path(path, allowed))
                || policy
                    .allowed_paths
                    .iter()
                    .any(|allowed| path_within(path, allowed))
        });
    }
    // Unknown permission requests stay default-deny inside SOP ACP.
    false
}

fn bash_request_allowed(message: &Value) -> bool {
    let command = find_string_key(message, &["command", "cmd", "script"]).unwrap_or_default();
    let trimmed = command.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return false;
    }
    let forbidden = [
        ">", ">>", "| tee", "set-content", "out-file", "new-item", "remove-item", "del ", "erase ",
        "rm ", "move-item", "mv ", "copy-item", "cp ", "mkdir", "rmdir", "echo ",
    ];
    if forbidden.iter().any(|needle| trimmed.contains(needle)) {
        return false;
    }
    let readonly = [
        "pwd", "cd", "ls", "dir", "cat ", "type ", "rg ", "findstr ", "git status", "git diff",
        "git show",
    ];
    readonly.iter().any(|prefix| trimmed.starts_with(prefix))
}

fn extract_tool_name(value: &Value) -> Option<String> {
    find_string_key(value, &["tool_name", "toolName", "tool"])
}

fn find_string_key(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(found) = map.get(*key).and_then(Value::as_str) {
                    return Some(found.to_string());
                }
            }
            map.values().find_map(|child| find_string_key(child, keys))
        }
        Value::Array(items) => items.iter().find_map(|child| find_string_key(child, keys)),
        _ => None,
    }
}

fn collect_path_strings(value: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    collect_path_strings_inner(value, "", &mut paths);
    paths.sort();
    paths.dedup();
    paths
}

fn collect_path_strings_inner(value: &Value, key: &str, paths: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (child_key, child) in map {
                collect_path_strings_inner(child, child_key, paths);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_path_strings_inner(child, key, paths);
            }
        }
        Value::String(text) => {
            let key = key.to_ascii_lowercase();
            if key.contains("path") || key.contains("file") || looks_like_path(text) {
                paths.push(text.clone());
            }
        }
        _ => {}
    }
}

fn looks_like_path(value: &str) -> bool {
    value.contains(":\\")
        || value.contains("\\")
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with('/')
        || (value.contains('/') && value.contains('.'))
}

fn normalize_path(value: &str) -> String {
    value.replace('/', "\\").trim_end_matches('\\').to_ascii_lowercase()
}

fn same_normalized_path(left: &str, right: &str) -> bool {
    normalize_path(left) == normalize_path(right)
}

fn path_within(path: &str, parent: &str) -> bool {
    let path = normalize_path(path);
    let parent = normalize_path(parent);
    path == parent || path.starts_with(&(parent + "\\"))
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

fn acp_env_vars(wrapper_session: &str) -> Vec<(&'static str, String)> {
    vec![
        ("PYTHONUTF8", "1".to_string()),
        ("PYTHONIOENCODING", "utf-8".to_string()),
        ("LANG", "C.UTF-8".to_string()),
        ("LC_ALL", "C.UTF-8".to_string()),
        ("AGENTCALL_WRAPPER_SESSION", wrapper_session.to_string()),
    ]
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

fn stderr_tail(stderr: &str, max_chars: usize) -> String {
    let chars: Vec<char> = stderr.chars().collect();
    let start = chars.len().saturating_sub(max_chars);
    chars[start..].iter().collect()
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
        assert_eq!(select_permission_option(&message, None), json!("allow-once"));
    }

    #[test]
    fn permission_policy_rejects_write_outside_report_path() {
        let policy = AcpPermissionPolicy {
            template: "read-and-report".to_string(),
            target_files: vec!["E:\\Project\\AgentCall\\README.md".to_string()],
            allowed_paths: vec!["E:\\Project\\AgentCall".to_string()],
            report_path: Some("E:\\Project\\AgentCall\\.agentcall\\reports\\r.md".to_string()),
        };
        let message = json!({
            "id": 7,
            "method": "session/request_permission",
            "params": {
                "tool": "Write",
                "file_path": "E:\\Project\\AgentCall\\src\\lib.rs",
                "options": [
                    {"optionId": "reject-once", "kind": "reject_once"},
                    {"optionId": "allow-once", "kind": "allow_once"}
                ]
            }
        });
        assert_eq!(select_permission_option(&message, Some(&policy)), json!("reject-once"));
    }

    #[test]
    fn permission_policy_allows_read_target_file_request() {
        let policy = AcpPermissionPolicy {
            template: "read-and-report".to_string(),
            target_files: vec!["src/lib.rs".to_string()],
            allowed_paths: vec!["E:\\Project\\AgentCall".to_string()],
            report_path: Some("E:\\Project\\AgentCall\\.agentcall\\reports\\r.md".to_string()),
        };
        let message = json!({
            "id": 700,
            "method": "session/request_permission",
            "params": {
                "sessionId": "sess_fake",
                "tool": "Read",
                "file_path": "src/lib.rs",
                "toolCall": {"toolCallId": "call_1"},
                "options": [
                    {"optionId": "allow-once", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "reject-once", "name": "Reject", "kind": "reject_once"}
                ]
            }
        });
        assert_eq!(select_permission_option(&message, Some(&policy)), json!("allow-once"));
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

    #[test]
    fn acp_env_vars_include_wrapper_session() {
        let vars = acp_env_vars("acp-child-1");
        assert!(vars.iter().any(|(key, value)| {
            *key == "AGENTCALL_WRAPPER_SESSION" && value == "acp-child-1"
        }));
        assert!(vars.iter().any(|(key, value)| {
            *key == "PYTHONIOENCODING" && value == "utf-8"
        }));
    }
}
