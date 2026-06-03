use serde_json::{json, Value};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const WRITE_TOOLS: &[&str] = &["Edit", "MultiEdit", "Write", "NotebookEdit"];

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = Args::from_env()?;
    let mut stdin = String::new();
    io::stdin()
        .read_to_string(&mut stdin)
        .map_err(|err| format!("failed to read stdin: {err}"))?;
    let payload = parse_payload(&stdin)?;
    let event = args
        .event
        .clone()
        .or_else(|| hook_event_name(&payload).map(str::to_string))
        .ok_or("missing --event and hook payload event name")?;

    let mut store = Store::new(args.root);
    let result = ingest(&mut store, &args.runtime, &event, &payload)?;
    let output = hook_output(&event, &result);
    if let Some(output) = output {
        println!("{}", serde_json::to_string(&output).unwrap());
    }
    Ok(())
}

#[derive(Debug)]
struct Args {
    root: PathBuf,
    event: Option<String>,
    runtime: String,
}

impl Args {
    fn from_env() -> Result<Self, String> {
        let mut root = env::current_dir().map_err(|err| err.to_string())?;
        let mut event = None;
        let mut runtime = "codex".to_string();
        let mut iter = env::args().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--root" => root = PathBuf::from(iter.next().ok_or("missing --root value")?),
                "--event" => event = Some(iter.next().ok_or("missing --event value")?),
                "--runtime" => runtime = iter.next().ok_or("missing --runtime value")?,
                "--help" | "-h" => {
                    return Err("usage: agentcall-hook --root PATH [--event EVENT] [--runtime codex|claude-code]".to_string());
                }
                other => return Err(format!("unknown argument: {other}")),
            }
        }
        Ok(Self { root, event, runtime })
    }
}

#[derive(Debug)]
struct Store {
    root: PathBuf,
    agent_dir: PathBuf,
    state_dir: PathBuf,
    events_path: PathBuf,
}

impl Store {
    fn new(root: PathBuf) -> Self {
        let root = fs::canonicalize(&root).unwrap_or(root);
        let agent_dir = root.join(".agentcall");
        let state_dir = agent_dir.join("state");
        let events_path = agent_dir.join("events.ndjson");
        Self {
            root,
            agent_dir,
            state_dir,
            events_path,
        }
    }

    fn init(&self) -> Result<(), String> {
        fs::create_dir_all(self.agent_dir.join("tasks")).map_err(|err| err.to_string())?;
        fs::create_dir_all(self.agent_dir.join("workers")).map_err(|err| err.to_string())?;
        fs::create_dir_all(&self.state_dir).map_err(|err| err.to_string())?;
        if !self.events_path.exists() {
            fs::write(&self.events_path, "").map_err(|err| err.to_string())?;
        }
        for (name, value) in [
            ("project.json", json!({"version": 1, "decisions": [], "risks": [], "memory": []})),
            ("file_claims.json", json!({})),
            ("active_sessions.json", json!({})),
            ("context_index.json", json!({"calls": []})),
            ("transcripts.json", json!({})),
        ] {
            let path = self.state_dir.join(name);
            if !path.exists() {
                self.write_json(name, &value)?;
            }
        }
        Ok(())
    }

    fn read_json(&self, name: &str, default: Value) -> Result<Value, String> {
        self.init()?;
        let path = self.state_dir.join(name);
        if !path.exists() {
            return Ok(default);
        }
        let text = fs::read_to_string(path).map_err(|err| err.to_string())?;
        serde_json::from_str(&text).map_err(|err| format!("invalid {name}: {err}"))
    }

    fn write_json(&self, name: &str, value: &Value) -> Result<(), String> {
        fs::create_dir_all(&self.state_dir).map_err(|err| err.to_string())?;
        let text = serde_json::to_string_pretty(value).unwrap() + "\n";
        fs::write(self.state_dir.join(name), text).map_err(|err| err.to_string())
    }

    fn append_event(&self, event_type: &str, message: &str, data: Value) -> Result<(), String> {
        self.init()?;
        let id = format!("evt-{:06}", self.next_event_number()?);
        let event = json!({
            "id": id,
            "ts": now_stamp(),
            "type": event_type,
            "task_id": Value::Null,
            "run_id": Value::Null,
            "message": message,
            "data": data,
        });
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events_path)
            .map_err(|err| err.to_string())?;
        writeln!(file, "{}", serde_json::to_string(&event).unwrap()).map_err(|err| err.to_string())
    }

    fn next_event_number(&self) -> Result<usize, String> {
        if !self.events_path.exists() {
            return Ok(1);
        }
        let text = fs::read_to_string(&self.events_path).map_err(|err| err.to_string())?;
        Ok(text.lines().filter(|line| !line.trim().is_empty()).count() + 1)
    }
}

#[derive(Debug)]
struct IngestResult {
    session_id: String,
    status: String,
    context_injection: String,
    decision: Option<Value>,
}

fn ingest(store: &mut Store, runtime: &str, event: &str, payload: &Value) -> Result<IngestResult, String> {
    store.init()?;
    let session_id = session_id_from_payload(payload);
    let status = infer_status(event, payload);
    let decision = apply_policy(store, event, payload)?;

    let session = json!({
        "session_id": session_id,
        "runtime": runtime,
        "status": status,
        "agent": payload_string(payload, &["agent", "agent_name"]).unwrap_or_else(|| runtime.to_string()),
        "pid": payload.get("pid").cloned().unwrap_or(Value::Null),
        "transcript_path": payload_string(payload, &["transcript_path"]),
        "workspace": payload_string(payload, &["workspace", "cwd", "project"]),
        "updated_at": now_stamp(),
        "last_hook_event": event,
    });
    let mut sessions = store.read_json("active_sessions.json", json!({}))?;
    sessions[&session_id] = session;
    store.write_json("active_sessions.json", &sessions)?;

    store.append_event(
        &format!("hook.{event}"),
        &format!("{runtime} hook received: {event}"),
        json!({
            "hook_event": event,
            "session_id": session_id,
            "runtime": runtime,
            "status": status,
            "decision": decision,
            "raw": payload,
        }),
    )?;

    let context_injection = if matches!(event, "SessionStart" | "UserPromptSubmit") {
        context_injection(store, runtime)?
    } else {
        String::new()
    };

    Ok(IngestResult {
        session_id,
        status,
        context_injection,
        decision,
    })
}

fn apply_policy(store: &Store, event: &str, payload: &Value) -> Result<Option<Value>, String> {
    match event {
        "PreToolUse" => Ok(Some(evaluate_pre_tool_use(store, payload)?)),
        "PostToolUse" => Ok(Some(observe_post_tool_use(store, payload)?)),
        "Stop" | "SubagentStop" | "SessionEnd" => Ok(Some(release_session(store, &session_id_from_payload(payload))?)),
        _ => Ok(None),
    }
}

fn evaluate_pre_tool_use(store: &Store, payload: &Value) -> Result<Value, String> {
    let tool_name = payload_string(payload, &["tool_name", "toolName"]).unwrap_or_default();
    if !WRITE_TOOLS.contains(&tool_name.as_str()) {
        return Ok(json!({"allowed": true, "reason": "tool does not claim files", "files": [], "conflicts": []}));
    }
    let files = extract_tool_files(payload);
    if files.is_empty() {
        return Ok(json!({"allowed": true, "reason": "write tool did not expose file path", "files": [], "conflicts": []}));
    }
    let session_id = session_id_from_payload(payload);
    let mut claims = store.read_json("file_claims.json", json!({}))?;
    let mut conflicts = Vec::new();
    for file in &files {
        if let Some(claim) = claims.get(file) {
            if claim.get("session_id").and_then(Value::as_str) != Some(session_id.as_str())
                && claim.get("status").and_then(Value::as_str) == Some("active")
            {
                conflicts.push(json!({"file": file, "claim": claim}));
            }
        }
    }
    if !conflicts.is_empty() {
        return Ok(json!({"allowed": false, "reason": "file claim conflict", "files": files, "conflicts": conflicts}));
    }
    for file in &files {
        let claimed_at = claims
            .get(file)
            .and_then(|claim| claim.get("claimed_at"))
            .cloned()
            .unwrap_or_else(|| json!(now_stamp()));
        claims[file] = json!({
            "file": file,
            "session_id": session_id,
            "tool_name": tool_name,
            "status": "active",
            "claimed_at": claimed_at,
            "updated_at": now_stamp(),
        });
    }
    store.write_json("file_claims.json", &claims)?;
    store.append_event(
        "file_claim.acquired",
        &format!("{session_id} claimed {}.", files.join(", ")),
        json!({"session_id": session_id, "files": files, "tool_name": tool_name}),
    )?;
    Ok(json!({"allowed": true, "reason": "file claims acquired", "files": files, "conflicts": []}))
}

fn observe_post_tool_use(store: &Store, payload: &Value) -> Result<Value, String> {
    let tool_name = payload_string(payload, &["tool_name", "toolName"]).unwrap_or_default();
    let files = extract_tool_files(payload);
    if files.is_empty() {
        return Ok(json!({"allowed": true, "reason": "no file paths observed", "files": [], "conflicts": []}));
    }
    let session_id = session_id_from_payload(payload);
    let mut claims = store.read_json("file_claims.json", json!({}))?;
    for file in &files {
        if claims
            .get(file)
            .and_then(|claim| claim.get("session_id"))
            .and_then(Value::as_str)
            == Some(session_id.as_str())
        {
            claims[file]["updated_at"] = json!(now_stamp());
            claims[file]["last_tool_name"] = json!(tool_name);
        }
    }
    store.write_json("file_claims.json", &claims)?;
    store.append_event(
        "file_claim.observed_write",
        &format!("{session_id} wrote {}.", files.join(", ")),
        json!({"session_id": session_id, "files": files, "tool_name": tool_name}),
    )?;
    Ok(json!({"allowed": true, "reason": "write observed", "files": files, "conflicts": []}))
}

fn release_session(store: &Store, session_id: &str) -> Result<Value, String> {
    let mut claims = store.read_json("file_claims.json", json!({}))?;
    let mut released = Vec::new();
    if let Some(object) = claims.as_object_mut() {
        for (file, claim) in object.iter_mut() {
            if claim.get("session_id").and_then(Value::as_str) == Some(session_id)
                && claim.get("status").and_then(Value::as_str) == Some("active")
            {
                claim["status"] = json!("released");
                claim["released_at"] = json!(now_stamp());
                released.push(file.clone());
            }
        }
    }
    store.write_json("file_claims.json", &claims)?;
    if !released.is_empty() {
        store.append_event(
            "file_claim.released",
            &format!("{session_id} released {}.", released.join(", ")),
            json!({"session_id": session_id, "files": released}),
        )?;
    }
    Ok(json!({"allowed": true, "reason": "session claims released", "files": released, "conflicts": []}))
}

fn context_injection(store: &Store, runtime: &str) -> Result<String, String> {
    let sessions = store.read_json("active_sessions.json", json!({}))?;
    let claims = store.read_json("file_claims.json", json!({}))?;
    let reports = count_reports(&store.root);
    let active_sessions = sessions.as_object().map(|object| object.len()).unwrap_or(0);
    let active_claims = claims
        .as_object()
        .map(|object| {
            object
                .values()
                .filter(|claim| claim.get("status").and_then(Value::as_str) == Some("active"))
                .count()
        })
        .unwrap_or(0);
    Ok(format!(
        "# AgentCall Context\n\n- runtime: {runtime}\n- workspace: {}\n- active_sessions: {active_sessions}\n- active_file_claims: {active_claims}\n- structured_reports: {reports}\n\nAgentCall discipline:\n- Before delegating, call `agentcall_codex_preflight` or inspect the board.\n- Use ACP for bounded child calls and PTY only for handoff/debug sessions.\n- Require a child report at lifecycle end. Write review only for drift, blockers, or revision.\n- Keep SOP in tools/code where possible; do not make the child infer hidden project state.\n",
        store.root.display()
    ))
}

fn hook_output(event: &str, result: &IngestResult) -> Option<Value> {
    if matches!(event, "SessionStart" | "UserPromptSubmit") {
        return Some(json!({
            "systemMessage": format!("AgentCall {}: status={}", result.session_id, result.status),
            "hookSpecificOutput": {
                "hookEventName": event,
                "additionalContext": result.context_injection,
            }
        }));
    }
    if event == "PreToolUse" {
        if let Some(decision) = &result.decision {
            if decision.get("allowed").and_then(Value::as_bool) == Some(false) {
                return Some(json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": decision.get("reason").and_then(Value::as_str).unwrap_or("AgentCall file claim conflict"),
                    }
                }));
            }
        }
    }
    None
}

fn parse_payload(stdin: &str) -> Result<Value, String> {
    if stdin.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(stdin).map_err(|err| format!("invalid hook JSON: {err}"))
}

fn hook_event_name(payload: &Value) -> Option<&str> {
    payload
        .get("hook_event_name")
        .or_else(|| payload.get("hookEventName"))
        .and_then(Value::as_str)
}

fn payload_string(payload: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = payload.get(*key).and_then(Value::as_str) {
            return Some(value.to_string());
        }
    }
    None
}

fn session_id_from_payload(payload: &Value) -> String {
    payload_string(
        payload,
        &["session_id", "sessionId", "agent_id", "conversation_id", "transcript_path"],
    )
    .unwrap_or_else(|| "unknown-session".to_string())
}

fn infer_status(event: &str, payload: &Value) -> String {
    if let Some(status) = payload.get("status").and_then(Value::as_str) {
        return status.to_string();
    }
    match event {
        "SessionStart" | "UserPromptSubmit" | "PreToolUse" => "running",
        "Notification" => {
            let message = payload.get("message").and_then(Value::as_str).unwrap_or("").to_lowercase();
            if message.contains("permission") {
                "needs_permission"
            } else if message.contains("idle") {
                "idle"
            } else {
                "notified"
            }
        }
        "Stop" | "SubagentStop" => "checkpoint_due",
        "SessionEnd" => "ended",
        "PreCompact" => "compacting",
        "PostCompact" => "resumed",
        _ => "observed",
    }
    .to_string()
}

fn extract_tool_files(payload: &Value) -> Vec<String> {
    let Some(input) = payload
        .get("tool_input")
        .or_else(|| payload.get("toolInput"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    let mut files = Vec::new();
    for key in ["file_path", "path", "notebook_path"] {
        if let Some(path) = input.get(key).and_then(Value::as_str) {
            files.push(normalize_workspace_path(path));
        }
    }
    files.sort();
    files.dedup();
    files
}

fn normalize_workspace_path(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_string();
    }
    normalized
}

fn count_reports(root: &Path) -> usize {
    let tasks = root.join(".agentcall").join("tasks");
    let Ok(task_dirs) = fs::read_dir(tasks) else {
        return 0;
    };
    let mut count = 0;
    for entry in task_dirs.flatten() {
        let reports = entry.path().join("reports");
        if let Ok(files) = fs::read_dir(reports) {
            count += files
                .flatten()
                .filter(|file| file.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
                .count();
        }
    }
    count
}

fn now_stamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("unix:{seconds}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_write_files() {
        let payload = json!({
            "tool_name": "Edit",
            "tool_input": {"file_path": ".\\src\\main.rs"}
        });
        assert_eq!(extract_tool_files(&payload), vec!["src/main.rs"]);
    }

    #[test]
    fn emits_codex_context_output() {
        let result = IngestResult {
            session_id: "s1".to_string(),
            status: "running".to_string(),
            context_injection: "# AgentCall Context".to_string(),
            decision: None,
        };
        let output = hook_output("UserPromptSubmit", &result).unwrap();
        assert_eq!(
            output["hookSpecificOutput"]["hookEventName"],
            json!("UserPromptSubmit")
        );
    }
}
