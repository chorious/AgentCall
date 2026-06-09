use crate::state::{AppState, read_json_file, write_json_file};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
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
    SharedReadonly,
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

pub(crate) fn ensure_owner_lease(
    state: &AppState,
    session_id: &str,
    owner_id: &str,
) -> Result<OwnerLease, String> {
    let mut leases = state.owner_leases.lock().unwrap();
    if let Some(existing) = leases.get(session_id) {
        if existing.owner_id != owner_id {
            return Err(format!(
                "rejected_owner_conflict: session={session_id} owner={} requested_owner={owner_id}",
                existing.owner_id
            ));
        }
        return Ok(existing.clone());
    }
    let now = chrono::Utc::now();
    let lease = OwnerLease {
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
    };
    leases.insert(session_id.to_string(), lease.clone());
    persist_owner_leases(state, &leases)?;
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

pub(crate) fn release_owner_lease(
    state: &AppState,
    session_id: &str,
    reason: &str,
) -> Result<Option<OwnerLease>, String> {
    let mut leases = state.owner_leases.lock().unwrap();
    let Some(mut lease) = leases.remove(session_id) else {
        return Ok(None);
    };
    lease.status = LeaseStatus::Released;
    lease.renewed_at = chrono::Utc::now().to_rfc3339();
    persist_owner_leases(state, &leases)?;
    crate::state::append_agent_event(
        state,
        "owner_lease.released",
        "Owner lease released.",
        json!({"session_id": session_id, "lease_id": lease.lease_id, "owner_id": lease.owner_id, "reason": reason}),
    );
    Ok(Some(lease))
}

pub(crate) fn acquire_workspace_lease(
    state: &AppState,
    session_id: &str,
    workspace: &Path,
    read_only: bool,
) -> Result<WorkspaceLease, String> {
    let mode = if read_only {
        WorkspaceLeaseMode::SharedReadonly
    } else {
        WorkspaceLeaseMode::Exclusive
    };
    let workspace_key = canonical_workspace_key(workspace);
    let mut leases = state.workspace_leases.lock().unwrap();
    for existing in leases.values() {
        if existing.workspace_key != workspace_key || existing.session_id == session_id {
            continue;
        }
        if existing.mode == WorkspaceLeaseMode::Exclusive || mode == WorkspaceLeaseMode::Exclusive {
            return Err(format!(
                "workspace_busy: workspace={} existing_session={} existing_mode={:?}",
                workspace.display(),
                existing.session_id,
                existing.mode
            ));
        }
    }
    let lease = WorkspaceLease {
        lease_id: format!("workspace-lease-{session_id}-1"),
        workspace: workspace.display().to_string(),
        workspace_key,
        mode,
        owner_id: "codex".to_string(),
        session_id: session_id.to_string(),
        expires_at: (chrono::Utc::now() + chrono::Duration::minutes(30)).to_rfc3339(),
    };
    leases.insert(session_id.to_string(), lease.clone());
    persist_workspace_leases(state, &leases)?;
    Ok(lease)
}

pub(crate) fn release_workspace_lease(
    state: &AppState,
    session_id: &str,
    reason: &str,
) -> Result<Option<WorkspaceLease>, String> {
    let mut leases = state.workspace_leases.lock().unwrap();
    let Some(lease) = leases.remove(session_id) else {
        return Ok(None);
    };
    persist_workspace_leases(state, &leases)?;
    crate::state::append_agent_event(
        state,
        "workspace_lease.released",
        "Workspace lease released.",
        json!({"session_id": session_id, "lease_id": lease.lease_id, "workspace_key": lease.workspace_key, "reason": reason}),
    );
    Ok(Some(lease))
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
}

pub(crate) fn load_workspace_leases(state: &AppState) {
    let value = read_json_file(&workspace_leases_path(state), json!({}));
    let Ok(leases) = serde_json::from_value::<HashMap<String, WorkspaceLease>>(value) else {
        return;
    };
    *state.workspace_leases.lock().unwrap() = leases;
}

pub(crate) fn owner_leases_summary(state: &AppState) -> serde_json::Value {
    let leases = state.owner_leases.lock().unwrap();
    let active = leases
        .values()
        .filter(|lease| lease.status == LeaseStatus::Active)
        .count();
    json!({
        "active": active,
        "total": leases.len(),
        "sessions": leases.keys().cloned().collect::<Vec<_>>()
    })
}

pub(crate) fn workspace_leases_summary(state: &AppState) -> serde_json::Value {
    let leases = state.workspace_leases.lock().unwrap();
    let exclusive = leases
        .values()
        .filter(|lease| lease.mode == WorkspaceLeaseMode::Exclusive)
        .count();
    let shared_readonly = leases
        .values()
        .filter(|lease| lease.mode == WorkspaceLeaseMode::SharedReadonly)
        .count();
    json!({
        "active": leases.len(),
        "exclusive": exclusive,
        "shared_readonly": shared_readonly,
        "workspaces": leases.values().map(|lease| json!({
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
    fn same_workspace_exclusive_blocks_second_writer() {
        let state = test_state("workspace-exclusive");
        let workspace = state.workspace.clone();
        let first = acquire_workspace_lease(&state, "worker-a", &workspace, false).unwrap();
        assert_eq!(first.mode, WorkspaceLeaseMode::Exclusive);

        let err = acquire_workspace_lease(&state, "worker-b", &workspace, false).unwrap_err();
        assert!(err.contains("workspace_busy"));
    }

    #[test]
    fn shared_readonly_allows_multiple_readers() {
        let state = test_state("workspace-readers");
        let workspace = state.workspace.clone();
        let first = acquire_workspace_lease(&state, "reader-a", &workspace, true).unwrap();
        let second = acquire_workspace_lease(&state, "reader-b", &workspace, true).unwrap();
        assert_eq!(first.mode, WorkspaceLeaseMode::SharedReadonly);
        assert_eq!(second.mode, WorkspaceLeaseMode::SharedReadonly);
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
}
