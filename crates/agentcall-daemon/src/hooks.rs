use crate::state::{
    AppState, append_agent_event, append_agent_event_locked, read_json_file, write_json_file,
};
use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Deserialize)]
pub(crate) struct HookIngestRequest {
    event: String,
    payload: serde_json::Value,
    runtime: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct EventAppendRequest {
    event_type: String,
    message: Option<String>,
    data: Option<serde_json::Value>,
}

pub(crate) fn file_claims_state(state: &AppState) -> serde_json::Value {
    read_json_file(
        &state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("file_claims.json"),
        serde_json::json!({}),
    )
}

pub(crate) fn unmatched_hooks_state(state: &AppState) -> serde_json::Value {
    read_json_file(
        &state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("unmatched_hooks.json"),
        serde_json::json!([]),
    )
}

pub(crate) fn runtime_bindings_state(state: &AppState) -> serde_json::Value {
    read_json_file(
        &state
            .workspace
            .join(".agentcall")
            .join("state")
            .join("runtime_binding.json"),
        serde_json::json!({}),
    )
}

pub(crate) fn ingest_hook(
    state: &AppState,
    req: HookIngestRequest,
) -> Result<serde_json::Value, String> {
    let _guard = state.state_writer.lock().unwrap();
    let agent_dir = state.workspace.join(".agentcall");
    let state_dir = agent_dir.join("state");
    fs::create_dir_all(&state_dir).map_err(|err| err.to_string())?;

    let mut payload = req.payload;
    let session_id = session_id_from_payload(&payload);
    let unmatched = session_id.is_none();
    let session_id = session_id.unwrap_or_else(|| fallback_session_id(&payload));
    let tool_name = string_field(&payload, &["tool_name", "toolName"]);
    let workspace = string_field(&payload, &["workspace", "cwd"]);
    let transcript_path = string_field(&payload, &["transcript_path"]);
    let status = infer_hook_status(&req.event, &payload);
    let env_wrapper_session = string_field(&payload, &["wrapper_session", "wrapperSession"]);
    let (wrapper_session, binding_source) = upsert_runtime_binding_locked(
        &state_dir,
        env_wrapper_session.as_deref(),
        &session_id,
        transcript_path.as_deref(),
        workspace.as_deref(),
        &req.event,
        &status,
        tool_name.as_deref(),
    )?;
    payload["session_id"] = serde_json::json!(session_id.clone());
    payload["binding_source"] = serde_json::json!(binding_source.clone());
    if let Some(wrapper_session) = &wrapper_session {
        payload["wrapper_session"] = serde_json::json!(wrapper_session);
    }

    if unmatched {
        append_unmatched_hook_locked(&state_dir, &req.event, &session_id, &payload)?;
    }

    let decision = apply_hook_policy_locked(
        state,
        &state_dir,
        &req.event,
        &session_id,
        tool_name.as_deref(),
        &payload,
    )?;

    upsert_active_session_locked(
        &state_dir,
        &session_id,
        serde_json::json!({
            "session_id": session_id,
            "runtime": req.runtime.unwrap_or_else(|| "claude-code-session".to_string()),
            "status": status,
            "agent": string_field(&payload, &["agent", "agent_name"]).unwrap_or_else(|| "claude-code".to_string()),
            "pid": payload.get("pid").cloned().unwrap_or(serde_json::Value::Null),
            "transcript_path": transcript_path,
            "workspace": workspace,
            "wrapper_session": wrapper_session,
            "binding_source": binding_source,
            "updated_at": chrono::Utc::now().to_rfc3339(),
            "last_hook_event": req.event,
            "last_tool": tool_name,
        }),
    )?;

    append_agent_event_locked(
        state,
        &agent_dir,
        &format!("hook.{}", req.event),
        &format!("Claude Code hook received: {}", req.event),
        serde_json::json!({
            "hook_event": req.event,
            "session_id": session_id,
            "status": status,
            "tool_name": tool_name,
            "workspace": workspace,
            "transcript_path": transcript_path,
            "wrapper_session": wrapper_session,
            "binding_source": binding_source,
            "raw": payload,
            "decision": decision,
        }),
    )?;

    Ok(serde_json::json!({
        "event_type": format!("hook.{}", req.event),
        "session_id": session_id,
        "status": status,
        "wrapper_session": wrapper_session,
        "binding_source": binding_source,
        "decision": decision,
        "unmatched": unmatched
    }))
}

pub(crate) fn append_event_request(state: &AppState, req: EventAppendRequest) -> serde_json::Value {
    append_agent_event(
        state,
        &req.event_type,
        req.message.as_deref().unwrap_or(""),
        req.data.unwrap_or_else(|| serde_json::json!({})),
    );
    serde_json::json!({"ok": true})
}

pub(crate) fn append_unmatched_hook_locked(
    state_dir: &Path,
    event: &str,
    session_id: &str,
    payload: &serde_json::Value,
) -> Result<(), String> {
    let path = state_dir.join("unmatched_hooks.json");
    let mut items = read_json_file(&path, serde_json::json!([]));
    if !items.is_array() {
        items = serde_json::json!([]);
    }
    items.as_array_mut().unwrap().push(serde_json::json!({
        "event": event,
        "fallback_session_id": session_id,
        "payload": payload,
        "observed_at": chrono::Utc::now().to_rfc3339(),
    }));
    write_json_file(&path, &items)
}

pub(crate) fn upsert_active_session_locked(
    state_dir: &Path,
    session_id: &str,
    session: serde_json::Value,
) -> Result<(), String> {
    let path = state_dir.join("active_sessions.json");
    let mut sessions = read_json_file(&path, serde_json::json!({}));
    if !sessions.is_object() {
        sessions = serde_json::json!({});
    }
    sessions[session_id] = session;
    write_json_file(&path, &sessions)
}

pub(crate) fn upsert_runtime_binding_locked(
    state_dir: &Path,
    env_wrapper_session: Option<&str>,
    claude_session_id: &str,
    transcript_path: Option<&str>,
    cwd: Option<&str>,
    event: &str,
    status: &str,
    tool_name: Option<&str>,
) -> Result<(Option<String>, String), String> {
    let path = state_dir.join("runtime_binding.json");
    let mut bindings = read_json_file(&path, serde_json::json!({}));
    if !bindings.is_object() {
        bindings = serde_json::json!({});
    }
    let env_wrapper_session = env_wrapper_session.filter(|value| !value.trim().is_empty());
    let wrapper_session = env_wrapper_session
        .map(|value| value.to_string())
        .or_else(|| find_known_wrapper_binding(&bindings, claude_session_id, transcript_path));
    let Some(wrapper_session) = wrapper_session else {
        return Ok((None, "unbound".to_string()));
    };
    let binding_source = if env_wrapper_session.is_some() {
        "env"
    } else {
        "known_session"
    };
    bindings[&wrapper_session] = serde_json::json!({
        "wrapper_session": wrapper_session.clone(),
        "claude_session_id": claude_session_id,
        "transcript_path": transcript_path,
        "cwd": cwd,
        "last_hook_event": event,
        "last_hook_status": status,
        "last_tool": tool_name,
        "last_seen": chrono::Utc::now().to_rfc3339(),
        "binding_source": binding_source,
    });
    write_json_file(&path, &bindings)?;
    Ok((Some(wrapper_session), binding_source.to_string()))
}

pub(crate) fn find_known_wrapper_binding(
    bindings: &serde_json::Value,
    claude_session_id: &str,
    transcript_path: Option<&str>,
) -> Option<String> {
    let object = bindings.as_object()?;
    object.iter().find_map(|(wrapper, binding)| {
        let session_match = binding
            .get("claude_session_id")
            .and_then(|value| value.as_str())
            == Some(claude_session_id);
        let transcript_match = transcript_path.is_some()
            && binding
                .get("transcript_path")
                .and_then(|value| value.as_str())
                == transcript_path;
        if session_match || transcript_match {
            Some(wrapper.clone())
        } else {
            None
        }
    })
}

pub(crate) fn apply_hook_policy_locked(
    state: &AppState,
    state_dir: &Path,
    event: &str,
    session_id: &str,
    tool_name: Option<&str>,
    payload: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    match event {
        "PreToolUse" => pre_tool_use_claim_locked(state, state_dir, session_id, tool_name, payload),
        "PostToolUse" => {
            post_tool_use_observe_locked(state, state_dir, session_id, tool_name, payload)
        }
        "Stop" | "SubagentStop" | "SessionEnd" => {
            release_claims_locked(state, state_dir, session_id)
        }
        _ => Ok(serde_json::Value::Null),
    }
}

pub(crate) fn pre_tool_use_claim_locked(
    state: &AppState,
    state_dir: &Path,
    session_id: &str,
    tool_name: Option<&str>,
    payload: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let tool_name = tool_name.unwrap_or("");
    if !is_write_tool(tool_name) {
        return Ok(
            serde_json::json!({"allowed": true, "reason": "tool does not claim files", "files": [], "conflicts": []}),
        );
    }
    let files = extract_tool_files(payload);
    if files.is_empty() {
        return Ok(
            serde_json::json!({"allowed": true, "reason": "write tool did not expose file path", "files": [], "conflicts": []}),
        );
    }
    let path = state_dir.join("file_claims.json");
    let mut claims = read_json_file(&path, serde_json::json!({}));
    if !claims.is_object() {
        claims = serde_json::json!({});
    }
    mark_expired_claims_stale(&mut claims);
    let mut conflicts = vec![];
    for file in &files {
        let claim = &claims[file];
        if claim.get("status").and_then(|value| value.as_str()) == Some("active")
            && claim.get("session_id").and_then(|value| value.as_str()) != Some(session_id)
        {
            conflicts.push(serde_json::json!({"file": file, "claim": claim}));
        }
    }
    if !conflicts.is_empty() {
        write_json_file(&path, &claims)?;
        return Ok(
            serde_json::json!({"allowed": false, "reason": "file claim conflict", "files": files, "conflicts": conflicts}),
        );
    }
    let now = chrono::Utc::now();
    let expires = now + chrono::Duration::minutes(5);
    for file in &files {
        let old_claimed_at = claims[file]
            .get("claimed_at")
            .cloned()
            .unwrap_or_else(|| serde_json::json!(now.to_rfc3339()));
        claims[file] = serde_json::json!({
            "file": file,
            "session_id": session_id,
            "tool_name": tool_name,
            "last_tool_name": tool_name,
            "status": "active",
            "claimed_at": old_claimed_at,
            "updated_at": now.to_rfc3339(),
            "expires_at": expires.to_rfc3339(),
            "owner_pid": payload.get("pid").cloned().unwrap_or(serde_json::Value::Null),
            "workspace": string_field(payload, &["workspace", "cwd"]).unwrap_or_else(|| state.workspace.display().to_string()),
            "project": state.workspace.file_name().and_then(|name| name.to_str()).unwrap_or("workspace"),
            "transcript_path": string_field(payload, &["transcript_path"]),
        });
    }
    write_json_file(&path, &claims)?;
    Ok(
        serde_json::json!({"allowed": true, "reason": "file claims acquired", "files": files, "conflicts": []}),
    )
}

pub(crate) fn post_tool_use_observe_locked(
    _state: &AppState,
    state_dir: &Path,
    session_id: &str,
    tool_name: Option<&str>,
    payload: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let tool_name = tool_name.unwrap_or("");
    let files = extract_tool_files(payload);
    if files.is_empty() {
        return Ok(
            serde_json::json!({"allowed": true, "reason": "no file paths observed", "files": [], "conflicts": []}),
        );
    }
    if !is_write_tool(tool_name) {
        return Ok(
            serde_json::json!({"allowed": true, "reason": "read observed", "files": files, "conflicts": []}),
        );
    }
    let path = state_dir.join("file_claims.json");
    let mut claims = read_json_file(&path, serde_json::json!({}));
    if !claims.is_object() {
        claims = serde_json::json!({});
    }
    let now = chrono::Utc::now().to_rfc3339();
    for file in &files {
        if claims[file]
            .get("session_id")
            .and_then(|value| value.as_str())
            == Some(session_id)
        {
            claims[file]["updated_at"] = serde_json::json!(now);
            claims[file]["last_tool_name"] = serde_json::json!(tool_name);
        }
    }
    write_json_file(&path, &claims)?;
    Ok(
        serde_json::json!({"allowed": true, "reason": "write observed", "files": files, "conflicts": []}),
    )
}

pub(crate) fn release_claims_locked(
    _state: &AppState,
    state_dir: &Path,
    session_id: &str,
) -> Result<serde_json::Value, String> {
    let path = state_dir.join("file_claims.json");
    let mut claims = read_json_file(&path, serde_json::json!({}));
    if !claims.is_object() {
        claims = serde_json::json!({});
    }
    let mut released = vec![];
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(items) = claims.as_object_mut() {
        for (file, claim) in items.iter_mut() {
            if claim.get("session_id").and_then(|value| value.as_str()) == Some(session_id)
                && claim.get("status").and_then(|value| value.as_str()) == Some("active")
            {
                claim["status"] = serde_json::json!("released");
                claim["released_at"] = serde_json::json!(now);
                released.push(file.clone());
            }
        }
    }
    write_json_file(&path, &claims)?;
    Ok(
        serde_json::json!({"allowed": true, "reason": "session claims released", "files": released, "conflicts": []}),
    )
}

pub(crate) fn mark_expired_claims_stale(claims: &mut serde_json::Value) {
    let now = chrono::Utc::now();
    let Some(items) = claims.as_object_mut() else {
        return;
    };
    for claim in items.values_mut() {
        if claim.get("status").and_then(|value| value.as_str()) != Some("active") {
            continue;
        }
        let Some(expires_at) = claim.get("expires_at").and_then(|value| value.as_str()) else {
            continue;
        };
        let Ok(expires) = chrono::DateTime::parse_from_rfc3339(expires_at) else {
            continue;
        };
        if expires.with_timezone(&chrono::Utc) <= now {
            claim["status"] = serde_json::json!("stale");
            claim["stale_at"] = serde_json::json!(now.to_rfc3339());
        }
    }
}

pub(crate) fn session_id_from_payload(payload: &serde_json::Value) -> Option<String> {
    string_field(payload, &["session_id", "sessionId", "agent_id"])
        .or_else(|| string_field(payload, &["transcript_path"]))
}

pub(crate) fn fallback_session_id(payload: &serde_json::Value) -> String {
    if let Some(path) = string_field(payload, &["transcript_path"]) {
        return format!("transcript:{}", stable_hash(&path));
    }
    format!("unmatched:{}", stable_hash(&payload.to_string()))
}

pub(crate) fn stable_hash(value: &str) -> u64 {
    let mut hash = 1469598103934665603u64;
    for byte in value.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(1099511628211);
    }
    hash
}

pub(crate) fn string_field(payload: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = payload.get(*key).and_then(|value| value.as_str()) {
            return Some(value.to_string());
        }
    }
    None
}

pub(crate) fn extract_tool_files(payload: &serde_json::Value) -> Vec<String> {
    let input = payload
        .get("tool_input")
        .or_else(|| payload.get("toolInput"))
        .and_then(|value| value.as_object());
    let Some(input) = input else {
        return vec![];
    };
    let mut files = vec![];
    for key in ["file_path", "path", "notebook_path"] {
        if let Some(value) = input.get(key).and_then(|value| value.as_str()) {
            files.push(normalize_workspace_path(value));
        }
    }
    files.sort();
    files.dedup();
    files
}

pub(crate) fn normalize_workspace_path(path: &str) -> String {
    let mut normalized = path.replace('\\', "/");
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_string();
    }
    normalized
}

pub(crate) fn is_write_tool(tool_name: &str) -> bool {
    matches!(tool_name, "Edit" | "MultiEdit" | "Write" | "NotebookEdit")
}

pub(crate) fn infer_hook_status(event: &str, payload: &serde_json::Value) -> String {
    if let Some(status) = payload.get("status").and_then(|value| value.as_str()) {
        return status.to_string();
    }
    match event {
        "SessionStart" | "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => "working".to_string(),
        "Notification" => {
            let message = payload
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if message.contains("permission") {
                "needs_permission".to_string()
            } else if message.contains("idle") || message.contains("waiting") {
                "waiting_input".to_string()
            } else {
                "notified".to_string()
            }
        }
        "Stop" => "idle".to_string(),
        "SubagentStop" => "checkpoint_due".to_string(),
        "SessionEnd" => "completed".to_string(),
        _ => "observed".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::now_ms;
    use std::env;
    use std::sync::Arc;

    fn test_state(name: &str) -> Arc<AppState> {
        let root = env::temp_dir().join(format!(
            "agentcall-daemon-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".agentcall").join("state")).unwrap();
        Arc::new(AppState::test(root))
    }

    fn write_payload(session_id: &str, file: &str) -> serde_json::Value {
        serde_json::json!({
            "session_id": session_id,
            "tool_name": "Write",
            "tool_input": {"file_path": file},
            "cwd": "E:\\Project\\AgentCall"
        })
    }

    #[test]
    fn daemon_hook_claims_conflict_on_same_file() {
        let state = test_state("claim-conflict");
        let first = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: write_payload("sess-a", "src/app.py"),
                runtime: None,
            },
        )
        .unwrap();
        assert_eq!(first["decision"]["allowed"], true);

        let second = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: write_payload("sess-b", "src/app.py"),
                runtime: None,
            },
        )
        .unwrap();
        assert_eq!(second["decision"]["allowed"], false);
        assert_eq!(second["decision"]["conflicts"][0]["file"], "src/app.py");
    }

    #[test]
    fn daemon_hook_read_does_not_create_write_claim() {
        let state = test_state("read-observe");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PostToolUse".to_string(),
                payload: serde_json::json!({
                    "session_id": "sess-a",
                    "tool_name": "Read",
                    "tool_input": {"file_path": "src/app.py"}
                }),
                runtime: None,
            },
        )
        .unwrap();
        assert_eq!(result["decision"]["reason"], "read observed");
        let claims = file_claims_state(&state);
        assert!(claims.as_object().unwrap().is_empty());
    }

    #[test]
    fn daemon_hook_missing_session_id_is_unmatched_not_unknown() {
        let state = test_state("unmatched");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: serde_json::json!({
                    "tool_name": "Read",
                    "tool_input": {"file_path": "src/app.py"}
                }),
                runtime: None,
            },
        )
        .unwrap();
        assert_eq!(result["unmatched"], true);
        assert_ne!(result["session_id"], "unknown-session");
        let unmatched = unmatched_hooks_state(&state);
        assert_eq!(unmatched.as_array().unwrap().len(), 1);
    }

    #[test]
    fn hook_status_semantics_keep_stop_benign_and_permission_distinct() {
        assert_eq!(infer_hook_status("Stop", &serde_json::json!({})), "idle");
        assert_eq!(
            infer_hook_status("SubagentStop", &serde_json::json!({})),
            "checkpoint_due"
        );
        assert_eq!(
            infer_hook_status(
                "Notification",
                &serde_json::json!({"message": "Permission required for Bash"})
            ),
            "needs_permission"
        );
        assert_eq!(
            infer_hook_status(
                "Notification",
                &serde_json::json!({"message": "Claude is waiting for input"})
            ),
            "waiting_input"
        );
    }

    #[test]
    fn hook_env_binding_creates_runtime_binding() {
        let state = test_state("env-binding");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: serde_json::json!({
                    "session_id": "claude-a",
                    "wrapper_session": "wrapper-a",
                    "transcript_path": "E:/tmp/a.jsonl",
                    "cwd": "E:/Project/AgentCall",
                    "tool_name": "Read"
                }),
                runtime: None,
            },
        )
        .unwrap();
        assert_eq!(result["wrapper_session"], "wrapper-a");
        assert_eq!(result["binding_source"], "env");
        let bindings = runtime_bindings_state(&state);
        assert_eq!(bindings["wrapper-a"]["claude_session_id"], "claude-a");
        assert_eq!(bindings["wrapper-a"]["binding_source"], "env");
    }

    #[test]
    fn hook_known_session_fallback_only_after_existing_binding() {
        let state = test_state("known-binding");
        ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: serde_json::json!({
                    "session_id": "claude-a",
                    "wrapper_session": "wrapper-a",
                    "transcript_path": "E:/tmp/a.jsonl",
                    "tool_name": "Read"
                }),
                runtime: None,
            },
        )
        .unwrap();
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "Stop".to_string(),
                payload: serde_json::json!({
                    "session_id": "claude-a",
                    "transcript_path": "E:/tmp/a.jsonl"
                }),
                runtime: None,
            },
        )
        .unwrap();
        assert_eq!(result["wrapper_session"], "wrapper-a");
        assert_eq!(result["binding_source"], "known_session");
        let bindings = runtime_bindings_state(&state);
        assert_eq!(bindings["wrapper-a"]["last_hook_status"], "idle");
        assert_eq!(bindings["wrapper-a"]["binding_source"], "known_session");
    }

    #[test]
    fn hook_without_env_or_known_session_stays_unbound() {
        let state = test_state("unbound-binding");
        let result = ingest_hook(
            &state,
            HookIngestRequest {
                event: "PreToolUse".to_string(),
                payload: serde_json::json!({
                    "session_id": "claude-a",
                    "transcript_path": "E:/tmp/a.jsonl",
                    "tool_name": "Read"
                }),
                runtime: None,
            },
        )
        .unwrap();
        assert_eq!(result["binding_source"], "unbound");
        assert!(result.get("wrapper_session").unwrap().is_null());
        let bindings = runtime_bindings_state(&state);
        assert!(bindings.as_object().unwrap().is_empty());
    }
}
