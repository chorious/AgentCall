use crate::routes::{patch_route_record, route_for_wrapper_session};
use crate::state::AppState;
use crate::util::now_ms;
use serde_json::{Value, json};

pub(crate) const DEFAULT_ACK_DEADLINE_MS: u64 = 15_000;
pub(crate) const DEFAULT_COMMIT_ACK_DEADLINE_MS: u64 = 8_000;

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
            "can_submit_pending_prompt": self.can_submit_pending_prompt(),
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
