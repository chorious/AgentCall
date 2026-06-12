use crate::errors::{ErrorCode, structured_error};
use crate::ownership::{
    owner_lease_is_active, prune_expired_leases, release_orphaned_runtime_leases,
};
use crate::state::AppState;
use serde_json::{Value, json};
use std::collections::HashSet;

const DEFAULT_MAX_SESSIONS: usize = 6;
const DEFAULT_PER_OWNER_MAX_SESSIONS: usize = 6;

#[derive(Clone, Debug)]
pub(crate) struct SchedulerDecision {
    pub(crate) status: String,
    pub(crate) reason: String,
    pub(crate) active_sessions: usize,
    pub(crate) active_owner_sessions: usize,
    pub(crate) max_sessions: usize,
    pub(crate) per_owner_max_sessions: usize,
}

pub(crate) fn enforce_start_capacity(
    state: &AppState,
    owner_id: &str,
) -> Result<SchedulerDecision, String> {
    let decision = scheduler_decision(state, owner_id);
    if decision.status == "start_now" {
        return Ok(decision);
    }
    Err(structured_error(
        ErrorCode::CapacityExceeded,
        decision.reason.clone(),
        json!({
            "active_sessions": decision.active_sessions,
            "max_sessions": decision.max_sessions,
            "active_owner_sessions": decision.active_owner_sessions,
            "per_owner_max_sessions": decision.per_owner_max_sessions,
            "allow_queue": false,
        }),
    ))
}

pub(crate) fn scheduler_decision(state: &AppState, owner_id: &str) -> SchedulerDecision {
    let _ = prune_expired_leases(state);
    let live_session_names = state
        .sessions
        .lock()
        .unwrap()
        .keys()
        .cloned()
        .collect::<HashSet<_>>();
    let _ = release_orphaned_runtime_leases(state, &live_session_names);
    let max_sessions = state
        .config
        .max_sessions
        .unwrap_or(DEFAULT_MAX_SESSIONS)
        .max(1);
    let per_owner_max_sessions = state
        .config
        .per_owner_max_sessions
        .unwrap_or(DEFAULT_PER_OWNER_MAX_SESSIONS)
        .max(1);
    let live_sessions = live_session_names.len();
    let now = chrono::Utc::now();
    let owner_leases = state.owner_leases.lock().unwrap();
    let active_leases = owner_leases
        .values()
        .filter(|lease| owner_lease_is_active(lease, now))
        .count();
    let active_sessions = live_sessions.max(active_leases);
    let active_owner_sessions = owner_leases
        .values()
        .filter(|lease| owner_lease_is_active(lease, now) && lease.owner_id == owner_id)
        .count();
    if active_sessions >= max_sessions {
        return SchedulerDecision {
            status: "capacity_exceeded".to_string(),
            reason: "global active session cap reached; no hidden queue is created".to_string(),
            active_sessions,
            active_owner_sessions,
            max_sessions,
            per_owner_max_sessions,
        };
    }
    if active_owner_sessions >= per_owner_max_sessions {
        return SchedulerDecision {
            status: "capacity_exceeded".to_string(),
            reason: format!(
                "owner {owner_id} active session cap reached; no hidden queue is created"
            ),
            active_sessions,
            active_owner_sessions,
            max_sessions,
            per_owner_max_sessions,
        };
    }
    SchedulerDecision {
        status: "start_now".to_string(),
        reason: "capacity available".to_string(),
        active_sessions,
        active_owner_sessions,
        max_sessions,
        per_owner_max_sessions,
    }
}

pub(crate) fn scheduler_health(state: &AppState) -> Value {
    let decision = scheduler_decision(state, "codex");
    json!({
        "active_sessions": decision.active_sessions,
        "max_sessions": decision.max_sessions,
        "codex_active_sessions": decision.active_owner_sessions,
        "per_owner_max_sessions": decision.per_owner_max_sessions,
        "queue_policy": "reject_when_full",
        "allow_hidden_queue": false,
    })
}

impl SchedulerDecision {
    pub(crate) fn to_value(&self) -> Value {
        json!({
            "status": self.status,
            "reason": self.reason,
            "active_sessions": self.active_sessions,
            "active_owner_sessions": self.active_owner_sessions,
            "max_sessions": self.max_sessions,
            "per_owner_max_sessions": self.per_owner_max_sessions,
            "queue_policy": "reject_when_full",
            "allow_hidden_queue": false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocalConfig;
    use crate::ownership::{LeaseStatus, OwnerLease};
    use crate::state::AppState;
    use std::path::PathBuf;

    #[test]
    fn scheduler_rejects_global_capacity_without_queue() {
        let state = test_state(Some(2), Some(10));
        insert_lease(&state, "worker-a", "codex");
        insert_lease(&state, "worker-b", "other");
        let err = enforce_start_capacity(&state, "codex").unwrap_err();
        let value: Value = serde_json::from_str(&err).unwrap();
        assert_eq!(value["error"]["code"], "capacity_exceeded");
        assert_eq!(value["error"]["details"]["allow_queue"], false);
    }

    #[test]
    fn scheduler_rejects_per_owner_capacity_without_queue() {
        let state = test_state(Some(10), Some(1));
        insert_lease(&state, "worker-a", "codex");
        let err = enforce_start_capacity(&state, "codex").unwrap_err();
        let value: Value = serde_json::from_str(&err).unwrap();
        assert_eq!(value["error"]["code"], "capacity_exceeded");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("owner codex active session cap reached")
        );
    }

    #[test]
    fn scheduler_ignores_expired_owner_leases() {
        let state = test_state(Some(1), Some(1));
        insert_expired_lease(&state, "worker-a", "codex");

        let decision = scheduler_decision(&state, "codex");

        assert_eq!(decision.status, "start_now");
        assert_eq!(decision.active_sessions, 0);
        assert_eq!(decision.active_owner_sessions, 0);
        assert_eq!(
            state
                .owner_leases
                .lock()
                .unwrap()
                .get("worker-a")
                .unwrap()
                .status,
            LeaseStatus::Expired
        );
    }

    #[test]
    fn scheduler_releases_old_orphaned_owner_leases() {
        let state = test_state(Some(1), Some(1));
        insert_old_lease(&state, "worker-a", "codex");

        let decision = scheduler_decision(&state, "codex");

        assert_eq!(decision.status, "start_now");
        assert_eq!(decision.active_sessions, 0);
        assert_eq!(decision.active_owner_sessions, 0);
        assert!(!state.owner_leases.lock().unwrap().contains_key("worker-a"));
    }

    fn test_state(max_sessions: Option<usize>, per_owner: Option<usize>) -> AppState {
        let root = PathBuf::from(format!(
            "{}\\agentcall-scheduler-test-{}",
            std::env::temp_dir().display(),
            std::process::id()
        ));
        AppState::new(
            root.clone(),
            LocalConfig {
                claude_workspace: Some(root),
                store_backend: None,
                max_sessions,
                per_owner_max_sessions: per_owner,
                ..LocalConfig::default()
            },
            None,
        )
    }

    fn insert_lease(state: &AppState, session_id: &str, owner_id: &str) {
        let now = chrono::Utc::now();
        let now_text = now.to_rfc3339();
        let expires_at = (now + chrono::Duration::minutes(30)).to_rfc3339();
        state.owner_leases.lock().unwrap().insert(
            session_id.to_string(),
            OwnerLease {
                lease_id: format!("lease-{session_id}"),
                owner_id: owner_id.to_string(),
                session_id: session_id.to_string(),
                lease_generation: 1,
                acquired_at: now_text.clone(),
                last_heartbeat_at: now_text.clone(),
                renewed_at: now_text,
                expires_at,
                status: LeaseStatus::Active,
                recoverable: true,
            },
        );
    }

    fn insert_expired_lease(state: &AppState, session_id: &str, owner_id: &str) {
        let now = chrono::Utc::now();
        let expired = (now - chrono::Duration::minutes(10)).to_rfc3339();
        state.owner_leases.lock().unwrap().insert(
            session_id.to_string(),
            OwnerLease {
                lease_id: format!("lease-{session_id}"),
                owner_id: owner_id.to_string(),
                session_id: session_id.to_string(),
                lease_generation: 1,
                acquired_at: expired.clone(),
                last_heartbeat_at: expired.clone(),
                renewed_at: expired.clone(),
                expires_at: expired,
                status: LeaseStatus::Active,
                recoverable: true,
            },
        );
    }

    fn insert_old_lease(state: &AppState, session_id: &str, owner_id: &str) {
        let now = chrono::Utc::now();
        let old = (now - chrono::Duration::minutes(10)).to_rfc3339();
        let expires_at = (now + chrono::Duration::minutes(20)).to_rfc3339();
        state.owner_leases.lock().unwrap().insert(
            session_id.to_string(),
            OwnerLease {
                lease_id: format!("lease-{session_id}"),
                owner_id: owner_id.to_string(),
                session_id: session_id.to_string(),
                lease_generation: 1,
                acquired_at: old.clone(),
                last_heartbeat_at: old.clone(),
                renewed_at: old,
                expires_at,
                status: LeaseStatus::Active,
                recoverable: true,
            },
        );
    }
}
