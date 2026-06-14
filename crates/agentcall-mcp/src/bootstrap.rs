use crate::config::Config;
use crate::daemon_client::{daemon_get, daemon_get_with_timeout, parse_daemon_url};
use serde_json::{Value, json};
use std::env;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub(crate) fn daemon_control(config: &Config, args: Value) -> Result<Value, String> {
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status");
    let debug = args.get("debug").and_then(Value::as_bool).unwrap_or(false);
    match action {
        "status" => Ok(match daemon_get(config, "/api/runtime/health") {
            Ok(health) => json!({
                "status": "running",
                "daemon": daemon_health_for_owner(config, health, debug)
            }),
            Err(err) => json!({"status": "stopped", "daemon_url": config.daemon_url, "error": err}),
        }),
        "start" => start_daemon(config, args, debug),
        other => Err(format!("unknown daemon action: {other}")),
    }
}

fn start_daemon(config: &Config, args: Value, debug: bool) -> Result<Value, String> {
    if let Ok(health) = daemon_get(config, "/api/runtime/health") {
        return Ok(json!({
            "status": "already_running",
            "daemon": daemon_health_for_owner(config, health, debug)
        }));
    }
    let (_, port) = parse_daemon_url(&config.daemon_url)?;
    let binary = daemon_binary_path()?;
    let mut command = Command::new(&binary);
    command
        .arg("--workspace")
        .arg(&config.workspace)
        .arg("--port")
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
    let child = command
        .spawn()
        .map_err(|err| format!("failed to start daemon {}: {err}", binary.display()))?;
    let wait_seconds = args
        .get("wait_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(10)
        .min(30);
    let deadline = Instant::now() + Duration::from_secs(wait_seconds);
    let last_error = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let probe_timeout = if wait_seconds == 0 {
            Duration::from_millis(1)
        } else {
            remaining.min(Duration::from_millis(300))
        };
        match daemon_get_with_timeout(config, "/api/runtime/health", probe_timeout) {
            Ok(health) => {
                return Ok(json!({
                    "status": "started",
                    "pid": child.id(),
                    "daemon_url": config.daemon_url,
                    "binary": binary,
                    "daemon": daemon_health_for_owner(config, health, debug)
                }));
            }
            Err(err) => {
                if wait_seconds == 0 || Instant::now() >= deadline {
                    break err;
                }
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(Duration::from_millis(200)));
    };
    Ok(json!({
        "status": "starting",
        "pid": child.id(),
        "daemon_url": config.daemon_url,
        "binary": binary,
        "warning": "daemon process was spawned but health did not become ready before wait_seconds",
        "last_error": last_error
    }))
}

fn daemon_health_for_owner(config: &Config, health: Value, debug: bool) -> Value {
    if debug {
        return health;
    }
    let build = health.get("build").cloned().unwrap_or_else(|| json!({}));
    let scheduler = health
        .get("scheduler")
        .cloned()
        .unwrap_or_else(|| json!({}));
    json!({
        "status": health.get("status").cloned().unwrap_or_else(|| json!("unknown")),
        "runtime": health.get("runtime").cloned().unwrap_or_else(|| json!("agentcall-daemon")),
        "build": build,
        "workspace": health.get("workspace").cloned().unwrap_or(Value::Null),
        "claude_workspace": health.get("claude_workspace").cloned().unwrap_or(Value::Null),
        "store_backend": health.get("store_backend").cloned().unwrap_or(Value::Null),
        "hook_aware_summary": health.get("hook_aware_summary").cloned().unwrap_or(Value::Null),
        "owner": {
            "owner_id": &config.owner_id,
            "scope": "mine"
        },
        "scheduler": {
            "per_owner_max_sessions": scheduler.get("per_owner_max_sessions").cloned().unwrap_or(Value::Null),
            "queue_policy": scheduler.get("queue_policy").cloned().unwrap_or(Value::Null)
        },
        "global_debug_redacted": true,
        "debug_hint": "Pass debug=true to agentcall_daemon only when inspecting global daemon workers."
    })
}

fn daemon_binary_path() -> Result<PathBuf, String> {
    if let Ok(path) = env::var("AGENTCALL_DAEMON_BIN") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Ok(path);
        }
        return Err(format!(
            "AGENTCALL_DAEMON_BIN does not exist: {}",
            path.display()
        ));
    }
    let exe_name = if cfg!(windows) {
        "agentcall-daemon.exe"
    } else {
        "agentcall-daemon"
    };
    if let Ok(current) = env::current_exe() {
        if let Some(dir) = current.parent() {
            let sibling = dir.join(exe_name);
            if sibling.exists() {
                return Ok(sibling);
            }
        }
    }
    Ok(PathBuf::from(exe_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        Config {
            workspace: PathBuf::from("E:\\Project\\AgentCall"),
            daemon_url: "http://127.0.0.1:3293".to_string(),
            daemon_token: None,
            owner_id: "codex-owner-a".to_string(),
        }
    }

    #[test]
    fn daemon_status_default_redacts_global_worker_counts() {
        let health = json!({
            "status": "ok",
            "runtime": "agentcall-daemon",
            "active_pty_sessions": 6,
            "live_daemon_sessions": 6,
            "build": {"version": "6.8.1"},
            "scheduler": {"per_owner_max_sessions": 6, "active_sessions": 6, "queue_policy": "reject_when_owner_full"}
        });
        let value = daemon_health_for_owner(&test_config(), health, false);
        assert_eq!(value["global_debug_redacted"], true);
        assert_eq!(value["owner"]["owner_id"], "codex-owner-a");
        assert!(value.get("active_pty_sessions").is_none());
        assert!(value["scheduler"].get("active_sessions").is_none());
    }

    #[test]
    fn daemon_status_debug_returns_full_health() {
        let health = json!({
            "status": "ok",
            "active_pty_sessions": 6,
            "scheduler": {"active_sessions": 6}
        });
        let value = daemon_health_for_owner(&test_config(), health, true);
        assert_eq!(value["active_pty_sessions"], 6);
        assert_eq!(value["scheduler"]["active_sessions"], 6);
    }
}
