use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ErrorCode {
    WorkspaceBusy,
    OwnerLeaseExists,
    OwnerConflict,
    OwnerMismatch,
    OwnerUnbound,
    StaleLease,
    StaleLeaseGeneration,
    ExpiredLease,
    CapacityExceeded,
    CodingRequiresWorktree,
    MissingControlToken,
    InvalidControlToken,
    MissingPrecondition,
    ControlUnavailable,
    StaleControlToken,
    ActionNotAllowed,
    AuthenticationRequired,
    Forbidden,
    BadRequest,
    InternalError,
}

impl ErrorCode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::WorkspaceBusy => "workspace_busy",
            Self::OwnerLeaseExists => "owner_lease_exists",
            Self::OwnerConflict => "owner_conflict",
            Self::OwnerMismatch => "owner_mismatch",
            Self::OwnerUnbound => "owner_unbound",
            Self::StaleLease => "stale_lease",
            Self::StaleLeaseGeneration => "stale_lease_generation",
            Self::ExpiredLease => "expired_lease",
            Self::CapacityExceeded => "capacity_exceeded",
            Self::CodingRequiresWorktree => "coding_requires_worktree",
            Self::MissingControlToken => "missing_control_token",
            Self::InvalidControlToken => "invalid_control_token",
            Self::MissingPrecondition => "missing_precondition",
            Self::ControlUnavailable => "control_unavailable",
            Self::StaleControlToken => "stale_control_token",
            Self::ActionNotAllowed => "action_not_allowed",
            Self::AuthenticationRequired => "authentication_required",
            Self::Forbidden => "forbidden",
            Self::BadRequest => "bad_request",
            Self::InternalError => "internal_error",
        }
    }

    pub(crate) fn from_str(value: &str) -> Option<Self> {
        Some(match value {
            "workspace_busy" => Self::WorkspaceBusy,
            "owner_lease_exists" => Self::OwnerLeaseExists,
            "owner_conflict" => Self::OwnerConflict,
            "owner_mismatch" => Self::OwnerMismatch,
            "owner_unbound" => Self::OwnerUnbound,
            "stale_lease" => Self::StaleLease,
            "stale_lease_generation" => Self::StaleLeaseGeneration,
            "expired_lease" => Self::ExpiredLease,
            "capacity_exceeded" => Self::CapacityExceeded,
            "coding_requires_worktree" => Self::CodingRequiresWorktree,
            "missing_control_token" => Self::MissingControlToken,
            "invalid_control_token" => Self::InvalidControlToken,
            "missing_precondition" => Self::MissingPrecondition,
            "control_unavailable" => Self::ControlUnavailable,
            "stale_control_token" => Self::StaleControlToken,
            "action_not_allowed" => Self::ActionNotAllowed,
            "authentication_required" => Self::AuthenticationRequired,
            "forbidden" => Self::Forbidden,
            "bad_request" => Self::BadRequest,
            "internal_error" => Self::InternalError,
            _ => return None,
        })
    }

    fn http_status(self, fallback: u16) -> u16 {
        match self {
            Self::WorkspaceBusy
            | Self::OwnerLeaseExists
            | Self::OwnerConflict
            | Self::OwnerMismatch
            | Self::OwnerUnbound
            | Self::StaleLease
            | Self::StaleLeaseGeneration
            | Self::ExpiredLease => 409,
            Self::CapacityExceeded => 429,
            Self::CodingRequiresWorktree => 409,
            Self::MissingControlToken
            | Self::InvalidControlToken
            | Self::MissingPrecondition
            | Self::ControlUnavailable
            | Self::StaleControlToken
            | Self::ActionNotAllowed => 428,
            Self::AuthenticationRequired => 401,
            Self::Forbidden => 403,
            Self::InternalError => 500,
            Self::BadRequest => fallback,
        }
    }

    fn metadata(self) -> (&'static str, bool, &'static str) {
        match self {
            Self::WorkspaceBusy => (
                "safety_lock",
                false,
                "Another active worker owns an incompatible workspace lease.",
            ),
            Self::OwnerLeaseExists => (
                "safety_lock",
                false,
                "The session already has an active owner lease; use the existing control token or stop/release the session.",
            ),
            Self::OwnerConflict | Self::OwnerMismatch => (
                "safety_lock",
                false,
                "The session belongs to another owner.",
            ),
            Self::OwnerUnbound => (
                "safety_lock",
                false,
                "Bind the caller owner before requesting control or owner-scoped state.",
            ),
            Self::StaleLease | Self::StaleLeaseGeneration | Self::ExpiredLease => (
                "safety_lock",
                false,
                "Refresh board/session summary and retry with the daemon-minted control token.",
            ),
            Self::CapacityExceeded => (
                "safety_lock",
                true,
                "Active worker capacity is full; wait for a worker to finish or stop an obsolete session.",
            ),
            Self::CodingRequiresWorktree => (
                "safety_lock",
                false,
                "Start coding workers in a dedicated git worktree branch and merge through a PR-style review report.",
            ),
            Self::MissingControlToken | Self::InvalidControlToken | Self::MissingPrecondition => (
                "safety_lock",
                false,
                "Use the control token and precondition returned by agentcall_session summary.",
            ),
            Self::ControlUnavailable => (
                "safety_lock",
                false,
                "Control authority is unavailable for this session; refresh session summary or restart via route.",
            ),
            Self::StaleControlToken => (
                "safety_lock",
                false,
                "Refresh session summary and retry with a fresh daemon-minted control token.",
            ),
            Self::ActionNotAllowed => (
                "safety_lock",
                false,
                "The current session projection does not allow that control action.",
            ),
            Self::AuthenticationRequired => (
                "auth",
                false,
                "Configure daemon_token for both daemon and MCP bridge, or explicitly enable dev_open_loopback locally.",
            ),
            Self::Forbidden => (
                "auth",
                false,
                "Only loopback Host/Origin requests are accepted by the daemon API.",
            ),
            Self::InternalError => (
                "internal",
                false,
                "Inspect daemon logs; this should not be retried blindly.",
            ),
            Self::BadRequest => ("validation", false, "Fix the request payload and retry."),
        }
    }
}

pub(crate) fn structured_error(
    code: ErrorCode,
    message: impl Into<String>,
    details: Value,
) -> String {
    serde_json::to_string(&structured_error_value(code, message, details))
        .unwrap_or_else(|_| "{\"error\":{\"code\":\"internal_error\"}}".to_string())
}

pub(crate) fn structured_error_value(
    code: ErrorCode,
    message: impl Into<String>,
    details: Value,
) -> Value {
    let (category, retryable, hint) = code.metadata();
    json!({
        "error": {
            "code": code.as_str(),
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
    value
        .pointer("/error/code")
        .and_then(Value::as_str)
        .and_then(ErrorCode::from_str)
        .map(|code| code.http_status(fallback))
        .unwrap_or(fallback)
}

fn classify_message(message: &str) -> ErrorCode {
    let lower = message.to_ascii_lowercase();
    if lower.starts_with("workspace_busy:") {
        ErrorCode::WorkspaceBusy
    } else if lower.starts_with("rejected_existing_owner_lease:") {
        ErrorCode::OwnerLeaseExists
    } else if lower.starts_with("rejected_owner_conflict:") {
        ErrorCode::OwnerConflict
    } else if lower.starts_with("rejected_owner_mismatch:") {
        ErrorCode::OwnerMismatch
    } else if lower.starts_with("rejected_stale_lease_generation:") {
        ErrorCode::StaleLeaseGeneration
    } else if lower.starts_with("rejected_stale_lease:") {
        ErrorCode::StaleLease
    } else if lower.starts_with("rejected_expired_lease:") {
        ErrorCode::ExpiredLease
    } else if lower.starts_with("capacity_exceeded:") {
        ErrorCode::CapacityExceeded
    } else if lower.starts_with("coding_requires_worktree:") {
        ErrorCode::CodingRequiresWorktree
    } else if lower.contains("missing control token") || lower.contains("control_token_required") {
        ErrorCode::MissingControlToken
    } else if lower.contains("invalid control token") {
        ErrorCode::InvalidControlToken
    } else if lower.contains("daemon token is required")
        || lower.contains("invalid or missing daemon token")
    {
        ErrorCode::AuthenticationRequired
    } else if lower.contains("host not allowed") || lower.contains("origin not allowed") {
        ErrorCode::Forbidden
    } else {
        ErrorCode::BadRequest
    }
}

fn details_from_message(code: ErrorCode, message: &str) -> Value {
    match code {
        ErrorCode::WorkspaceBusy => parse_workspace_busy(message),
        ErrorCode::OwnerLeaseExists => parse_key_values(message),
        ErrorCode::CapacityExceeded | ErrorCode::CodingRequiresWorktree => {
            parse_key_values(message)
        }
        _ => json!({}),
    }
}

fn parse_workspace_busy(message: &str) -> Value {
    let fields = parse_key_values(message);
    json!({
        "workspace": fields.get("workspace").cloned().unwrap_or(Value::Null),
        "existing_session": fields.get("existing_session").cloned().unwrap_or(Value::Null),
        "existing_mode": fields.get("existing_mode").cloned().unwrap_or(Value::Null),
        "suggested_action": "For parallel Code work, create a separate temporary workspace or git worktree for the new worker and route with workspace pointing at that shard. Do not start two exclusive Code workers in the same worktree. Otherwise inspect the existing session, accept its report, or stop it if stale."
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
