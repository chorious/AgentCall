use crate::state::{AppState, append_agent_event};
use crate::util::{now_ms, safe_name};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use serde::{Deserialize, Serialize};
use std::env;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const REPLAY_LIMIT: usize = 512 * 1024;

pub(crate) struct Session {
    pub(crate) name: String,
    pub(crate) command: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) master: Mutex<Box<dyn MasterPty + Send>>,
    pub(crate) writer: Mutex<Box<dyn Write + Send>>,
    pub(crate) child: Mutex<Box<dyn Child + Send>>,
    pub(crate) status: Mutex<String>,
    pub(crate) created_at: u64,
    pub(crate) updated_at: AtomicU64,
    pub(crate) replay: Mutex<Vec<u8>>,
    pub(crate) clients: Mutex<Vec<Sender<StreamEvent>>>,
}

#[derive(Clone, Serialize)]
pub(crate) struct SessionInfo {
    name: String,
    command: Vec<String>,
    cwd: String,
    status: String,
    created_at: u64,
    updated_at: u64,
    replay_bytes: usize,
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
    name: String,
    command: Vec<String>,
    cwd: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
}

#[derive(Deserialize)]
pub(crate) struct InputRequest {
    text: String,
    enter: Option<bool>,
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

    let requested_cwd = req
        .cwd
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(default_claude_workspace);
    let cwd = if is_claude_command(&req.command) {
        default_claude_workspace()
    } else {
        requested_cwd.clone()
    };
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

    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|err| err.to_string())?;
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
        writer: Mutex::new(writer),
        child: Mutex::new(child),
        status: Mutex::new("running".to_string()),
        created_at: now_ms(),
        updated_at: AtomicU64::new(now_ms()),
        replay: Mutex::new(Vec::new()),
        clients: Mutex::new(Vec::new()),
    });

    state
        .sessions
        .lock()
        .unwrap()
        .insert(session.name.clone(), Arc::clone(&session));
    append_agent_event(
        state,
        "pty.session_started",
        "PTY session started.",
        serde_json::json!({
            "name": session.name,
            "command": session.command,
            "cwd": session.cwd,
            "requested_cwd": requested_cwd,
            "cwd_policy": if is_claude_command(&session.command) { "force_claude_workspace" } else { "requested_or_default" },
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
                    session.updated_at.store(now_ms(), Ordering::Relaxed);
                    let data = String::from_utf8_lossy(bytes).to_string();
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
            serde_json::json!({"name": session.name, "status": session.status.lock().unwrap().clone(), "cwd": session.cwd}),
        );
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

pub(crate) fn default_claude_workspace() -> PathBuf {
    env::var("AGENTCALL_CLAUDE_WORKSPACE")
        .map(PathBuf::from)
        .or_else(|_| env::current_dir())
        .unwrap_or_else(|_| PathBuf::from("."))
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
        created_at: session.created_at,
        updated_at: session.updated_at.load(Ordering::Relaxed),
        replay_bytes: session.replay.lock().unwrap().len(),
    }
}

pub(crate) fn get_session(state: &AppState, name: &str) -> Option<Arc<Session>> {
    state.sessions.lock().unwrap().get(name).cloned()
}

pub(crate) fn write_input(state: &AppState, name: &str, req: InputRequest) -> Result<(), String> {
    let session = get_session(state, name).ok_or_else(|| "session not found".to_string())?;
    let enter = req.enter.unwrap_or(true);
    let mut writer = session.writer.lock().unwrap();
    let text_len = req.text.len();
    if !req.text.is_empty() {
        writer
            .write_all(req.text.as_bytes())
            .map_err(|err| err.to_string())?;
    }
    if enter {
        thread::sleep(Duration::from_millis(80));
        writer.write_all(b"\r").map_err(|err| err.to_string())?;
    }
    append_agent_event(
        state,
        "pty.input_sent",
        "Input sent to PTY session.",
        serde_json::json!({"name": name, "chars": text_len + if enter { 1 } else { 0 }, "enter": enter, "submit_split": enter}),
    );
    Ok(())
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
    session
        .child
        .lock()
        .unwrap()
        .kill()
        .map_err(|err| err.to_string())?;
    append_agent_event(
        state,
        "pty.stop_requested",
        "PTY stop requested.",
        serde_json::json!({"name": name}),
    );
    Ok(())
}

pub(crate) fn broadcast(session: &Arc<Session>, event: StreamEvent) {
    let mut clients = session.clients.lock().unwrap();
    clients.retain(|tx| tx.send(event.clone()).is_ok());
}
