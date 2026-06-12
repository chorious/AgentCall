use crate::errors::{ErrorCode, structured_error};
use crate::state::{AppState, read_json_file, write_json_file};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum LeaseStatus {
    Active,
    Released,
    Expired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct OwnerLease {
    pub(crate) lease_id: String,
    pub(crate) owner_id: String,
    pub(crate) session_id: String,
    pub(crate) lease_generation: u64,
    pub(crate) acquired_at: String,
    pub(crate) last_heartbeat_at: String,
    pub(crate) renewed_at: String,
    pub(crate) expires_at: String,
    pub(crate) status: LeaseStatus,
    pub(crate) recoverable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum WorkspaceLeaseMode {
    Exclusive,
    SharedReport,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WorkspaceLease {
    pub(crate) lease_id: String,
    pub(crate) workspace: String,
    pub(crate) workspace_key: String,
    pub(crate) mode: WorkspaceLeaseMode,
    pub(crate) owner_id: String,
    pub(crate) session_id: String,
    pub(crate) expires_at: String,
}

pub(crate) struct RouteLeaseReservation {
    pub(crate) owner_lease: OwnerLease,
    pub(crate) workspace_lease: WorkspaceLease,
}

const ORPHANED_LEASE_GRACE_SECONDS: i64 = 30;

pub(crate) fn prune_expired_leases(state: &AppState) -> Result<(), String> {
    let now = chrono::Utc::now();
    let mut expired_owner_leases = Vec::new();
    {
        let mut leases = state.owner_leases.lock().unwrap();
        for lease in leases.values_mut() {
            if lease.status == LeaseStatus::Active && lease_expired_at(&lease.expires_at, now) {
                lease.status = LeaseStatus::Expired;
                lease.renewed_at = now.to_rfc3339();
                lease.recoverable = false;
                expired_owner_leases.push(lease.clone());
            }
        }
        if !expired_owner_leases.is_empty() {
            persist_owner_leases(state, &leases)?;
        }
    }
    for lease in expired_owner_leases {
        let _ = state.store.upsert_owner_lease(&lease);
        crate::state::append_agent_event(
            state,
            "owner_lease.expired",
            "Owner lease expired.",
            json!({
                "session_id": lease.session_id,
                "lease_id": lease.lease_id,
                "owner_id": lease.owner_id,
                "expires_at": lease.expires_at
            }),
        );
    }

    let mut expired_workspace_leases = Vec::new();
    {
        let mut leases = state.workspace_leases.lock().unwrap();
        leases.retain(|session_id, lease| {
            let active = workspace_lease_is_active(lease, now);
            if !active {
                expired_workspace_leases.push((session_id.clone(), lease.clone()));
            }
            active
        });
        if !expired_workspace_leases.is_empty() {
            persist_workspace_leases(state, &leases)?;
        }
    }
    for (session_id, lease) in expired_workspace_leases {
        let _ = state.store.release_workspace_lease(&session_id, "expired");
        crate::state::append_agent_event(
            state,
            "workspace_lease.expired",
            "Workspace lease expired.",
            json!({
                "session_id": session_id,
                "lease_id": lease.lease_id,
                "workspace_key": lease.workspace_key,
                "expires_at": lease.expires_at
            }),
        );
    }
    Ok(())
}

pub(crate) fn release_orphaned_runtime_leases(
    state: &AppState,
    live_session_names: &HashSet<String>,
) -> Result<usize, String> {
    prune_expired_leases(state)?;
    let now = chrono::Utc::now();
    let grace = chrono::Duration::seconds(ORPHANED_LEASE_GRACE_SECONDS);
    let owner_sessions = {
        let leases = state.owner_leases.lock().unwrap();
        leases
            .values()
            .filter(|lease| {
                owner_lease_is_active(lease, now)
                    && lease.recoverable
                    && !live_session_names.contains(&lease.session_id)
                    && lease_timestamp_older_than(&lease.last_heartbeat_at, now, grace)
            })
            .map(|lease| lease.session_id.clone())
            .collect::<Vec<_>>()
    };

    let mut released = 0usize;
    for session_id in owner_sessions {
        if release_owner_lease(state, &session_id, "orphaned_no_live_session")?.is_some() {
            released += 1;
        }
        let _ = release_workspace_lease(state, &session_id, "orphaned_no_live_session")?;
    }

    let active_owner_sessions = {
        let leases = state.owner_leases.lock().unwrap();
        leases
            .values()
            .filter(|lease| owner_lease_is_active(lease, now))
            .map(|lease| lease.session_id.clone())
            .collect::<HashSet<_>>()
    };
    let workspace_sessions = {
        let leases = state.workspace_leases.lock().unwrap();
        leases
            .values()
            .filter(|lease| {
                workspace_lease_is_active(lease, now)
                    && !live_session_names.contains(&lease.session_id)
                    && !active_owner_sessions.contains(&lease.session_id)
            })
            .map(|lease| lease.session_id.clone())
            .collect::<Vec<_>>()
    };
    for session_id in workspace_sessions {
        if release_workspace_lease(state, &session_id, "orphaned_no_live_session")?.is_some() {
            released += 1;
        }
    }

    Ok(released)
}

pub(crate) fn owner_lease_is_active(
    lease: &OwnerLease,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    lease.status == LeaseStatus::Active && !lease_expired_at(&lease.expires_at, now)
}

pub(crate) fn workspace_lease_is_active(
    lease: &WorkspaceLease,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    !lease_expired_at(&lease.expires_at, now)
}

fn lease_expired_at(expires_at: &str, now: chrono::DateTime<chrono::Utc>) -> bool {
    let Ok(expires) = chrono::DateTime::parse_from_rfc3339(expires_at) else {
        return true;
    };
    expires.with_timezone(&chrono::Utc) <= now
}

fn lease_timestamp_older_than(
    timestamp: &str,
    now: chrono::DateTime<chrono::Utc>,
    grace: chrono::Duration,
) -> bool {
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(timestamp) else {
        return true;
    };
    now.signed_duration_since(parsed.with_timezone(&chrono::Utc)) >= grace
}

pub(crate) fn ensure_owner_lease(
    state: &AppState,
    session_id: &str,
    owner_id: &str,
) -> Result<OwnerLease, String> {
    prune_expired_leases(state)?;
    let mut leases = state.owner_leases.lock().unwrap();
    if let Some(existing) = leases.get(session_id) {
        if existing.owner_id != owner_id {
            return Err(structured_error(
                ErrorCode::OwnerConflict,
                "Session is already owned by another owner.",
                json!({
                    "session_id": session_id,
                    "existing_owner": existing.owner_id.clone(),
                    "requested_owner": owner_id,
                }),
            ));
        }
        if owner_lease_is_active(existing, chrono::Utc::now()) {
            return Ok(existing.clone());
        }
        leases.remove(session_id);
    }
    let lease = build_owner_lease(session_id, owner_id);
    leases.insert(session_id.to_string(), lease.clone());
    persist_owner_leases(state, &leases)?;
    state.store.upsert_owner_lease(&lease)?;
    Ok(lease)
}

pub(crate) fn attach_or_validate_owner_lease(
    state: &AppState,
    session_id: &str,
    args: &Value,
) -> Result<Value, String> {
    let mut enriched = args.clone();
    if !enriched.is_object() {
        enriched = json!({});
    }
    let owner_id = enriched
        .get("owner_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            enriched
                .get("precondition")
                .and_then(|value| value.get("owner_id"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or("codex")
        .to_string();
    let lease = ensure_owner_lease(state, session_id, &owner_id)?;
    let provided_lease_id = enriched.get("owner_lease_id").and_then(Value::as_str);
    let provided_generation = enriched.get("lease_generation").and_then(Value::as_u64);
    if let Some(provided) = provided_lease_id {
        if provided != lease.lease_id {
            return Err(format!(
                "rejected_stale_lease: session={session_id} expected={} got={provided}",
                lease.lease_id
            ));
        }
    }
    if let Some(provided) = provided_generation {
        if provided != lease.lease_generation {
            return Err(format!(
                "rejected_stale_lease_generation: session={session_id} expected={} got={provided}",
                lease.lease_generation
            ));
        }
    }
    let object = enriched.as_object_mut().unwrap();
    object.insert("owner_id".to_string(), json!(lease.owner_id));
    object.insert("owner_lease_id".to_string(), json!(lease.lease_id));
    object.insert(
        "lease_generation".to_string(),
        json!(lease.lease_generation),
    );
    let precondition = object
        .entry("precondition".to_string())
        .or_insert_with(|| json!({}));
    if precondition.is_object() {
        let precondition = precondition.as_object_mut().unwrap();
        precondition
            .entry("owner_lease_id".to_string())
            .or_insert_with(|| json!(lease.lease_id));
        precondition
            .entry("lease_generation".to_string())
            .or_insert_with(|| json!(lease.lease_generation));
    }
    Ok(enriched)
}

pub(crate) fn validate_owner_lease_precondition(
    state: &AppState,
    session_id: &str,
    owner_id: &str,
    owner_lease_id: &str,
    lease_generation: u64,
) -> Result<(), String> {
    prune_expired_leases(state)?;
    let leases = state.owner_leases.lock().unwrap();
    let Some(lease) = leases.get(session_id) else {
        return Err(format!(
            "rejected_stale_lease: session={session_id} has no active owner lease"
        ));
    };
    if lease.owner_id != owner_id {
        return Err(format!(
            "rejected_owner_mismatch: session={session_id} expected_owner={} got={owner_id}",
            lease.owner_id
        ));
    }
    match lease.status {
        LeaseStatus::Active => {}
        LeaseStatus::Expired => {
            return Err(format!(
                "rejected_expired_lease: session={session_id} lease={} is expired",
                lease.lease_id
            ));
        }
        LeaseStatus::Released => {
            return Err(format!(
                "rejected_stale_lease: session={session_id} lease={} is released",
                lease.lease_id
            ));
        }
    }
    if !owner_lease_is_active(lease, chrono::Utc::now()) {
        return Err(format!(
            "rejected_expired_lease: session={session_id} lease={} is expired",
            lease.lease_id
        ));
    }
    if lease.lease_id != owner_lease_id {
        return Err(format!(
            "rejected_stale_lease: session={session_id} expected={} got={owner_lease_id}",
            lease.lease_id
        ));
    }
    if lease.lease_generation != lease_generation {
        return Err(format!(
            "rejected_stale_lease_generation: session={session_id} expected={} got={lease_generation}",
            lease.lease_generation
        ));
    }
    Ok(())
}

pub(crate) fn reserve_route_leases(
    state: &AppState,
    session_id: &str,
    owner_id: &str,
    workspace: &Path,
    shared_report: bool,
) -> Result<RouteLeaseReservation, String> {
    prune_expired_leases(state)?;
    let owner_lease = build_owner_lease(session_id, owner_id);
    let workspace_lease = build_workspace_lease(session_id, owner_id, workspace, shared_report);
    {
        let now = chrono::Utc::now();
        let owner_leases = state.owner_leases.lock().unwrap();
        if let Some(existing) = owner_leases.get(session_id) {
            if owner_lease_is_active(existing, now) {
                return Err(structured_error(
                    ErrorCode::OwnerLeaseExists,
                    "Session already has an active owner lease.",
                    json!({
                        "session_id": session_id,
                        "owner_id": existing.owner_id.clone(),
                        "lease_id": existing.lease_id.clone(),
                        "lease_generation": existing.lease_generation,
                        "expires_at": existing.expires_at.clone(),
                    }),
                ));
            }
        }
    }
    {
        let now = chrono::Utc::now();
        let workspace_leases = state.workspace_leases.lock().unwrap();
        for existing in workspace_leases.values() {
            if !workspace_lease_is_active(existing, now) {
                continue;
            }
            if existing.workspace_key != workspace_lease.workspace_key
                || existing.session_id == session_id
            {
                continue;
            }
            if existing.mode == WorkspaceLeaseMode::Exclusive
                || workspace_lease.mode == WorkspaceLeaseMode::Exclusive
            {
                return Err(structured_error(
                    ErrorCode::WorkspaceBusy,
                    "Workspace has an incompatible active lease.",
                    json!({
                        "workspace": workspace.display().to_string(),
                        "workspace_key": workspace_lease.workspace_key,
                        "requested_session": session_id,
                        "requested_owner": owner_id,
                        "requested_mode": workspace_lease.mode.clone(),
                        "existing_session": existing.session_id.clone(),
                        "existing_owner": existing.owner_id.clone(),
                        "existing_mode": existing.mode.clone(),
                        "existing_expires_at": existing.expires_at.clone(),
                        "suggested_action": "Use report-only shared routing, wait for the existing worker report, or stop/release the stale session explicitly."
                    }),
                ));
            }
        }
    }
    Ok(RouteLeaseReservation {
        owner_lease,
        workspace_lease,
    })
}

pub(crate) fn install_reserved_route_leases(
    state: &AppState,
    reservation: &RouteLeaseReservation,
) -> Result<(), String> {
    let owner_snapshot = {
        let mut owner_leases = state.owner_leases.lock().unwrap();
        owner_leases.insert(
            reservation.owner_lease.session_id.clone(),
            reservation.owner_lease.clone(),
        );
        owner_leases.clone()
    };
    persist_owner_leases(state, &owner_snapshot)?;
    let workspace_snapshot = {
        let mut workspace_leases = state.workspace_leases.lock().unwrap();
        workspace_leases.insert(
            reservation.workspace_lease.session_id.clone(),
            reservation.workspace_lease.clone(),
        );
        workspace_leases.clone()
    };
    persist_workspace_leases(state, &workspace_snapshot)?;
    Ok(())
}

pub(crate) fn release_owner_lease(
    state: &AppState,
    session_id: &str,
    reason: &str,
) -> Result<Option<OwnerLease>, String> {
    let (lease, snapshot) = {
        let mut leases = state.owner_leases.lock().unwrap();
        let Some(mut lease) = leases.remove(session_id) else {
            return Ok(None);
        };
        lease.status = LeaseStatus::Released;
        lease.renewed_at = chrono::Utc::now().to_rfc3339();
        (lease, leases.clone())
    };
    persist_owner_leases(state, &snapshot)?;
    state.store.release_owner_lease(session_id, reason)?;
    crate::state::append_agent_event(
        state,
        "owner_lease.released",
        "Owner lease released.",
        json!({"session_id": session_id, "lease_id": lease.lease_id, "owner_id": lease.owner_id, "reason": reason}),
    );
    Ok(Some(lease))
}

#[allow(dead_code)]
pub(crate) fn acquire_workspace_lease(
    state: &AppState,
    session_id: &str,
    workspace: &Path,
    shared_report: bool,
) -> Result<WorkspaceLease, String> {
    prune_expired_leases(state)?;
    let mode = if shared_report {
        WorkspaceLeaseMode::SharedReport
    } else {
        WorkspaceLeaseMode::Exclusive
    };
    let workspace_key = canonical_workspace_key(workspace);
    let now = chrono::Utc::now();
    let (lease, snapshot) = {
        let mut leases = state.workspace_leases.lock().unwrap();
        for existing in leases.values() {
            if !workspace_lease_is_active(existing, now) {
                continue;
            }
            if existing.workspace_key != workspace_key || existing.session_id == session_id {
                continue;
            }
            if existing.mode == WorkspaceLeaseMode::Exclusive
                || mode == WorkspaceLeaseMode::Exclusive
            {
                return Err(structured_error(
                    ErrorCode::WorkspaceBusy,
                    "Workspace has an incompatible active lease.",
                    json!({
                        "workspace": workspace.display().to_string(),
                        "workspace_key": workspace_key.clone(),
                        "requested_session": session_id,
                        "requested_mode": mode.clone(),
                        "existing_session": existing.session_id.clone(),
                        "existing_owner": existing.owner_id.clone(),
                        "existing_mode": existing.mode.clone(),
                        "existing_expires_at": existing.expires_at.clone(),
                        "suggested_action": "Use report-only shared routing, wait for the existing worker report, or stop/release the stale session explicitly."
                    }),
                ));
            }
        }
        let lease =
            build_workspace_lease_with_key(session_id, "codex", workspace, workspace_key, mode);
        leases.insert(session_id.to_string(), lease.clone());
        (lease, leases.clone())
    };
    persist_workspace_leases(state, &snapshot)?;
    state.store.upsert_workspace_lease(&lease)?;
    Ok(lease)
}

pub(crate) fn release_workspace_lease(
    state: &AppState,
    session_id: &str,
    reason: &str,
) -> Result<Option<WorkspaceLease>, String> {
    let (lease, snapshot) = {
        let mut leases = state.workspace_leases.lock().unwrap();
        let Some(lease) = leases.remove(session_id) else {
            return Ok(None);
        };
        (lease, leases.clone())
    };
    persist_workspace_leases(state, &snapshot)?;
    state.store.release_workspace_lease(session_id, reason)?;
    crate::state::append_agent_event(
        state,
        "workspace_lease.released",
        "Workspace lease released.",
        json!({"session_id": session_id, "lease_id": lease.lease_id, "workspace_key": lease.workspace_key, "reason": reason}),
    );
    Ok(Some(lease))
}

fn build_owner_lease(session_id: &str, owner_id: &str) -> OwnerLease {
    let now = chrono::Utc::now();
    OwnerLease {
        lease_id: format!("lease-{session_id}-1"),
        owner_id: owner_id.to_string(),
        session_id: session_id.to_string(),
        lease_generation: 1,
        acquired_at: now.to_rfc3339(),
        last_heartbeat_at: now.to_rfc3339(),
        renewed_at: now.to_rfc3339(),
        expires_at: (now + chrono::Duration::minutes(30)).to_rfc3339(),
        status: LeaseStatus::Active,
        recoverable: true,
    }
}

fn build_workspace_lease(
    session_id: &str,
    owner_id: &str,
    workspace: &Path,
    shared_report: bool,
) -> WorkspaceLease {
    let mode = if shared_report {
        WorkspaceLeaseMode::SharedReport
    } else {
        WorkspaceLeaseMode::Exclusive
    };
    let workspace_key = canonical_workspace_key(workspace);
    build_workspace_lease_with_key(session_id, owner_id, workspace, workspace_key, mode)
}

fn build_workspace_lease_with_key(
    session_id: &str,
    owner_id: &str,
    workspace: &Path,
    workspace_key: String,
    mode: WorkspaceLeaseMode,
) -> WorkspaceLease {
    WorkspaceLease {
        lease_id: format!("workspace-lease-{session_id}-1"),
        workspace: workspace.display().to_string(),
        workspace_key,
        mode,
        owner_id: owner_id.to_string(),
        session_id: session_id.to_string(),
        expires_at: (chrono::Utc::now() + chrono::Duration::minutes(30)).to_rfc3339(),
    }
}

pub(crate) fn canonical_workspace_key(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        }
    });
    normalize_workspace_key(&canonical)
}

fn persist_owner_leases(
    state: &AppState,
    leases: &HashMap<String, OwnerLease>,
) -> Result<(), String> {
    let value = serde_json::to_value(leases).map_err(|err| err.to_string())?;
    write_json_file(&owner_leases_path(state), &value)
}

fn persist_workspace_leases(
    state: &AppState,
    leases: &HashMap<String, WorkspaceLease>,
) -> Result<(), String> {
    let value = serde_json::to_value(leases).map_err(|err| err.to_string())?;
    write_json_file(&workspace_leases_path(state), &value)
}

fn owner_leases_path(state: &AppState) -> std::path::PathBuf {
    state
        .workspace
        .join(".agentcall")
        .join("state")
        .join("owner_leases.json")
}

fn workspace_leases_path(state: &AppState) -> std::path::PathBuf {
    state
        .workspace
        .join(".agentcall")
        .join("state")
        .join("workspace_leases.json")
}

fn normalize_workspace_key(path: &Path) -> String {
    let mut text = path.display().to_string().replace('/', "\\");
    while text.ends_with('\\') {
        text.pop();
    }
    #[cfg(windows)]
    {
        text = text.to_ascii_lowercase();
        if let Some(stripped) = text.strip_prefix(r"\\?\") {
            text = stripped.to_string();
        }
    }
    text
}

pub(crate) fn load_owner_leases(state: &AppState) {
    let value = read_json_file(&owner_leases_path(state), json!({}));
    let Ok(leases) = serde_json::from_value::<HashMap<String, OwnerLease>>(value) else {
        return;
    };
    *state.owner_leases.lock().unwrap() = leases;
    let _ = prune_expired_leases(state);
}

pub(crate) fn load_workspace_leases(state: &AppState) {
    let value = read_json_file(&workspace_leases_path(state), json!({}));
    let Ok(leases) = serde_json::from_value::<HashMap<String, WorkspaceLease>>(value) else {
        return;
    };
    *state.workspace_leases.lock().unwrap() = leases;
    let _ = prune_expired_leases(state);
}

pub(crate) fn owner_leases_summary(state: &AppState) -> serde_json::Value {
    let leases = state.owner_leases.lock().unwrap();
    let now = chrono::Utc::now();
    let active_sessions = leases
        .values()
        .filter(|lease| owner_lease_is_active(lease, now))
        .map(|lease| lease.session_id.clone())
        .collect::<Vec<_>>();
    let active = leases
        .values()
        .filter(|lease| owner_lease_is_active(lease, now))
        .count();
    json!({
        "active": active,
        "total": leases.len(),
        "sessions": active_sessions
    })
}

pub(crate) fn workspace_leases_summary(state: &AppState) -> serde_json::Value {
    let leases = state.workspace_leases.lock().unwrap();
    let now = chrono::Utc::now();
    let active_leases = leases
        .values()
        .filter(|lease| workspace_lease_is_active(lease, now))
        .collect::<Vec<_>>();
    let exclusive = active_leases
        .iter()
        .filter(|lease| lease.mode == WorkspaceLeaseMode::Exclusive)
        .count();
    let shared_report = active_leases
        .iter()
        .filter(|lease| lease.mode == WorkspaceLeaseMode::SharedReport)
        .count();
    json!({
        "active": active_leases.len(),
        "exclusive": exclusive,
        "shared_report": shared_report,
        "workspaces": active_leases.iter().map(|lease| json!({
            "session_id": lease.session_id,
            "workspace": lease.workspace,
            "workspace_key": lease.workspace_key,
            "mode": lease.mode,
            "expires_at": lease.expires_at,
        })).collect::<Vec<_>>()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocalConfig;
    use std::fs;

    fn test_state(name: &str) -> AppState {
        let root =
            std::env::temp_dir().join(format!("agentcall-owner-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".agentcall").join("state")).unwrap();
        AppState::new(root, LocalConfig::default(), None)
    }

    #[test]
    fn owner_lease_is_attached_to_command_args() {
        let state = test_state("attach");
        let enriched =
            attach_or_validate_owner_lease(&state, "worker-a", &json!({"text": "go"})).unwrap();
        assert_eq!(enriched["owner_id"], "codex");
        assert_eq!(enriched["owner_lease_id"], "lease-worker-a-1");
        assert_eq!(enriched["lease_generation"], 1);
        assert_eq!(
            enriched["precondition"]["owner_lease_id"],
            "lease-worker-a-1"
        );
    }

    #[test]
    fn stale_lease_generation_is_rejected() {
        let state = test_state("stale");
        let _ = attach_or_validate_owner_lease(&state, "worker-a", &json!({})).unwrap();
        let err = attach_or_validate_owner_lease(
            &state,
            "worker-a",
            &json!({"owner_lease_id": "lease-worker-a-1", "lease_generation": 0}),
        )
        .unwrap_err();
        assert!(err.contains("rejected_stale_lease_generation"));
    }

    #[test]
    fn expired_owner_lease_precondition_is_rejected() {
        let state = test_state("expired-owner-precondition");
        let mut lease = build_owner_lease("worker-a", "codex");
        lease.expires_at = (chrono::Utc::now() - chrono::Duration::seconds(1)).to_rfc3339();
        let lease_id = lease.lease_id.clone();
        let generation = lease.lease_generation;
        state
            .owner_leases
            .lock()
            .unwrap()
            .insert("worker-a".to_string(), lease);

        let err =
            validate_owner_lease_precondition(&state, "worker-a", "codex", &lease_id, generation)
                .unwrap_err();
        assert!(err.contains("rejected_expired_lease"));
    }

    #[test]
    fn released_owner_lease_precondition_is_rejected() {
        let state = test_state("released-owner-precondition");
        let mut lease = build_owner_lease("worker-a", "codex");
        lease.status = LeaseStatus::Released;
        let lease_id = lease.lease_id.clone();
        let generation = lease.lease_generation;
        state
            .owner_leases
            .lock()
            .unwrap()
            .insert("worker-a".to_string(), lease);

        let err =
            validate_owner_lease_precondition(&state, "worker-a", "codex", &lease_id, generation)
                .unwrap_err();
        assert!(err.contains("rejected_stale_lease"));
    }

    #[test]
    fn same_workspace_exclusive_blocks_second_writer() {
        let state = test_state("workspace-exclusive");
        let workspace = state.workspace.clone();
        let first = acquire_workspace_lease(&state, "worker-a", &workspace, false).unwrap();
        assert_eq!(first.mode, WorkspaceLeaseMode::Exclusive);

        let err = acquire_workspace_lease(&state, "worker-b", &workspace, false).unwrap_err();
        assert!(err.contains("workspace_busy"));
    }

    #[test]
    fn shared_report_allows_multiple_report_workers() {
        let state = test_state("workspace-report");
        let workspace = state.workspace.clone();
        let first = acquire_workspace_lease(&state, "report-a", &workspace, true).unwrap();
        let second = acquire_workspace_lease(&state, "report-b", &workspace, true).unwrap();
        assert_eq!(first.mode, WorkspaceLeaseMode::SharedReport);
        assert_eq!(second.mode, WorkspaceLeaseMode::SharedReport);
    }

    #[test]
    fn same_workspace_different_path_spelling_conflicts() {
        let state = test_state("workspace-canonical");
        let workspace = state.workspace.clone();
        let dotted = workspace.join(".");
        assert_eq!(
            canonical_workspace_key(&workspace),
            canonical_workspace_key(&dotted)
        );
        let _ = acquire_workspace_lease(&state, "worker-a", &workspace, false).unwrap();
        let err = acquire_workspace_lease(&state, "worker-b", &dotted, false).unwrap_err();
        assert!(err.contains("workspace_busy"));
    }

    #[test]
    fn old_orphaned_runtime_lease_releases_owner_and_workspace() {
        let state = test_state("orphan-release");
        let mut owner = build_owner_lease("worker-a", "codex");
        let old = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
        owner.acquired_at = old.clone();
        owner.last_heartbeat_at = old.clone();
        owner.renewed_at = old;
        let workspace = build_workspace_lease("worker-a", "codex", &state.workspace, false);
        state
            .owner_leases
            .lock()
            .unwrap()
            .insert("worker-a".to_string(), owner);
        state
            .workspace_leases
            .lock()
            .unwrap()
            .insert("worker-a".to_string(), workspace);

        let released = release_orphaned_runtime_leases(&state, &HashSet::new()).unwrap();

        assert_eq!(released, 1);
        assert!(!state.owner_leases.lock().unwrap().contains_key("worker-a"));
        assert!(
            !state
                .workspace_leases
                .lock()
                .unwrap()
                .contains_key("worker-a")
        );
    }

    #[test]
    fn fresh_orphaned_runtime_lease_keeps_startup_grace() {
        let state = test_state("orphan-grace");
        let owner = build_owner_lease("worker-a", "codex");
        let workspace = build_workspace_lease("worker-a", "codex", &state.workspace, false);
        state
            .owner_leases
            .lock()
            .unwrap()
            .insert("worker-a".to_string(), owner);
        state
            .workspace_leases
            .lock()
            .unwrap()
            .insert("worker-a".to_string(), workspace);

        let released = release_orphaned_runtime_leases(&state, &HashSet::new()).unwrap();

        assert_eq!(released, 0);
        assert!(state.owner_leases.lock().unwrap().contains_key("worker-a"));
        assert!(
            state
                .workspace_leases
                .lock()
                .unwrap()
                .contains_key("worker-a")
        );
    }

    #[test]
    fn live_session_keeps_old_recoverable_lease() {
        let state = test_state("orphan-live");
        let mut owner = build_owner_lease("worker-a", "codex");
        let old = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
        owner.acquired_at = old.clone();
        owner.last_heartbeat_at = old.clone();
        owner.renewed_at = old;
        state
            .owner_leases
            .lock()
            .unwrap()
            .insert("worker-a".to_string(), owner);
        let live = HashSet::from(["worker-a".to_string()]);

        let released = release_orphaned_runtime_leases(&state, &live).unwrap();

        assert_eq!(released, 0);
        assert!(state.owner_leases.lock().unwrap().contains_key("worker-a"));
    }
}
