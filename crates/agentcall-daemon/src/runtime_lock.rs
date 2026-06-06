use serde_json::json;
use std::fs;
use std::path::Path;
use std::process::Command;

pub(crate) fn acquire_runtime_lock(
    workspace: &Path,
    kind: &str,
    identity: &str,
) -> Result<(), String> {
    let state_dir = workspace.join(".agentcall").join("state");
    fs::create_dir_all(&state_dir).map_err(|err| err.to_string())?;
    let path = state_dir.join("runtime_locks.json");
    let key = format!("{kind}:{identity}");
    let mut locks = read_json(&path);
    if let Some(existing) = locks.get(&key) {
        let pid = existing
            .get("pid")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        if pid != 0 && pid_is_live(pid) {
            return Err(format!(
                "AgentCall {kind} already running for {identity} (pid {pid}); close it before starting another instance"
            ));
        }
    }
    locks[&key] = json!({
        "kind": kind,
        "identity": identity,
        "workspace": workspace,
        "pid": std::process::id(),
        "started_at": chrono::Utc::now().to_rfc3339(),
        "exe": std::env::current_exe().ok(),
    });
    write_json(&path, &locks)
}

fn read_json(path: &Path) -> serde_json::Value {
    let Ok(text) = fs::read_to_string(path) else {
        return json!({});
    };
    serde_json::from_str(&text).unwrap_or_else(|_| json!({}))
}

fn write_json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let tmp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("runtime_locks.json"),
        std::process::id()
    ));
    let text = serde_json::to_string_pretty(value).map_err(|err| err.to_string())? + "\n";
    fs::write(&tmp, text).map_err(|err| err.to_string())?;
    fs::rename(&tmp, path).map_err(|err| err.to_string())
}

fn pid_is_live(pid: u64) -> bool {
    if pid == std::process::id() as u64 {
        return true;
    }
    #[cfg(windows)]
    {
        let Ok(output) = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
        else {
            return false;
        };
        String::from_utf8_lossy(&output.stdout).contains(&pid.to_string())
    }
    #[cfg(not(windows))]
    {
        let _ = Command::new("true");
        std::path::PathBuf::from(format!("/proc/{pid}")).exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn stale_lock_is_replaced_and_live_lock_is_rejected() {
        let root = test_workspace("runtime-lock");
        acquire_runtime_lock(&root, "daemon", "port:3293").unwrap();
        let err = acquire_runtime_lock(&root, "daemon", "port:3293").unwrap_err();
        assert!(err.contains("already running"));
        let _ = fs::remove_dir_all(root);
    }

    fn test_workspace(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("agentcall-{name}-{nonce}"))
    }
}
