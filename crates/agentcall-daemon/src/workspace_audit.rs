use crate::crypto::sha256_hex;
use crate::state::{AppState, append_agent_event_locked, read_json_file, write_json_file};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const AUDIT_STATE_FILE: &str = "workspace_audit.json";
const POLICY_DENIALS_FILE: &str = "policy_denials.json";
const MAX_SCAN_DIRS: usize = 4096;
const MAX_CHANGED_DIRS: usize = 64;
const MAX_SCAN_FILES: usize = 50_000;

const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".agentcall",
    ".agents",
    ".claude",
    ".codex",
    ".venv",
    "venv",
    "node_modules",
    "target",
    "dist",
    "build",
    "__pycache__",
];

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceAuditRoot {
    pub(crate) kind: String,
    pub(crate) abs: String,
}

#[derive(Clone, Debug)]
pub(crate) struct WorkspaceAuditPolicy {
    pub(crate) mode: String,
    pub(crate) target_workspace: Option<String>,
    pub(crate) scratch_root: Option<String>,
    pub(crate) writable_roots: Vec<WorkspaceAuditRoot>,
}

#[derive(Default)]
struct DirectorySignature {
    files: u64,
    dirs: u64,
    bytes: u64,
    max_modified_ms: u64,
}

#[derive(Default)]
struct ScanStats {
    dirs_seen: usize,
    files_seen: usize,
    overflow: bool,
}

struct ScanResult {
    snapshot: BTreeMap<String, String>,
    overflow: bool,
    dirs_seen: usize,
    files_seen: usize,
}

pub(crate) fn policy_from_containment(containment: &Value) -> WorkspaceAuditPolicy {
    let roots = containment.get("roots");
    WorkspaceAuditPolicy {
        mode: containment
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("enforced")
            .to_string(),
        target_workspace: roots
            .and_then(|roots| roots.get("target_workspace"))
            .and_then(Value::as_str)
            .map(str::to_string),
        scratch_root: roots
            .and_then(|roots| roots.get("scratch_root"))
            .and_then(Value::as_str)
            .or_else(|| containment.get("scratch_root").and_then(Value::as_str))
            .map(str::to_string),
        writable_roots: containment
            .get("writable_roots")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let abs = item.get("abs").and_then(Value::as_str)?;
                        Some(WorkspaceAuditRoot {
                            kind: item
                                .get("kind")
                                .and_then(Value::as_str)
                                .unwrap_or("writable_root")
                                .to_string(),
                            abs: abs.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

pub(crate) fn initialize_session_audit(
    state: &AppState,
    wrapper_session: &str,
    containment: &Value,
) -> Result<Value, String> {
    let policy = policy_from_containment(containment);
    let Some(target_workspace) = policy.target_workspace.as_deref() else {
        return Ok(json!({"status": "skipped", "reason": "missing target workspace"}));
    };
    let scan = scan_workspace(target_workspace);
    let _guard = state.state_writer.lock().unwrap();
    let agent_dir = state.workspace.join(".agentcall");
    let state_dir = agent_dir.join("state");
    fs::create_dir_all(&state_dir).map_err(|err| err.to_string())?;
    let path = audit_state_path(&state_dir);
    let mut audits = read_json_file(&path, json!({}));
    if !audits.is_object() {
        audits = json!({});
    }
    let now = chrono::Utc::now().to_rfc3339();
    audits[wrapper_session] = json!({
        "schema_version": 1,
        "session": wrapper_session,
        "status": if scan.overflow { "baseline_overflow" } else { "clean" },
        "mode": policy.mode.clone(),
        "target_workspace": target_workspace,
        "scratch_root": policy.scratch_root.clone(),
        "allowed_dirs": allowed_dirs_for_policy(&policy, &[]),
        "approved_dirs": [],
        "baseline": scan.snapshot,
        "last_heartbeat": {
            "seq": 0,
            "timestamp": now,
            "changed_dirs": [],
            "blocked_dirs": [],
            "overflow": scan.overflow,
            "dirs_seen": scan.dirs_seen,
            "files_seen": scan.files_seen
        },
        "active_block": Value::Null
    });
    write_json_file(&path, &audits)?;
    Ok(json!({
        "status": audits[wrapper_session]["status"].clone(),
        "overflow": scan.overflow,
        "dirs_seen": scan.dirs_seen,
        "files_seen": scan.files_seen
    }))
}

pub(crate) fn observe_post_tool_heartbeat_locked(
    state: &AppState,
    state_dir: &Path,
    wrapper_session: &str,
    tool_name: &str,
    policy: &WorkspaceAuditPolicy,
) -> Result<Value, String> {
    let Some(target_workspace) = policy.target_workspace.as_deref() else {
        return Ok(json!({"status": "skipped", "reason": "missing target workspace"}));
    };
    let path = audit_state_path(state_dir);
    let mut audits = read_json_file(&path, json!({}));
    if !audits.is_object() {
        audits = json!({});
    }
    let previous = audits
        .get(wrapper_session)
        .cloned()
        .unwrap_or_else(|| json!({}));
    let previous_snapshot = snapshot_from_value(previous.get("baseline"));
    let scan = scan_workspace(target_workspace);
    let now = chrono::Utc::now().to_rfc3339();
    let approved_dirs = string_array(previous.get("approved_dirs"));
    let allowed_dirs = allowed_dirs_for_policy(policy, &approved_dirs);
    let changed_dirs = if previous_snapshot.is_empty() {
        Vec::new()
    } else {
        changed_dirs_between(&previous_snapshot, &scan.snapshot)
    };
    let blocked_dirs: Vec<String> = changed_dirs
        .iter()
        .filter(|dir| !dir_allowed(dir, &allowed_dirs))
        .cloned()
        .collect();
    let changed_dirs = cap_dirs(changed_dirs);
    let blocked_dirs = cap_dirs(blocked_dirs);
    let seq = previous
        .pointer("/last_heartbeat/seq")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + 1;
    let status = if scan.overflow {
        "overflow_attention"
    } else if blocked_dirs.is_empty() {
        "clean"
    } else {
        "blocked_by_policy"
    };
    let mut entry = json!({
        "schema_version": 1,
        "session": wrapper_session,
        "status": status,
        "mode": policy.mode.clone(),
        "target_workspace": target_workspace,
        "scratch_root": policy.scratch_root.clone(),
        "allowed_dirs": allowed_dirs,
        "approved_dirs": approved_dirs,
        "baseline": scan.snapshot,
        "last_heartbeat": {
            "seq": seq,
            "timestamp": now,
            "tool": tool_name,
            "changed_dirs": changed_dirs,
            "blocked_dirs": blocked_dirs,
            "overflow": scan.overflow,
            "dirs_seen": scan.dirs_seen,
            "files_seen": scan.files_seen
        },
        "active_block": Value::Null
    });
    if status == "blocked_by_policy" {
        let block = workspace_audit_policy_block(wrapper_session, &entry);
        entry["active_block"] = block.clone();
        record_workspace_audit_policy_block_locked(state, state_dir, wrapper_session, &block)?;
    }
    audits[wrapper_session] = entry.clone();
    write_json_file(&path, &audits)?;
    Ok(json!({
        "status": status,
        "heartbeat": entry["last_heartbeat"].clone(),
        "active_block": entry["active_block"].clone()
    }))
}

pub(crate) fn active_workspace_audit_block_locked(
    state_dir: &Path,
    wrapper_session: &str,
) -> Option<Value> {
    let denials = read_json_file(&state_dir.join(POLICY_DENIALS_FILE), json!({}));
    let block = denials.get(wrapper_session)?;
    if block.get("active").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    if block.get("category").and_then(Value::as_str) != Some("workspace_audit_changed_dir") {
        return None;
    }
    Some(block.clone())
}

pub(crate) fn approve_changed_dir(
    state: &AppState,
    wrapper_session: &str,
    dir: &str,
    owner_id: Option<&str>,
    reason: Option<&str>,
) -> Result<Value, String> {
    let _guard = state.state_writer.lock().unwrap();
    let agent_dir = state.workspace.join(".agentcall");
    let state_dir = agent_dir.join("state");
    fs::create_dir_all(&state_dir).map_err(|err| err.to_string())?;
    let path = audit_state_path(&state_dir);
    let mut audits = read_json_file(&path, json!({}));
    if !audits.is_object() {
        audits = json!({});
    }
    let mut entry = audits
        .get(wrapper_session)
        .cloned()
        .unwrap_or_else(|| json!({"session": wrapper_session}));
    let approved_dir = normalize_compare_path(dir);
    let mut approved_dirs = string_array(entry.get("approved_dirs"));
    if !approved_dirs
        .iter()
        .any(|item| paths_equal_for_compare(item, &approved_dir))
    {
        approved_dirs.push(approved_dir.clone());
    }
    entry["approved_dirs"] = json!(approved_dirs);
    entry["status"] = json!("approved");
    entry["active_block"] = Value::Null;
    entry["last_approval"] = json!({
        "dir": approved_dir,
        "owner_id": owner_id,
        "reason": reason,
        "approved_at": chrono::Utc::now().to_rfc3339()
    });
    audits[wrapper_session] = entry.clone();
    write_json_file(&path, &audits)?;
    clear_workspace_audit_policy_block_locked(&state_dir, wrapper_session)?;
    append_agent_event_locked(
        state,
        &agent_dir,
        "workspace_audit.approved",
        "Workspace audit changed directory was approved for this session.",
        json!({
            "wrapper_session": wrapper_session,
            "dir": approved_dir,
            "owner_id": owner_id,
            "reason": reason
        }),
    )?;
    Ok(json!({
        "ok": true,
        "status": "changed_dir_approved",
        "session": wrapper_session,
        "dir": approved_dir,
        "scope": "session",
        "workspace_audit": entry
    }))
}

fn record_workspace_audit_policy_block_locked(
    state: &AppState,
    state_dir: &Path,
    wrapper_session: &str,
    block: &Value,
) -> Result<(), String> {
    let path = state_dir.join(POLICY_DENIALS_FILE);
    let mut denials = read_json_file(&path, json!({}));
    if !denials.is_object() {
        denials = json!({});
    }
    let previous = denials.get(wrapper_session).cloned().unwrap_or(Value::Null);
    let should_emit = previous.get("key") != block.get("key")
        || previous.get("active").and_then(Value::as_bool) != Some(true);
    denials[wrapper_session] = block.clone();
    write_json_file(&path, &denials)?;
    if should_emit {
        append_agent_event_locked(
            state,
            state_dir
                .parent()
                .unwrap_or_else(|| Path::new(".agentcall")),
            "workspace_audit.policy_blocked",
            "Workspace audit observed changed folders outside approved write areas.",
            json!({
                "wrapper_session": wrapper_session,
                "policy_block": block
            }),
        )?;
    }
    Ok(())
}

fn clear_workspace_audit_policy_block_locked(
    state_dir: &Path,
    wrapper_session: &str,
) -> Result<(), String> {
    let path = state_dir.join(POLICY_DENIALS_FILE);
    let mut denials = read_json_file(&path, json!({}));
    if let Some(object) = denials.as_object_mut() {
        let should_remove = object
            .get(wrapper_session)
            .and_then(|block| block.get("category"))
            .and_then(Value::as_str)
            == Some("workspace_audit_changed_dir");
        if should_remove {
            object.remove(wrapper_session);
        }
    }
    write_json_file(&path, &denials)
}

fn workspace_audit_policy_block(wrapper_session: &str, entry: &Value) -> Value {
    let heartbeat = entry.get("last_heartbeat").cloned().unwrap_or(Value::Null);
    let blocked_dirs = string_array(heartbeat.get("blocked_dirs"));
    let target = blocked_dirs
        .first()
        .cloned()
        .unwrap_or_else(|| "<unknown changed directory>".to_string());
    let key_material = format!(
        "{}:{}",
        wrapper_session,
        serde_json::to_string(&blocked_dirs).unwrap_or_default()
    );
    json!({
        "active": true,
        "key": format!("workspace-audit:{}", sha256_hex(&key_material)),
        "wrapper_session": wrapper_session,
        "tool": "workspace_audit",
        "tool_name": "workspace_audit",
        "target": target,
        "reason": "workspace audit observed changed folders outside allowed write areas",
        "repeat_count": 1,
        "recent_denial_count": 1,
        "recent_denials": [],
        "window_seconds": 0,
        "threshold": 1,
        "category": "workspace_audit_changed_dir",
        "recommended_action": "approve_changed_dir_or_interrupt",
        "allowed_actions": blocked_dirs.iter().map(|dir| {
            json!({
                "kind": "approve_changed_dir",
                "tool": "agentcall_session_send",
                "args": {
                    "name": wrapper_session,
                    "action": "approve_changed_dir",
                    "dir": dir
                }
            })
        }).collect::<Vec<Value>>(),
        "path_diagnosis": {
            "target_workspace": entry.get("target_workspace").cloned().unwrap_or(Value::Null),
            "scratch_root": entry.get("scratch_root").cloned().unwrap_or(Value::Null),
            "changed_dirs": heartbeat.get("changed_dirs").cloned().unwrap_or_else(|| json!([])),
            "blocked_dirs": heartbeat.get("blocked_dirs").cloned().unwrap_or_else(|| json!([])),
            "allowed_dirs": entry.get("allowed_dirs").cloned().unwrap_or_else(|| json!([])),
            "approved_dirs": entry.get("approved_dirs").cloned().unwrap_or_else(|| json!([])),
            "overflow": heartbeat.get("overflow").cloned().unwrap_or_else(|| json!(false))
        },
        "last_seen": heartbeat.get("timestamp").cloned().unwrap_or(Value::Null),
        "policy": {
            "mode": entry.get("mode").cloned().unwrap_or_else(|| json!("workspace_audit")),
            "audit": "folder_heartbeat"
        }
    })
}

fn audit_state_path(state_dir: &Path) -> PathBuf {
    state_dir.join(AUDIT_STATE_FILE)
}

fn snapshot_from_value(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn scan_workspace(root: &str) -> ScanResult {
    let root = PathBuf::from(root);
    let mut snapshot = BTreeMap::new();
    let mut stats = ScanStats::default();
    if root.exists() {
        scan_dir(&root, &mut snapshot, &mut stats);
    }
    ScanResult {
        snapshot,
        overflow: stats.overflow,
        dirs_seen: stats.dirs_seen,
        files_seen: stats.files_seen,
    }
}

fn scan_dir(
    dir: &Path,
    snapshot: &mut BTreeMap<String, String>,
    stats: &mut ScanStats,
) -> DirectorySignature {
    if stats.dirs_seen >= MAX_SCAN_DIRS || stats.files_seen >= MAX_SCAN_FILES {
        stats.overflow = true;
        return DirectorySignature::default();
    }
    stats.dirs_seen += 1;
    let mut sig = DirectorySignature::default();
    let Ok(entries) = fs::read_dir(dir) else {
        return sig;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            if should_ignore_dir(&name) {
                continue;
            }
            sig.dirs = sig.dirs.saturating_add(1);
            let _ = scan_dir(&path, snapshot, stats);
        } else if metadata.is_file() {
            stats.files_seen += 1;
            sig.files = sig.files.saturating_add(1);
            sig.bytes = sig.bytes.saturating_add(metadata.len());
            if let Ok(modified) = metadata.modified() {
                if let Ok(duration) = modified.duration_since(UNIX_EPOCH) {
                    sig.max_modified_ms = sig
                        .max_modified_ms
                        .max(duration.as_millis().min(u128::from(u64::MAX)) as u64);
                }
            }
            if stats.files_seen >= MAX_SCAN_FILES {
                stats.overflow = true;
                break;
            }
        }
    }
    snapshot.insert(
        normalize_compare_path(&dir.display().to_string()),
        format!(
            "f:{};d:{};b:{};m:{}",
            sig.files, sig.dirs, sig.bytes, sig.max_modified_ms
        ),
    );
    sig
}

fn should_ignore_dir(name: &str) -> bool {
    IGNORED_DIRS
        .iter()
        .any(|ignored| name.eq_ignore_ascii_case(ignored))
}

fn changed_dirs_between(
    previous: &BTreeMap<String, String>,
    current: &BTreeMap<String, String>,
) -> Vec<String> {
    let keys: BTreeSet<String> = previous.keys().chain(current.keys()).cloned().collect();
    let changed: Vec<String> = keys
        .into_iter()
        .filter(|dir| previous.get(dir) != current.get(dir))
        .collect();
    collapse_dirs(changed)
}

fn collapse_dirs(mut dirs: Vec<String>) -> Vec<String> {
    dirs.sort_by_key(|dir| dir.len());
    let mut collapsed: Vec<String> = Vec::new();
    'next: for dir in dirs {
        for kept in &collapsed {
            if path_within_or_equal(&dir, kept) {
                continue 'next;
            }
        }
        collapsed.push(dir);
        if collapsed.len() >= MAX_CHANGED_DIRS {
            break;
        }
    }
    collapsed
}

fn cap_dirs(mut dirs: Vec<String>) -> Vec<String> {
    if dirs.len() > MAX_CHANGED_DIRS {
        dirs.truncate(MAX_CHANGED_DIRS);
    }
    dirs
}

fn allowed_dirs_for_policy(policy: &WorkspaceAuditPolicy, approved_dirs: &[String]) -> Vec<String> {
    let mut dirs = Vec::new();
    if let Some(scratch_root) = policy.scratch_root.as_ref() {
        push_unique_dir(&mut dirs, scratch_root);
    }
    for root in &policy.writable_roots {
        let allowed = if root.kind == "report" || looks_like_file_path(&root.abs) {
            PathBuf::from(&root.abs)
                .parent()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| root.abs.clone())
        } else {
            root.abs.clone()
        };
        push_unique_dir(&mut dirs, &allowed);
    }
    for dir in approved_dirs {
        push_unique_dir(&mut dirs, dir);
    }
    dirs
}

fn push_unique_dir(dirs: &mut Vec<String>, dir: &str) {
    let normalized = normalize_compare_path(dir);
    if !dirs
        .iter()
        .any(|item| paths_equal_for_compare(item, &normalized))
    {
        dirs.push(normalized);
    }
}

fn dir_allowed(dir: &str, allowed_dirs: &[String]) -> bool {
    allowed_dirs
        .iter()
        .any(|allowed| path_within_or_equal(dir, allowed))
}

fn looks_like_file_path(path: &str) -> bool {
    PathBuf::from(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains('.'))
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn normalize_compare_path(path: &str) -> String {
    let replaced = path.replace('\\', "/");
    let lowered = if cfg!(windows) {
        replaced.to_ascii_lowercase()
    } else {
        replaced
    };
    if lowered.ends_with('/') && lowered.len() > 1 {
        lowered.trim_end_matches('/').to_string()
    } else {
        lowered
    }
}

fn path_within_or_equal(path: &str, root: &str) -> bool {
    let path = normalize_compare_path(path);
    let root = normalize_compare_path(root);
    path == root || path.starts_with(&format!("{root}/"))
}

fn paths_equal_for_compare(left: &str, right: &str) -> bool {
    normalize_compare_path(left) == normalize_compare_path(right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use crate::util::now_ms;
    use std::env;
    use std::sync::Arc;

    fn test_state(name: &str) -> Arc<AppState> {
        let root = env::temp_dir().join(format!(
            "agentcall-daemon-audit-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".agentcall").join("state")).unwrap();
        Arc::new(AppState::test(root))
    }

    #[test]
    fn folder_heartbeat_blocks_unapproved_target_dir() {
        let state = test_state("block-unapproved");
        let target = state.workspace.join("target");
        fs::create_dir_all(target.join("src")).unwrap();
        fs::write(target.join("src").join("existing.txt"), "old").unwrap();
        let containment = json!({
            "mode": "report",
            "roots": {
                "target_workspace": target.display().to_string(),
                "scratch_root": state.workspace.join(".agentcall/workspaces/a").display().to_string()
            },
            "writable_roots": [{
                "kind": "scratch",
                "display": ".agentcall/workspaces/a",
                "abs": state.workspace.join(".agentcall/workspaces/a").display().to_string()
            }]
        });
        initialize_session_audit(&state, "worker-a", &containment).unwrap();
        fs::write(target.join("src").join("generated.txt"), "new").unwrap();
        let state_dir = state.workspace.join(".agentcall").join("state");
        let heartbeat = observe_post_tool_heartbeat_locked(
            &state,
            &state_dir,
            "worker-a",
            "Bash",
            &policy_from_containment(&containment),
        )
        .unwrap();
        assert_eq!(heartbeat["status"], "blocked_by_policy");
        let block = active_workspace_audit_block_locked(&state_dir, "worker-a").unwrap();
        assert_eq!(block["category"], "workspace_audit_changed_dir");
        assert!(
            block["path_diagnosis"]["blocked_dirs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|dir| dir.as_str().unwrap().ends_with("/src"))
        );
    }

    #[test]
    fn approve_changed_dir_clears_workspace_audit_block() {
        let state = test_state("approve-dir");
        let target = state.workspace.join("target");
        fs::create_dir_all(target.join("src")).unwrap();
        let containment = json!({
            "mode": "report",
            "roots": {
                "target_workspace": target.display().to_string(),
                "scratch_root": state.workspace.join(".agentcall/workspaces/a").display().to_string()
            },
            "writable_roots": []
        });
        initialize_session_audit(&state, "worker-a", &containment).unwrap();
        fs::write(target.join("src").join("generated.txt"), "new").unwrap();
        let state_dir = state.workspace.join(".agentcall").join("state");
        observe_post_tool_heartbeat_locked(
            &state,
            &state_dir,
            "worker-a",
            "Bash",
            &policy_from_containment(&containment),
        )
        .unwrap();
        let dir = target.join("src").display().to_string();
        let approval = approve_changed_dir(
            &state,
            "worker-a",
            &dir,
            Some("codex"),
            Some("expected cache"),
        )
        .unwrap();
        assert_eq!(approval["status"], "changed_dir_approved");
        assert!(active_workspace_audit_block_locked(&state_dir, "worker-a").is_none());
    }
}
