use serde_json::{Value, json};

pub(crate) fn structured_error(code: &str, message: impl Into<String>, details: Value) -> String {
    serde_json::to_string(&structured_error_value(code, message, details))
        .unwrap_or_else(|_| "{\"error\":{\"code\":\"internal_error\"}}".to_string())
}

pub(crate) fn structured_error_value(
    code: &str,
    message: impl Into<String>,
    details: Value,
) -> Value {
    let (category, retryable, hint) = metadata_for_code(code);
    json!({
        "error": {
            "code": code,
            "category": category,
            "message": message.into(),
            "details": details,
            "retryable": retryable,
            "hint": hint
        }
    })
}

pub(crate) fn error_value(message: &str) -> Value {
    if let Ok(value) = serde_json::from_str::<Value>(message) {
        if value.pointer("/error/code").is_some() {
            return value;
        }
    }
    let code = classify_message(message);
    structured_error_value(
        code,
        message.to_string(),
        details_from_message(code, message),
    )
}

pub(crate) fn status_for_error(value: &Value, fallback: u16) -> u16 {
    match value.pointer("/error/code").and_then(Value::as_str) {
        Some("workspace_busy")
        | Some("owner_lease_exists")
        | Some("owner_conflict")
        | Some("owner_mismatch")
        | Some("stale_lease")
        | Some("stale_lease_generation")
        | Some("expired_lease") => 409,
        Some("capacity_exceeded") => 429,
        Some("missing_control_token")
        | Some("invalid_control_token")
        | Some("missing_precondition") => 428,
        Some("authentication_required") => 401,
        Some("forbidden") => 403,
        _ => fallback,
    }
}

fn classify_message(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if lower.starts_with("workspace_busy:") {
        "workspace_busy"
    } else if lower.starts_with("rejected_existing_owner_lease:") {
        "owner_lease_exists"
    } else if lower.starts_with("rejected_owner_conflict:") {
        "owner_conflict"
    } else if lower.starts_with("rejected_owner_mismatch:") {
        "owner_mismatch"
    } else if lower.starts_with("rejected_stale_lease_generation:") {
        "stale_lease_generation"
    } else if lower.starts_with("rejected_stale_lease:") {
        "stale_lease"
    } else if lower.starts_with("rejected_expired_lease:") {
        "expired_lease"
    } else if lower.starts_with("capacity_exceeded:") {
        "capacity_exceeded"
    } else if lower.contains("missing control token") || lower.contains("control_token_required") {
        "missing_control_token"
    } else if lower.contains("invalid control token") {
        "invalid_control_token"
    } else if lower.contains("daemon token is required")
        || lower.contains("invalid or missing daemon token")
    {
        "authentication_required"
    } else if lower.contains("host not allowed") || lower.contains("origin not allowed") {
        "forbidden"
    } else {
        "bad_request"
    }
}

fn details_from_message(code: &str, message: &str) -> Value {
    match code {
        "workspace_busy" => parse_workspace_busy(message),
        "owner_lease_exists" => parse_key_values(message),
        "capacity_exceeded" => parse_key_values(message),
        _ => json!({}),
    }
}

fn parse_workspace_busy(message: &str) -> Value {
    let fields = parse_key_values(message);
    json!({
        "workspace": fields.get("workspace").cloned().unwrap_or(Value::Null),
        "existing_session": fields.get("existing_session").cloned().unwrap_or(Value::Null),
        "existing_mode": fields.get("existing_mode").cloned().unwrap_or(Value::Null),
        "suggested_action": "Inspect the existing session, accept its report or stop it if stale; report-only workers should use report_path/write_paths that do not claim implementation ownership."
    })
}

fn parse_key_values(message: &str) -> Value {
    let (_, rest) = message.split_once(':').unwrap_or(("", message));
    let mut object = serde_json::Map::new();
    for part in rest.split_whitespace() {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        object.insert(
            key.trim_matches(|ch: char| ch == ',' || ch == ';')
                .to_string(),
            json!(value.trim_matches(|ch: char| ch == ',' || ch == ';')),
        );
    }
    Value::Object(object)
}

fn metadata_for_code(code: &str) -> (&'static str, bool, &'static str) {
    match code {
        "workspace_busy" => (
            "safety_lock",
            false,
            "Another active worker owns an incompatible workspace lease.",
        ),
        "owner_lease_exists" => (
            "safety_lock",
            false,
            "The session already has an active owner lease; use the existing control token or stop/release the session.",
        ),
        "owner_conflict" | "owner_mismatch" => (
            "safety_lock",
            false,
            "The session belongs to another owner.",
        ),
        "stale_lease" | "stale_lease_generation" | "expired_lease" => (
            "safety_lock",
            false,
            "Refresh board/session summary and retry with the daemon-minted control token.",
        ),
        "capacity_exceeded" => (
            "safety_lock",
            true,
            "Active worker capacity is full; wait for a worker to finish or stop an obsolete session.",
        ),
        "missing_control_token" | "invalid_control_token" | "missing_precondition" => (
            "safety_lock",
            false,
            "Use the control token and precondition returned by agentcall_session summary.",
        ),
        "authentication_required" => (
            "auth",
            false,
            "Configure daemon_token for both daemon and MCP bridge, or explicitly enable dev_open_loopback locally.",
        ),
        "forbidden" => (
            "auth",
            false,
            "Only loopback Host/Origin requests are accepted by the daemon API.",
        ),
        _ => ("validation", false, "Fix the request payload and retry."),
    }
}
