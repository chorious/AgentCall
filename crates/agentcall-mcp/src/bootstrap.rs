use crate::config::Config;
use crate::daemon_client::{daemon_get, daemon_get_with_timeout, parse_daemon_url};
use crate::protocol::SERVER_VERSION;
use serde_json::{Value, json};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const RUNTIME_VERSION_FILE: &str = "agentcall-version.json";

#[derive(Clone, Debug)]
pub(crate) struct RuntimeVersionSpec {
    pub(crate) version: String,
    pub(crate) daemon_binary: PathBuf,
    pub(crate) mcp_binary: Option<PathBuf>,
    pub(crate) manifest_path: Option<PathBuf>,
}

pub(crate) fn daemon_control(config: &Config, args: Value) -> Result<Value, String> {
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("status");
    let debug = args.get("debug").and_then(Value::as_bool).unwrap_or(false);
    match action {
        "status" => Ok(match daemon_get(config, "/api/runtime/health") {
            Ok(health) => {
                ensure_daemon_matches_runtime(&health)?;
                json!({
                    "status": "running",
                    "daemon": daemon_health_for_owner(config, health, debug)
                })
            }
            Err(err) => json!({"status": "stopped", "daemon_url": config.daemon_url, "error": err}),
        }),
        "start" => start_daemon(config, args, debug),
        other => Err(format!("unknown daemon action: {other}")),
    }
}

fn start_daemon(config: &Config, args: Value, debug: bool) -> Result<Value, String> {
    if let Ok(health) = daemon_get(config, "/api/runtime/health") {
        ensure_daemon_matches_runtime(&health)?;
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
                ensure_daemon_matches_runtime(&health)?;
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

pub(crate) fn ensure_daemon_runtime(config: &Config) -> Result<Value, String> {
    let health = daemon_get(config, "/api/runtime/health")?;
    ensure_daemon_matches_runtime(&health)?;
    Ok(health)
}

pub(crate) fn ensure_daemon_runtime_with_timeout(
    config: &Config,
    timeout: Duration,
) -> Result<Value, String> {
    let health = daemon_get_with_timeout(config, "/api/runtime/health", timeout)?;
    ensure_daemon_matches_runtime(&health)?;
    Ok(health)
}

fn ensure_daemon_matches_runtime(health: &Value) -> Result<(), String> {
    let spec = runtime_version_spec()?;
    ensure_daemon_matches_spec(health, &spec)
}

fn ensure_daemon_matches_spec(health: &Value, spec: &RuntimeVersionSpec) -> Result<(), String> {
    let actual_version = health
        .pointer("/build/version")
        .and_then(Value::as_str)
        .unwrap_or("");
    let actual_binary = health
        .pointer("/build/binary_path")
        .and_then(Value::as_str)
        .unwrap_or("");
    if actual_version != spec.version {
        return Err(daemon_drift_error(
            "version_mismatch",
            &spec,
            actual_version,
            actual_binary,
        ));
    }
    if actual_binary.trim().is_empty()
        || normalize_path_text(actual_binary)
            != normalize_path_for_compare(spec.daemon_binary.as_path())
    {
        return Err(daemon_drift_error(
            "binary_path_mismatch",
            &spec,
            actual_version,
            actual_binary,
        ));
    }
    Ok(())
}

fn daemon_drift_error(
    reason: &str,
    spec: &RuntimeVersionSpec,
    actual_version: &str,
    actual_binary: &str,
) -> String {
    serde_json::to_string(&json!({
        "error": {
            "code": "daemon_version_drift",
            "category": "runtime_identity",
            "message": "AgentCall MCP rejected the daemon because its runtime identity does not match the MCP runtime manifest.",
            "details": {
                "reason": reason,
                "expected_version": spec.version,
                "actual_version": actual_version,
                "expected_daemon_binary": spec.daemon_binary,
                "actual_daemon_binary": actual_binary,
                "mcp_binary": spec.mcp_binary,
                "manifest_path": spec.manifest_path,
                "mcp_server_version": SERVER_VERSION
            }
        }
    }))
    .unwrap_or_else(|_| "daemon_version_drift".to_string())
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
    let spec = runtime_version_spec()?;
    if spec.daemon_binary.exists() {
        return Ok(spec.daemon_binary);
    }
    if spec.manifest_path.is_some() {
        return Err(format!(
            "runtime manifest daemon_binary does not exist: {}",
            spec.daemon_binary.display()
        ));
    }
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

pub(crate) fn runtime_version_spec() -> Result<RuntimeVersionSpec, String> {
    let current_exe = env::current_exe().ok();
    let manifest_path = runtime_manifest_path(current_exe.as_deref());
    if let Some(path) = manifest_path.as_ref().filter(|path| path.exists()) {
        return runtime_version_spec_from_manifest(path, current_exe.as_deref());
    }
    let daemon_binary = sibling_daemon_binary(current_exe.as_deref())
        .or_else(|| env::var("AGENTCALL_DAEMON_BIN").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(executable_name("agentcall-daemon")));
    Ok(RuntimeVersionSpec {
        version: SERVER_VERSION.to_string(),
        daemon_binary,
        mcp_binary: current_exe,
        manifest_path: None,
    })
}

fn runtime_manifest_path(current_exe: Option<&Path>) -> Option<PathBuf> {
    current_exe
        .and_then(Path::parent)
        .map(|dir| dir.join(RUNTIME_VERSION_FILE))
}

fn runtime_version_spec_from_manifest(
    path: &Path,
    current_exe: Option<&Path>,
) -> Result<RuntimeVersionSpec, String> {
    let text = fs::read_to_string(path)
        .map_err(|err| format!("failed to read runtime manifest {}: {err}", path.display()))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|err| format!("failed to parse runtime manifest {}: {err}", path.display()))?;
    let version = value
        .get("version")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("runtime manifest missing version: {}", path.display()))?
        .to_string();
    if version != SERVER_VERSION {
        return Err(serde_json::to_string(&json!({
            "error": {
                "code": "mcp_runtime_manifest_drift",
                "category": "runtime_identity",
                "message": "AgentCall MCP runtime manifest version does not match the compiled MCP server version.",
                "details": {
                    "manifest_path": path,
                    "manifest_version": version,
                    "mcp_server_version": SERVER_VERSION
                }
            }
        }))
        .unwrap_or_else(|_| "mcp_runtime_manifest_drift".to_string()));
    }
    let daemon_binary = manifest_path_field(&value, "daemon_binary")
        .or_else(|| value.pointer("/binaries/daemon").and_then(Value::as_str))
        .map(PathBuf::from)
        .or_else(|| {
            value
                .get("runtime_dir")
                .and_then(Value::as_str)
                .map(|dir| PathBuf::from(dir).join(executable_name("agentcall-daemon")))
        })
        .ok_or_else(|| format!("runtime manifest missing daemon_binary: {}", path.display()))?;
    let mcp_binary = manifest_path_field(&value, "mcp_binary")
        .or_else(|| value.pointer("/binaries/mcp").and_then(Value::as_str))
        .map(PathBuf::from)
        .or_else(|| current_exe.map(Path::to_path_buf));
    Ok(RuntimeVersionSpec {
        version,
        daemon_binary,
        mcp_binary,
        manifest_path: Some(path.to_path_buf()),
    })
}

fn manifest_path_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

fn sibling_daemon_binary(current_exe: Option<&Path>) -> Option<PathBuf> {
    current_exe.and_then(|current| {
        current.parent().map(|dir| {
            let sibling = dir.join(executable_name("agentcall-daemon"));
            if sibling.exists() {
                sibling
            } else {
                PathBuf::from(executable_name("agentcall-daemon"))
            }
        })
    })
}

fn executable_name(name: &str) -> &'static str {
    match (cfg!(windows), name) {
        (true, "agentcall-daemon") => "agentcall-daemon.exe",
        (true, "agentcall-mcp") => "agentcall-mcp.exe",
        (true, "agentcall-hook") => "agentcall-hook.exe",
        (_, "agentcall-daemon") => "agentcall-daemon",
        (_, "agentcall-mcp") => "agentcall-mcp",
        (_, "agentcall-hook") => "agentcall-hook",
        _ => "agentcall-daemon",
    }
}

fn normalize_path_for_compare(path: &Path) -> String {
    let text = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string();
    normalize_path_text(&text)
}

fn normalize_path_text(text: &str) -> String {
    let mut value = text.trim().replace('\\', "/");
    if let Some(stripped) = value.strip_prefix("//?/") {
        value = stripped.to_string();
    }
    while value.ends_with('/') {
        value.pop();
    }
    if cfg!(windows) {
        value = value.to_ascii_lowercase();
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn runtime_identity_error_code(err: &str) -> Option<String> {
        serde_json::from_str::<Value>(err).ok().and_then(|value| {
            value
                .pointer("/error/code")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
    }

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
            "build": {"version": "6.8.2"},
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

    #[test]
    fn daemon_runtime_identity_accepts_matching_manifest_spec() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-runtime-identity-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let daemon = root.join(executable_name("agentcall-daemon"));
        fs::write(&daemon, "").unwrap();
        let spec = RuntimeVersionSpec {
            version: SERVER_VERSION.to_string(),
            daemon_binary: daemon.clone(),
            mcp_binary: Some(root.join(executable_name("agentcall-mcp"))),
            manifest_path: Some(root.join(RUNTIME_VERSION_FILE)),
        };
        let health = json!({
            "build": {
                "version": SERVER_VERSION,
                "binary_path": daemon.display().to_string()
            }
        });
        assert!(ensure_daemon_matches_spec(&health, &spec).is_ok());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn daemon_runtime_identity_rejects_stale_daemon_version() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-runtime-version-drift-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let daemon = root.join(executable_name("agentcall-daemon"));
        fs::write(&daemon, "").unwrap();
        let spec = RuntimeVersionSpec {
            version: SERVER_VERSION.to_string(),
            daemon_binary: daemon.clone(),
            mcp_binary: None,
            manifest_path: Some(root.join(RUNTIME_VERSION_FILE)),
        };
        let health = json!({
            "build": {
                "version": "6.9.0",
                "binary_path": daemon.display().to_string()
            }
        });
        let err = ensure_daemon_matches_spec(&health, &spec).unwrap_err();
        assert_eq!(
            runtime_identity_error_code(&err).as_deref(),
            Some("daemon_version_drift")
        );
        assert!(err.contains("version_mismatch"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn daemon_runtime_identity_rejects_wrong_daemon_binary() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-runtime-binary-drift-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let expected = root.join(executable_name("agentcall-daemon"));
        let actual = root.join("old").join(executable_name("agentcall-daemon"));
        fs::create_dir_all(actual.parent().unwrap()).unwrap();
        fs::write(&expected, "").unwrap();
        fs::write(&actual, "").unwrap();
        let spec = RuntimeVersionSpec {
            version: SERVER_VERSION.to_string(),
            daemon_binary: expected,
            mcp_binary: None,
            manifest_path: Some(root.join(RUNTIME_VERSION_FILE)),
        };
        let health = json!({
            "build": {
                "version": SERVER_VERSION,
                "binary_path": actual.display().to_string()
            }
        });
        let err = ensure_daemon_matches_spec(&health, &spec).unwrap_err();
        assert_eq!(
            runtime_identity_error_code(&err).as_deref(),
            Some("daemon_version_drift")
        );
        assert!(err.contains("binary_path_mismatch"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_manifest_rejects_mcp_version_drift() {
        let root = std::env::temp_dir().join(format!(
            "agentcall-mcp-manifest-drift-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let manifest = root.join(RUNTIME_VERSION_FILE);
        fs::write(
            &manifest,
            serde_json::to_string(&json!({
                "version": "0.0.0",
                "daemon_binary": root.join(executable_name("agentcall-daemon")).display().to_string(),
                "mcp_binary": root.join(executable_name("agentcall-mcp")).display().to_string()
            }))
            .unwrap(),
        )
        .unwrap();
        let err = runtime_version_spec_from_manifest(&manifest, None).unwrap_err();
        assert_eq!(
            runtime_identity_error_code(&err).as_deref(),
            Some("mcp_runtime_manifest_drift")
        );
        let _ = fs::remove_dir_all(root);
    }
}
