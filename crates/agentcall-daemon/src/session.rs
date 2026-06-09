use crate::actor::{spawn_session_actor, submit_raw_write};
use crate::hooks::cleanup_wrapper_session;
use crate::ownership::{ensure_owner_lease, release_owner_lease, release_workspace_lease};
use crate::process::ProcessHandle;
use crate::state::{AppState, append_agent_event};
use crate::terminal::{DecodeHealth, append_limited_text, decode_utf8_stream};
use crate::util::{now_ms, safe_name};
use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

const REPLAY_LIMIT: usize = 512 * 1024;

pub(crate) struct Session {
    pub(crate) name: String,
    pub(crate) command: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) master: Mutex<Box<dyn MasterPty + Send>>,
    pub(crate) child: Mutex<Box<dyn Child + Send>>,
    pub(crate) child_pid: Option<u32>,
    pub(crate) process: Mutex<ProcessHandle>,
    pub(crate) killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pub(crate) status: Mutex<String>,
    pub(crate) created_at: u64,
    pub(crate) updated_at: AtomicU64,
    pub(crate) replay: Mutex<Vec<u8>>,
    pub(crate) clean_replay: Mutex<String>,
    pub(crate) decode_health: Mutex<DecodeHealth>,
    pub(crate) clients: Mutex<Vec<Sender<StreamEvent>>>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct SessionInfo {
    pub(crate) name: String,
    pub(crate) command: Vec<String>,
    pub(crate) cwd: String,
    pub(crate) status: String,
    pub(crate) child_pid: Option<u32>,
    pub(crate) created_at: u64,
    pub(crate) updated_at: u64,
    pub(crate) replay_bytes: usize,
    pub(crate) decode_health: DecodeHealth,
}

#[derive(Clone, Serialize)]
pub(crate) struct StreamEvent {
    pub(crate) seq: u64,
    pub(crate) kind: String,
    pub(crate) data: String,
    pub(crate) status: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct StartRequest {
    pub(crate) name: String,
    pub(crate) command: Vec<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) cols: Option<u16>,
    pub(crate) rows: Option<u16>,
}

#[derive(Deserialize)]
pub(crate) struct ResizeRequest {
    cols: u16,
    rows: u16,
}

pub(crate) fn start_session(
    state: &Arc<AppState>,
    req: StartRequest,
) -> Result<SessionInfo, String> {
    if !safe_name(&req.name) {
        return Err("unsafe session name".to_string());
    }
    if req.command.is_empty() {
        return Err("missing command".to_string());
    }
    if state.sessions.lock().unwrap().contains_key(&req.name) {
        return Err("session already exists".to_string());
    }

    let requested_cwd = req.cwd.as_deref().map(PathBuf::from);
    let cwd = resolve_session_cwd(state, &req.command, requested_cwd.as_ref())?;
    if !cwd.exists() {
        return Err(format!("cwd does not exist: {}", cwd.display()));
    }
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: req.rows.unwrap_or(40),
            cols: req.cols.unwrap_or(100),
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| err.to_string())?;

    let mut command = CommandBuilder::new(&req.command[0]);
    for arg in req.command.iter().skip(1) {
        command.arg(arg);
    }
    command.cwd(&cwd);
    command.env("PYTHONUTF8", "1");
    command.env("PYTHONIOENCODING", "utf-8");
    command.env("LANG", "C.UTF-8");
    command.env("LC_ALL", "C.UTF-8");
    command.env("AGENTCALL_WRAPPER_SESSION", &req.name);

    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|err| err.to_string())?;
    let child_pid = child.process_id();
    let process = ProcessHandle::create(&req.name, child_pid);
    let killer = child.clone_killer();
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|err| err.to_string())?;
    let writer = pair.master.take_writer().map_err(|err| err.to_string())?;

    let session = Arc::new(Session {
        name: req.name.clone(),
        command: req.command,
        cwd,
        master: Mutex::new(pair.master),
        child: Mutex::new(child),
        child_pid,
        process: Mutex::new(process),
        killer: Mutex::new(killer),
        status: Mutex::new("running".to_string()),
        created_at: now_ms(),
        updated_at: AtomicU64::new(now_ms()),
        replay: Mutex::new(Vec::new()),
        clean_replay: Mutex::new(String::new()),
        decode_health: Mutex::new(DecodeHealth::default()),
        clients: Mutex::new(Vec::new()),
    });

    state
        .sessions
        .lock()
        .unwrap()
        .insert(session.name.clone(), Arc::clone(&session));
    let owner_lease = ensure_owner_lease(state, &session.name, "codex")?;
    spawn_session_actor(Arc::clone(state), Arc::clone(&session), writer);
    append_agent_event(
        state,
        "pty.session_started",
        "PTY session started.",
        serde_json::json!({
            "session_id": session.name,
            "name": session.name,
            "command": session.command,
            "cwd": session.cwd,
            "child_pid": session.child_pid,
            "process_controller": session.process.lock().unwrap().controller,
            "owner_lease_id": owner_lease.lease_id,
            "lease_generation": owner_lease.lease_generation,
            "owner_id": owner_lease.owner_id,
            "requested_cwd": requested_cwd,
            "cwd_policy": if is_claude_command(&session.command) { "force_configured_claude_workspace" } else { "requested_or_default" },
            "runtime": "pty"
        }),
    );
    spawn_reader(Arc::clone(state), Arc::clone(&session), reader);
    spawn_waiter(Arc::clone(state), Arc::clone(&session));
    Ok(session_info(&session))
}

pub(crate) fn spawn_reader(
    state: Arc<AppState>,
    session: Arc<Session>,
    mut reader: Box<dyn Read + Send>,
) {
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        let mut pending = Vec::<u8>::new();
        let mut control_tail = Vec::<u8>::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let bytes = &buf[..n];
                    {
                        let mut replay = session.replay.lock().unwrap();
                        replay.extend_from_slice(bytes);
                        if replay.len() > REPLAY_LIMIT {
                            let drop = replay.len() - REPLAY_LIMIT;
                            replay.drain(..drop);
                        }
                    }
                    let mut control_scan = control_tail.clone();
                    control_scan.extend_from_slice(bytes);
                    for _ in control_scan
                        .windows(4)
                        .filter(|window| *window == b"\x1b[6n")
                    {
                        submit_raw_write(&state, &session.name, b"\x1b[1;1R".to_vec());
                    }
                    control_tail.clear();
                    control_tail
                        .extend_from_slice(&control_scan[control_scan.len().saturating_sub(3)..]);
                    session.updated_at.store(now_ms(), Ordering::Relaxed);
                    let data = {
                        let mut health = session.decode_health.lock().unwrap();
                        decode_utf8_stream(&mut pending, bytes, &mut health)
                    };
                    if data.is_empty() {
                        continue;
                    }
                    {
                        let mut clean = session.clean_replay.lock().unwrap();
                        append_limited_text(&mut clean, &data);
                    }
                    broadcast(
                        &session,
                        StreamEvent {
                            seq: state.next_seq(),
                            kind: "output".to_string(),
                            data,
                            status: None,
                        },
                    );
                }
                Err(_) => break,
            }
        }
    });
}

pub(crate) fn spawn_waiter(state: Arc<AppState>, session: Arc<Session>) {
    thread::spawn(move || {
        let status = {
            let mut child = session.child.lock().unwrap();
            match child.wait() {
                Ok(exit) => format!("exited:{exit:?}"),
                Err(err) => format!("error:{err}"),
            }
        };
        *session.status.lock().unwrap() = status.clone();
        session.updated_at.store(now_ms(), Ordering::Relaxed);
        broadcast(
            &session,
            StreamEvent {
                seq: state.next_seq(),
                kind: "status".to_string(),
                data: String::new(),
                status: Some(status),
            },
        );
        append_agent_event(
            &state,
            "pty.session_ended",
            "PTY session ended.",
            serde_json::json!({"session_id": session.name, "name": session.name, "status": session.status.lock().unwrap().clone(), "cwd": session.cwd, "child_pid": session.child_pid}),
        );
        state.actors.lock().unwrap().remove(&session.name);
        if let Err(err) = release_owner_lease(&state, &session.name, "session_exited") {
            append_agent_event(
                &state,
                "owner_lease.release_failed",
                "Owner lease release failed.",
                serde_json::json!({"name": session.name, "reason": "session_exited", "error": err}),
            );
        }
        if let Err(err) = release_workspace_lease(&state, &session.name, "session_exited") {
            append_agent_event(
                &state,
                "workspace_lease.release_failed",
                "Workspace lease release failed.",
                serde_json::json!({"name": session.name, "reason": "session_exited", "error": err}),
            );
        }
        if let Err(err) = cleanup_wrapper_session(&state, &session.name, "session_exited") {
            append_agent_event(
                &state,
                "session.cleanup_failed",
                "Session runtime cleanup failed.",
                serde_json::json!({"name": session.name, "reason": "session_exited", "error": err}),
            );
        }
    });
}

pub(crate) fn list_sessions(state: &AppState) -> Vec<SessionInfo> {
    let mut sessions: Vec<SessionInfo> = state
        .sessions
        .lock()
        .unwrap()
        .values()
        .map(session_info)
        .collect();
    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    sessions
}

pub(crate) fn configured_claude_workspace(state: &AppState) -> Result<PathBuf, String> {
    let Some(path) = state.config.claude_workspace.clone() else {
        return Err(state.config_error.clone().unwrap_or_else(|| {
            "missing required daemon config: claude_workspace. Copy config/agentcall.example.json to config/agentcall.local.json and set claude_workspace; this cwd is required for Claude hooks/runtime binding.".to_string()
        }));
    };
    Ok(path)
}

pub(crate) fn resolve_session_cwd(
    state: &AppState,
    command: &[String],
    requested_cwd: Option<&PathBuf>,
) -> Result<PathBuf, String> {
    if is_claude_command(command) {
        return configured_claude_workspace(state);
    }
    Ok(requested_cwd
        .cloned()
        .unwrap_or_else(|| state.workspace.clone()))
}

pub(crate) fn is_claude_command(command: &[String]) -> bool {
    command
        .first()
        .map(|program| {
            let name = Path::new(program)
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(program)
                .to_ascii_lowercase();
            name == "claude" || name == "claude.exe"
        })
        .unwrap_or(false)
}

pub(crate) fn session_info(session: &Arc<Session>) -> SessionInfo {
    SessionInfo {
        name: session.name.clone(),
        command: session.command.clone(),
        cwd: session.cwd.display().to_string(),
        status: session.status.lock().unwrap().clone(),
        child_pid: session.child_pid,
        created_at: session.created_at,
        updated_at: session.updated_at.load(Ordering::Relaxed),
        replay_bytes: session.replay.lock().unwrap().len(),
        decode_health: session.decode_health.lock().unwrap().clone(),
    }
}

pub(crate) fn get_session(state: &AppState, name: &str) -> Option<Arc<Session>> {
    state.sessions.lock().unwrap().get(name).cloned()
}

pub(crate) fn resize_session(
    state: &AppState,
    name: &str,
    req: ResizeRequest,
) -> Result<(), String> {
    let session = get_session(state, name).ok_or_else(|| "session not found".to_string())?;
    session
        .master
        .lock()
        .unwrap()
        .resize(PtySize {
            rows: req.rows,
            cols: req.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| err.to_string())?;
    append_agent_event(
        state,
        "pty.resized",
        "PTY session resized.",
        serde_json::json!({"name": name, "cols": req.cols, "rows": req.rows}),
    );
    Ok(())
}

pub(crate) fn stop_session(state: &AppState, name: &str) -> Result<(), String> {
    let session = get_session(state, name).ok_or_else(|| "session not found".to_string())?;
    let process_kill = session.process.lock().unwrap().kill_tree();
    let kill_result = session.killer.lock().unwrap().kill();
    if let Err(err) = kill_result {
        if err.raw_os_error() != Some(0) {
            return Err(err.to_string());
        }
    }
    *session.status.lock().unwrap() = "stopping".to_string();
    session.updated_at.store(now_ms(), Ordering::Relaxed);
    append_agent_event(
        state,
        "pty.stop_requested",
        "PTY stop requested.",
        serde_json::json!({
            "session_id": name,
            "name": name,
            "child_pid": session.child_pid,
            "process_controller": process_kill.controller,
            "cleanup_guarantee": process_kill.cleanup_guarantee,
            "fallback_used": process_kill.fallback_used,
            "process_error": process_kill.error,
        }),
    );
    if let Err(err) = cleanup_wrapper_session(state, name, "stop_requested") {
        append_agent_event(
            state,
            "session.cleanup_failed",
            "Session runtime cleanup failed.",
            serde_json::json!({"name": name, "reason": "stop_requested", "error": err}),
        );
    }
    if let Err(err) = release_owner_lease(state, name, "stop_requested") {
        append_agent_event(
            state,
            "owner_lease.release_failed",
            "Owner lease release failed.",
            serde_json::json!({"name": name, "reason": "stop_requested", "error": err}),
        );
    }
    if let Err(err) = release_workspace_lease(state, name, "stop_requested") {
        append_agent_event(
            state,
            "workspace_lease.release_failed",
            "Workspace lease release failed.",
            serde_json::json!({"name": name, "reason": "stop_requested", "error": err}),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocalConfig;

    #[test]
    fn claude_cwd_ignores_requested_workspace_and_uses_config() {
        let workspace = PathBuf::from("E:/Project/AgentCall");
        let claude_workspace = PathBuf::from("D:/guKimi");
        let state = AppState::new(
            workspace,
            LocalConfig {
                claude_workspace: Some(claude_workspace.clone()),
                ..LocalConfig::default()
            },
            None,
        );
        let requested = PathBuf::from("E:/GameProject/GGMYS");
        let cwd = resolve_session_cwd(&state, &["claude".to_string()], Some(&requested)).unwrap();
        assert_eq!(cwd, claude_workspace);
    }

    #[test]
    fn missing_claude_workspace_rejects_claude_cwd_resolution() {
        let state = AppState::new(
            PathBuf::from("E:/Project/AgentCall"),
            LocalConfig {
                claude_workspace: None,
                ..LocalConfig::default()
            },
            Some("missing claude_workspace".to_string()),
        );
        let requested = PathBuf::from("E:/GameProject/GGMYS");
        let err =
            resolve_session_cwd(&state, &["claude".to_string()], Some(&requested)).unwrap_err();
        assert!(err.contains("missing claude_workspace"));
    }

    #[test]
    fn non_claude_cwd_uses_requested_workspace() {
        let state = AppState::test(PathBuf::from("E:/Project/AgentCall"));
        let requested = PathBuf::from("E:/GameProject/GGMYS");
        let cwd = resolve_session_cwd(&state, &["cmd".to_string()], Some(&requested)).unwrap();
        assert_eq!(cwd, requested);
    }
}

pub(crate) fn broadcast(session: &Arc<Session>, event: StreamEvent) {
    let mut clients = session.clients.lock().unwrap();
    clients.retain(|tx| tx.send(event.clone()).is_ok());
}
