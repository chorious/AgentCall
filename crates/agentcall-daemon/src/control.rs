use crate::commands::{CommandEnvelopeV1, CommandType};
use crate::crypto::sha256_hex;
use crate::errors::ErrorCode;
use crate::ownership::{owner_lease_is_active, validate_owner_lease_precondition};
use crate::projection::{SessionProjectionV1, read_session_projection};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
#[cfg(unix)]
use std::io::Read;

const CONTROL_TOKEN_TTL_SECONDS: i64 = 60;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ControlTokenClaims {
    pub(crate) session_id: String,
    pub(crate) owner_id: String,
    pub(crate) owner_lease_id: String,
    pub(crate) lease_generation: u64,
    pub(crate) control_epoch: u64,
    pub(crate) allowed_actions: Vec<String>,
    pub(crate) issued_at: String,
    pub(crate) expires_at: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ValidatedControlToken {
    pub(crate) token_hash: String,
    pub(crate) claims: ControlTokenClaims,
}

#[derive(Clone, Debug)]
pub(crate) struct ControlError {
    pub(crate) status: ErrorCode,
    pub(crate) reason: String,
    pub(crate) next_step: &'static str,
    pub(crate) current: Value,
}

impl ControlError {
    pub(crate) fn to_value(&self) -> Value {
        json!({
            "ok": false,
            "status": self.status.as_str(),
            "error": {
                "code": self.status.as_str()
            },
            "reason": self.reason,
            "next_step": self.next_step,
            "current": self.current
        })
    }
}

pub(crate) fn destructive_action_requires_control(action: &str) -> bool {
    matches!(
        action,
        "stop" | "kill" | "interrupt" | "approve_plan" | "start_auto"
    )
}

pub(crate) fn control_summary_for_session(
    state: &AppState,
    session_id: &str,
    owner_id: Option<&str>,
) -> Value {
    let Some(owner_id) = owner_id else {
        return json!({
            "available": state.sessions.lock().unwrap().contains_key(session_id),
            "token_included": false,
            "token_required_for": ["interrupt", "stop", "kill", "approve_plan", "start_auto"],
            "reason": "control token is minted only when caller explicitly requests control with a bound owner"
        });
    };
    match mint_control_token(state, session_id, owner_id) {
        Ok((token, claims)) => json!({
            "available": true,
            "token": token,
            "token_included": true,
            "expires_at": claims.expires_at,
            "ttl_seconds": CONTROL_TOKEN_TTL_SECONDS,
            "control_epoch": claims.control_epoch,
            "owner": {
                "owner_id": claims.owner_id,
                "owner_lease_id": claims.owner_lease_id,
                "lease_generation": claims.lease_generation
            },
            "allowed_actions": claims.allowed_actions
        }),
        Err(err) => {
            let mut value = err.to_value();
            value["available"] = json!(false);
            value
        }
    }
}

pub(crate) fn mint_control_token(
    state: &AppState,
    session_id: &str,
    owner_id: &str,
) -> Result<(String, ControlTokenClaims), ControlError> {
    cleanup_expired_control_tokens(state);
    let live = state.sessions.lock().unwrap().contains_key(session_id);
    if !live {
        return Err(control_error(
            ErrorCode::ControlUnavailable,
            format!("session {session_id} is not live"),
            "inspect_session_summary",
            json!({"session_id": session_id, "live": false}),
        ));
    }
    let projection = read_session_projection(state, session_id)
        .unwrap_or_else(|| missing_projection(session_id));
    if projection.terminal {
        return Err(control_error(
            ErrorCode::ControlUnavailable,
            "terminal sessions cannot mint control tokens".to_string(),
            "inspect_report_or_cleanup",
            projection_current(&projection),
        ));
    }
    let now = chrono::Utc::now();
    let lease = {
        let leases = state.owner_leases.lock().unwrap();
        let Some(lease) = leases.get(session_id) else {
            return Err(control_error(
                ErrorCode::ControlUnavailable,
                "session has no active owner lease; refusing to mint hidden control authority"
                    .to_string(),
                "restart_or_reacquire_session_via_route",
                json!({"session_id": session_id, "live": true, "owner_lease": "missing"}),
            ));
        };
        if lease.owner_id != owner_id {
            return Err(control_error(
                ErrorCode::OwnerMismatch,
                format!(
                    "session owner is {}, but caller requested {}",
                    lease.owner_id, owner_id
                ),
                "inspect_board_scope_or_use_correct_owner",
                json!({
                    "session_id": session_id,
                    "owner_id": lease.owner_id,
                    "requested_owner_id": owner_id
                }),
            ));
        }
        if !owner_lease_is_active(lease, now) {
            return Err(control_error(
                ErrorCode::StaleControlToken,
                "session owner lease is not active".to_string(),
                "refresh_session_summary",
                json!({
                    "session_id": session_id,
                    "owner_id": lease.owner_id,
                    "owner_lease_id": lease.lease_id,
                    "lease_generation": lease.lease_generation,
                    "lease_status": format!("{:?}", lease.status)
                }),
            ));
        }
        lease.clone()
    };

    let token = random_control_token().map_err(|err| {
        control_error(
            ErrorCode::ControlUnavailable,
            format!("failed to generate control token: {err}"),
            "retry_or_restart_daemon",
            json!({"session_id": session_id}),
        )
    })?;
    let token_hash = control_token_hash(&token);
    let expires_at = now + chrono::Duration::seconds(CONTROL_TOKEN_TTL_SECONDS);
    let claims = ControlTokenClaims {
        session_id: session_id.to_string(),
        owner_id: lease.owner_id.clone(),
        owner_lease_id: lease.lease_id.clone(),
        lease_generation: lease.lease_generation,
        control_epoch: projection.control_epoch,
        allowed_actions: allowed_actions_for_projection(&projection),
        issued_at: now.to_rfc3339(),
        expires_at: expires_at.to_rfc3339(),
    };
    state
        .control_tokens
        .lock()
        .unwrap()
        .insert(token_hash, claims.clone());
    Ok((token, claims))
}

pub(crate) fn validate_control_token(
    state: &AppState,
    session_id: &str,
    action: &str,
    token: &str,
) -> Result<ValidatedControlToken, ControlError> {
    cleanup_expired_control_tokens(state);
    let token_hash = control_token_hash(token);
    let claims = {
        let tokens = state.control_tokens.lock().unwrap();
        let Some(claims) = tokens.get(&token_hash) else {
            return Err(control_error(
                ErrorCode::StaleControlToken,
                "control token is missing or expired".to_string(),
                "refresh_session_summary",
                json!({"session_id": session_id, "action": action}),
            ));
        };
        claims.clone()
    };
    validate_control_claims(state, session_id, action, &claims)?;
    Ok(ValidatedControlToken { token_hash, claims })
}

pub(crate) fn validate_envelope_control_at_actor(
    state: &AppState,
    session_id: &str,
    command: &CommandEnvelopeV1,
) -> Result<(), ControlError> {
    if command.control_token_hash.is_none() {
        return Ok(());
    }
    validate_control_claims_for_command(state, session_id, command)
}

pub(crate) fn control_token_hash(token: &str) -> String {
    sha256_hex(token)
}

fn validate_control_claims(
    state: &AppState,
    session_id: &str,
    action: &str,
    claims: &ControlTokenClaims,
) -> Result<(), ControlError> {
    if claims.session_id != session_id {
        return Err(control_error(
            ErrorCode::StaleControlToken,
            format!(
                "token belongs to session {}, not {session_id}",
                claims.session_id
            ),
            "refresh_session_summary",
            json!({"token_session_id": claims.session_id, "requested_session_id": session_id}),
        ));
    }
    if !claims.allowed_actions.iter().any(|item| item == action) {
        return Err(control_error(
            ErrorCode::ActionNotAllowed,
            format!("control token does not allow action {action}"),
            "inspect_session_summary",
            json!({
                "session_id": session_id,
                "action": action,
                "allowed_actions": claims.allowed_actions
            }),
        ));
    }
    validate_owner_and_epoch(state, session_id, action, claims)
}

fn validate_control_claims_for_command(
    state: &AppState,
    session_id: &str,
    command: &CommandEnvelopeV1,
) -> Result<(), ControlError> {
    let action = command
        .payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("");
    let Some(control_epoch) = command.control_epoch else {
        return Err(control_error(
            ErrorCode::StaleControlToken,
            "command is missing control_epoch".to_string(),
            "refresh_session_summary",
            json!({"session_id": session_id, "action": action}),
        ));
    };
    let claims = ControlTokenClaims {
        session_id: command.session_id.clone(),
        owner_id: command.owner_id.clone(),
        owner_lease_id: command.owner_lease_id.clone(),
        lease_generation: command.lease_generation,
        control_epoch,
        allowed_actions: vec![action.to_string()],
        issued_at: command.created_at.clone(),
        expires_at: String::new(),
    };
    validate_owner_and_epoch(state, session_id, action, &claims)
}

fn validate_owner_and_epoch(
    state: &AppState,
    session_id: &str,
    action: &str,
    claims: &ControlTokenClaims,
) -> Result<(), ControlError> {
    if !state.sessions.lock().unwrap().contains_key(session_id) {
        return Err(control_error(
            ErrorCode::StaleControlToken,
            "session is no longer live".to_string(),
            "inspect_session_summary",
            json!({"session_id": session_id, "action": action, "live": false}),
        ));
    }
    validate_owner_lease_precondition(
        state,
        session_id,
        &claims.owner_id,
        &claims.owner_lease_id,
        claims.lease_generation,
    )
    .map_err(|err| {
        control_error(
            ErrorCode::StaleControlToken,
            err,
            "refresh_session_summary",
            json!({
                "session_id": session_id,
                "action": action,
                "owner_id": claims.owner_id,
                "owner_lease_id": claims.owner_lease_id,
                "lease_generation": claims.lease_generation
            }),
        )
    })?;
    let projection = read_session_projection(state, session_id)
        .unwrap_or_else(|| missing_projection(session_id));
    if projection.control_epoch != claims.control_epoch {
        return Err(control_error(
            ErrorCode::StaleControlToken,
            format!(
                "control_epoch changed from {} to {}",
                claims.control_epoch, projection.control_epoch
            ),
            "refresh_session_summary",
            json!({
                "session_id": session_id,
                "action": action,
                "token_epoch": claims.control_epoch,
                "current_epoch": projection.control_epoch,
                "attention_status": projection.attention_status,
                "liveness_status": projection.liveness_status
            }),
        ));
    }
    Ok(())
}

fn allowed_actions_for_projection(projection: &SessionProjectionV1) -> Vec<String> {
    let mut actions = HashSet::new();
    actions.insert("continue".to_string());
    actions.insert("request_report".to_string());
    actions.insert("revise_plan".to_string());
    actions.insert("select_option".to_string());
    actions.insert("send".to_string());
    actions.insert("submit_pending_prompt".to_string());
    if !projection.terminal {
        actions.insert("interrupt".to_string());
        actions.insert("stop".to_string());
    }
    if projection.liveness_status != "running_terminal" {
        actions.insert("kill".to_string());
    }
    if projection.pending_interaction.is_object()
        || projection.attention_status == "needs_permission"
    {
        actions.insert("select_option".to_string());
    }
    if projection.current_task.contains("plan")
        || projection.turn_status.contains("plan")
        || projection.attention_status == "waiting_input"
    {
        actions.insert("approve_plan".to_string());
        actions.insert("start_auto".to_string());
    }
    let mut actions = actions.into_iter().collect::<Vec<_>>();
    actions.sort();
    actions
}

fn cleanup_expired_control_tokens(state: &AppState) {
    let now = chrono::Utc::now();
    state.control_tokens.lock().unwrap().retain(|_, claims| {
        chrono::DateTime::parse_from_rfc3339(&claims.expires_at)
            .map(|expires| expires.with_timezone(&chrono::Utc) > now)
            .unwrap_or(false)
    });
}

fn control_error(
    status: ErrorCode,
    reason: String,
    next_step: &'static str,
    current: Value,
) -> ControlError {
    ControlError {
        status,
        reason,
        next_step,
        current,
    }
}

fn projection_current(projection: &SessionProjectionV1) -> Value {
    json!({
        "session_id": projection.session_id,
        "liveness_status": projection.liveness_status,
        "attention_status": projection.attention_status,
        "control_epoch": projection.control_epoch,
        "terminal": projection.terminal
    })
}

fn missing_projection(session_id: &str) -> SessionProjectionV1 {
    let mut projection = crate::projection::stale_projection_for_session_name(session_id);
    projection.control_epoch = 0;
    projection
}

fn random_control_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    fill_random(&mut bytes)?;
    Ok(format!("ctl_{}", base64url_no_pad(&bytes)))
}

fn base64url_no_pad(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    let mut chunks = bytes.chunks_exact(3);
    for chunk in &mut chunks {
        let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | chunk[2] as u32;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        out.push(TABLE[(n & 0x3f) as usize] as char);
    }
    let rem = chunks.remainder();
    if rem.len() == 1 {
        let n = (rem[0] as u32) << 16;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
    } else if rem.len() == 2 {
        let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
    }
    out
}

#[cfg(windows)]
fn fill_random(bytes: &mut [u8]) -> Result<(), String> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(format!("BCryptGenRandom failed with NTSTATUS {status:#x}"))
    }
}

#[cfg(unix)]
fn fill_random(bytes: &mut [u8]) -> Result<(), String> {
    let mut file = std::fs::File::open("/dev/urandom").map_err(|err| err.to_string())?;
    file.read_exact(bytes).map_err(|err| err.to_string())
}

#[cfg(not(any(windows, unix)))]
fn fill_random(_bytes: &mut [u8]) -> Result<(), String> {
    Err("no CSPRNG backend available for this platform".to_string())
}

pub(crate) fn command_type_needs_actor_revalidation(command_type: &CommandType) -> bool {
    matches!(
        command_type,
        CommandType::StopSession | CommandType::KillSession | CommandType::InterruptTurn
    )
}
