use crate::actor::submit_session_command;
use crate::commands::{PreparedCommand, prepare_session_send_command};
use crate::routes::{patch_route_record, route_for_wrapper_session};
use crate::state::AppState;
use crate::util::now_ms;
use serde_json::{Value, json};

pub(crate) const DEFAULT_ACK_DEADLINE_MS: u64 = 15_000;
pub(crate) const DEFAULT_COMMIT_ACK_DEADLINE_MS: u64 = 8_000;
pub(crate) const DEFAULT_AUTO_COMMIT_GRACE_MS: u64 = 2_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PromptGateState {
    NotRequired,
    PromptPendingAck,
    PromptMissing,
    CommitSignalSent,
    PromptSubmitted,
    PromptCommitUnacknowledged,
    PromptCommitFailed,
    AbortedSessionExited,
}

impl PromptGateState {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::PromptPendingAck => "prompt_pending_ack",
            Self::PromptMissing => "prompt_missing",
            Self::CommitSignalSent => "commit_signal_sent",
            Self::PromptSubmitted => "prompt_submitted",
            Self::PromptCommitUnacknowledged => "prompt_commit_unacknowledged",
            Self::PromptCommitFailed => "prompt_commit_failed",
            Self::AbortedSessionExited => "aborted_session_exited",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PromptGateView {
    pub(crate) route_id: Option<String>,
    pub(crate) prompt_id: Option<String>,
    pub(crate) state: PromptGateState,
    pub(crate) task_started: bool,
    pub(crate) ack_deadline_exceeded: bool,
    pub(crate) commit_deadline_exceeded: bool,
    pub(crate) commit_attempts: u64,
    pub(crate) active_commit_attempt_id: Option<String>,
    pub(crate) prompt_age_ms: u64,
    pub(crate) ack_deadline_ms: u64,
    pub(crate) commit_ack_deadline_ms: u64,
    pub(crate) awaiting_hook: Option<String>,
    pub(crate) handoff_path: Option<String>,
}

impl PromptGateView {
    pub(crate) fn none() -> Self {
        Self {
            route_id: None,
            prompt_id: None,
            state: PromptGateState::NotRequired,
            task_started: false,
            ack_deadline_exceeded: false,
            commit_deadline_exceeded: false,
            commit_attempts: 0,
            active_commit_attempt_id: None,
            prompt_age_ms: 0,
            ack_deadline_ms: DEFAULT_ACK_DEADLINE_MS,
            commit_ack_deadline_ms: DEFAULT_COMMIT_ACK_DEADLINE_MS,
            awaiting_hook: None,
            handoff_path: None,
        }
    }

    pub(crate) fn can_submit_pending_prompt(&self) -> bool {
        matches!(
            self.state,
            PromptGateState::PromptMissing | PromptGateState::PromptCommitUnacknowledged
        ) && !self.task_started
            && self.commit_attempts < 2
    }

    pub(crate) fn can_daemon_auto_submit_pending_prompt(&self) -> bool {
        !self.task_started
            && self.commit_attempts < 2
            && (self.can_submit_pending_prompt()
                || (self.state == PromptGateState::PromptPendingAck
                    && self.prompt_age_ms >= DEFAULT_AUTO_COMMIT_GRACE_MS))
    }

    pub(crate) fn is_prompt_gate_active(&self) -> bool {
        matches!(
            self.state,
            PromptGateState::PromptPendingAck
                | PromptGateState::PromptMissing
                | PromptGateState::CommitSignalSent
                | PromptGateState::PromptCommitUnacknowledged
        ) && !self.task_started
    }

    pub(crate) fn to_value(&self) -> Value {
        json!({
            "route_id": self.route_id,
            "prompt_id": self.prompt_id,
            "state": self.state.as_str(),
            "task_started": self.task_started,
            "awaiting_hook": self.awaiting_hook,
            "ack_deadline_ms": self.ack_deadline_ms,
            "ack_deadline_exceeded": self.ack_deadline_exceeded,
            "commit_ack_deadline_ms": self.commit_ack_deadline_ms,
            "commit_deadline_exceeded": self.commit_deadline_exceeded,
            "commit_attempts": self.commit_attempts,
            "active_commit_attempt_id": self.active_commit_attempt_id,
            "prompt_age_ms": self.prompt_age_ms,
            "daemon_auto_commit_after_ms": DEFAULT_AUTO_COMMIT_GRACE_MS,
            "can_submit_pending_prompt": self.can_submit_pending_prompt(),
            "daemon_auto_submit_enabled": self.can_daemon_auto_submit_pending_prompt(),
            "handoff_path": self.handoff_path,
        })
    }
}

pub(crate) fn route_prompt_id(route_id: &str, wrapper_session: &str) -> String {
    format!("route_prompt:{route_id}:{wrapper_session}")
}

pub(crate) fn prompt_commit_attempt_id(
    route_id: &str,
    wrapper_session: &str,
    attempt: u64,
) -> String {
    format!("prompt-commit-{route_id}-{wrapper_session}-{attempt}")
}

pub(crate) fn prompt_gate_for_session(state: &AppState, wrapper_session: &str) -> PromptGateView {
    let Some((route_id, route)) = route_for_wrapper_session(state, wrapper_session) else {
        return PromptGateView::none();
    };
    prompt_gate_from_route(&route_id, &route)
}

pub(crate) fn refresh_prompt_gate_timeouts_for_session(
    state: &AppState,
    wrapper_session: &str,
) -> PromptGateView {
    let view = prompt_gate_for_session(state, wrapper_session);
    if view.can_daemon_auto_submit_pending_prompt() {
        if daemon_auto_submit_pending_prompt(state, wrapper_session, &view).is_ok() {
            return prompt_gate_for_session(state, wrapper_session);
        }
    }
    if matches!(view.state, PromptGateState::PromptCommitUnacknowledged) {
        if let Some(route_id) = view.route_id.as_deref() {
            let _ = patch_route_record(
                state,
                route_id,
                json!({
                    "status": "prompt_commit_unacknowledged",
                    "updated_at": now_ms(),
                    "result": {
                        "prompt_gate": {
                            "state": "prompt_commit_unacknowledged",
                            "awaiting_hook": "UserPromptSubmit",
                            "last_error": "commit signal was not acknowledged before deadline"
                        },
                        "prompt": {
                            "state": "prompt_commit_unacknowledged",
                            "awaiting_hook": "UserPromptSubmit",
                            "last_error": "commit signal was not acknowledged before deadline"
                        }
                    }
                }),
            );
        }
    }
    view
}

fn daemon_auto_submit_pending_prompt(
    state: &AppState,
    wrapper_session: &str,
    view: &PromptGateView,
) -> Result<(), String> {
    let route_id = view
        .route_id
        .clone()
        .ok_or_else(|| "missing route_id for prompt gate".to_string())?;
    let attempt_index = view.commit_attempts.saturating_add(1);
    let attempt_id = prompt_commit_attempt_id(&route_id, wrapper_session, attempt_index);
    let sent_at_ms = now_ms();
    let args = json!({
        "idempotency_key": attempt_id,
        "owner_id": "codex"
    });
    let mut command =
        match prepare_session_send_command(state, wrapper_session, "submit_pending_prompt", &args)?
        {
            PreparedCommand::Submit(command) => command,
            PreparedCommand::Deduped(_) => return Ok(()),
        };
    command.payload["text"] = json!(" ");
    command.payload["enter"] = json!(true);
    command.payload["attempt_id"] = json!(attempt_id.clone());
    command.payload["prompt_id"] = view
        .prompt_id
        .clone()
        .map(Value::String)
        .unwrap_or(Value::Null);
    let _ = submit_session_command(state, wrapper_session, command)?;
    let attempts =
        prompt_commit_attempts_for_session(state, wrapper_session, &attempt_id, sent_at_ms);
    patch_route_record(
        state,
        &route_id,
        json!({
            "status": "prompt_commit_signal_sent",
            "updated_at": now_ms(),
            "result": {
                "prompt": {
                    "state": "commit_signal_sent",
                    "task_started": false,
                    "active_commit_attempt_id": attempt_id,
                    "active_commit_sent_at_ms": sent_at_ms,
                    "commit_ack_deadline_ms": DEFAULT_COMMIT_ACK_DEADLINE_MS,
                    "awaiting_hook": "UserPromptSubmit",
                    "commit_attempts": attempts
                },
                "prompt_gate": {
                    "schema_version": 2,
                    "state": "commit_signal_sent",
                    "task_started": false,
                    "active_commit_attempt_id": attempt_id,
                    "active_commit_sent_at_ms": sent_at_ms,
                    "commit_ack_deadline_ms": DEFAULT_COMMIT_ACK_DEADLINE_MS,
                    "awaiting_hook": "UserPromptSubmit",
                    "commit_attempts": attempts
                }
            }
        }),
    )
}

fn prompt_commit_attempts_for_session(
    state: &AppState,
    wrapper_session: &str,
    attempt_id: &str,
    sent_at_ms: u64,
) -> Value {
    let mut attempts = route_for_wrapper_session(state, wrapper_session)
        .and_then(|(_, route)| {
            route
                .pointer("/result/prompt_gate/commit_attempts")
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default();
    let next_index = attempts.len() + 1;
    attempts.push(json!({
        "attempt_id": attempt_id,
        "kind": "daemon_auto",
        "state": "signal_sent",
        "sent_at_ms": sent_at_ms,
        "ack_deadline_ms": DEFAULT_COMMIT_ACK_DEADLINE_MS,
        "index": next_index,
    }));
    Value::Array(attempts)
}

pub(crate) fn prompt_gate_from_route(route_id: &str, route: &Value) -> PromptGateView {
    let gate = route
        .pointer("/result/prompt_gate")
        .or_else(|| route.pointer("/result/prompt"))
        .unwrap_or(&Value::Null);
    if !gate.is_object() {
        return PromptGateView::none();
    }
    let acknowledged = gate
        .get("acknowledged")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let task_started = gate
        .get("task_started")
        .and_then(Value::as_bool)
        .unwrap_or(acknowledged);
    let commit_attempts = gate
        .get("commit_attempts")
        .and_then(Value::as_array)
        .map(|items| items.len() as u64)
        .or_else(|| gate.get("submit_attempts").and_then(Value::as_u64))
        .unwrap_or(0);
    let explicit_state = gate
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if acknowledged {
                "prompt_submitted"
            } else {
                "prompt_pending_ack"
            }
        });
    let prompt_written_at = gate
        .get("prompt_written_at_ms")
        .or_else(|| gate.get("written_at_ms"))
        .or_else(|| gate.get("created_at_ms"))
        .and_then(Value::as_u64)
        .or_else(|| route.get("updated_at").and_then(Value::as_u64))
        .unwrap_or(0);
    let ack_deadline_ms = gate
        .get("ack_deadline_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_ACK_DEADLINE_MS);
    let ack_deadline_exceeded = !task_started
        && prompt_written_at > 0
        && now_ms().saturating_sub(prompt_written_at) >= ack_deadline_ms;
    let prompt_age_ms = if prompt_written_at > 0 {
        now_ms().saturating_sub(prompt_written_at)
    } else {
        0
    };
    let active_commit_attempt_id = gate
        .get("active_commit_attempt_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let commit_ack_deadline_ms = gate
        .get("commit_ack_deadline_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_COMMIT_ACK_DEADLINE_MS);
    let commit_sent_at = gate
        .get("active_commit_sent_at_ms")
        .and_then(Value::as_u64)
        .or_else(|| {
            gate.get("commit_attempts")
                .and_then(Value::as_array)
                .and_then(|items| items.last())
                .and_then(|item| item.get("sent_at_ms"))
                .and_then(Value::as_u64)
        })
        .unwrap_or(0);
    let commit_deadline_exceeded = !task_started
        && commit_sent_at > 0
        && now_ms().saturating_sub(commit_sent_at) >= commit_ack_deadline_ms;

    let state = match explicit_state {
        "not_required" => PromptGateState::NotRequired,
        "prompt_submitted" => PromptGateState::PromptSubmitted,
        "prompt_commit_failed" => PromptGateState::PromptCommitFailed,
        "aborted_session_exited" => PromptGateState::AbortedSessionExited,
        "prompt_commit_unacknowledged" => PromptGateState::PromptCommitUnacknowledged,
        "commit_signal_sent" => {
            if commit_deadline_exceeded {
                PromptGateState::PromptCommitUnacknowledged
            } else {
                PromptGateState::CommitSignalSent
            }
        }
        "prompt_missing" => PromptGateState::PromptMissing,
        "prompt_pending_ack" => {
            if ack_deadline_exceeded {
                PromptGateState::PromptMissing
            } else {
                PromptGateState::PromptPendingAck
            }
        }
        _ if task_started => PromptGateState::PromptSubmitted,
        _ if ack_deadline_exceeded => PromptGateState::PromptMissing,
        _ => PromptGateState::PromptPendingAck,
    };
    PromptGateView {
        route_id: Some(route_id.to_string()),
        prompt_id: gate
            .get("prompt_id")
            .or_else(|| gate.get("prompt_idempotency_key"))
            .or_else(|| gate.get("idempotency_key"))
            .and_then(Value::as_str)
            .map(str::to_string),
        state,
        task_started,
        ack_deadline_exceeded,
        commit_deadline_exceeded,
        commit_attempts,
        active_commit_attempt_id,
        prompt_age_ms,
        ack_deadline_ms,
        commit_ack_deadline_ms,
        awaiting_hook: gate
            .get("awaiting_hook")
            .or_else(|| gate.get("ack_expected"))
            .or_else(|| gate.get("expected_hook"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| Some("UserPromptSubmit".to_string())),
        handoff_path: gate
            .get("handoff_path")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}
