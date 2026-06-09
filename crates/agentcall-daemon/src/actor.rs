use crate::commands::{CommandEnvelopeV1, CommandType};
use crate::hooks::queue_supervisor_instruction;
use crate::session::{Session, stop_session};
use crate::state::{AppState, append_agent_event};
use serde_json::{Value, json};
use std::io::Write;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

pub(crate) struct PtyWriter {
    inner: Box<dyn Write + Send>,
}

impl PtyWriter {
    pub(crate) fn new(inner: Box<dyn Write + Send>) -> Self {
        Self { inner }
    }

    fn write_input(&mut self, text: &str, enter: bool) -> Result<(), String> {
        if !text.is_empty() {
            self.inner
                .write_all(text.as_bytes())
                .map_err(|err| err.to_string())?;
        }
        if enter {
            thread::sleep(Duration::from_millis(80));
            self.inner.write_all(b"\r").map_err(|err| err.to_string())?;
        }
        self.inner.flush().map_err(|err| err.to_string())
    }

    fn write_raw(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.inner.write_all(bytes).map_err(|err| err.to_string())?;
        self.inner.flush().map_err(|err| err.to_string())
    }
}

#[derive(Clone)]
pub(crate) struct ActorHandle {
    pub(crate) session_id: String,
    pub(crate) sender: Sender<ActorControlCommand>,
}

pub(crate) enum ActorControlCommand {
    Submit(CommandEnvelopeV1, Sender<Result<Value, String>>),
    RawWrite(Vec<u8>),
    RefreshProjection,
}

pub(crate) fn spawn_session_actor(
    state: Arc<AppState>,
    session: Arc<Session>,
    writer: Box<dyn Write + Send>,
) {
    let (sender, receiver) = mpsc::channel::<ActorControlCommand>();
    let handle = ActorHandle {
        session_id: session.name.clone(),
        sender,
    };
    state
        .actors
        .lock()
        .unwrap()
        .insert(session.name.clone(), handle);
    thread::spawn(move || {
        session_actor_loop(
            state,
            session.name.clone(),
            PtyWriter::new(writer),
            receiver,
        )
    });
}

pub(crate) fn submit_session_command(
    state: &AppState,
    session_id: &str,
    command: CommandEnvelopeV1,
) -> Result<Value, String> {
    let handle = state
        .actors
        .lock()
        .unwrap()
        .get(session_id)
        .cloned()
        .ok_or_else(|| {
            "missing session actor; session is orphaned or not actor-managed".to_string()
        })?;
    if handle.session_id != session_id {
        return Err(format!(
            "actor registry mismatch: requested {session_id}, handle owns {}",
            handle.session_id
        ));
    }
    let (reply_tx, reply_rx) = mpsc::channel();
    handle
        .sender
        .send(ActorControlCommand::Submit(command, reply_tx))
        .map_err(|err| format!("failed to submit command to session actor: {err}"))?;
    reply_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|err| format!("session actor command timeout: {err}"))?
}

pub(crate) fn submit_raw_write(state: &AppState, session_id: &str, bytes: Vec<u8>) {
    if let Some(handle) = state.actors.lock().unwrap().get(session_id).cloned() {
        let _ = handle.sender.send(ActorControlCommand::RawWrite(bytes));
    }
}

fn session_actor_loop(
    state: Arc<AppState>,
    session_id: String,
    mut writer: PtyWriter,
    receiver: Receiver<ActorControlCommand>,
) {
    for command in receiver {
        match command {
            ActorControlCommand::Submit(envelope, reply) => {
                let result = execute_command(&state, &session_id, &envelope, &mut writer);
                let _ = reply.send(result);
            }
            ActorControlCommand::RawWrite(bytes) => {
                let _ = writer.write_raw(&bytes);
            }
            ActorControlCommand::RefreshProjection => {}
        }
    }
}

fn execute_command(
    state: &AppState,
    session_id: &str,
    command: &CommandEnvelopeV1,
    writer: &mut PtyWriter,
) -> Result<Value, String> {
    append_agent_event(
        state,
        "command.accepted",
        "Session actor accepted command.",
        json!({
            "session_id": session_id,
            "command_id": command.command_id,
            "idempotency_key": command.idempotency_key,
            "command_type": format!("{:?}", command.command_type),
            "owner_id": command.owner_id,
            "owner_lease_id": command.owner_lease_id,
            "lease_generation": command.lease_generation,
        }),
    );
    let result = match command.command_type {
        CommandType::StopSession | CommandType::KillSession => {
            stop_session(state, session_id)?;
            json!({
                "ok": true,
                "status": "stop_signal_sent",
                "awaiting_observation": true,
                "next_required_observation": "process_exited_or_session_ended"
            })
        }
        CommandType::InterruptTurn => {
            let redirect_text = command
                .payload
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string);
            writer.write_raw(b"\x1b")?;
            append_agent_event(
                state,
                "pty.interrupt_sent",
                "Interrupt sent to PTY session.",
                json!({
                    "name": session_id,
                    "control": "esc",
                    "warning": "Use interrupt only when the worker is drifting, doing the wrong thing, or must be reclaimed immediately."
                }),
            );
            if let Some(text) = redirect_text.filter(|value| !value.trim().is_empty()) {
                thread::sleep(Duration::from_millis(250));
                actor_write_input(state, session_id, writer, &text, true)?;
            }
            json!({
                "ok": true,
                "action": "interrupt",
                "status": "interrupt_sent",
                "awaiting_observation": true,
                "next_required_observation": "hook_idle_or_ready_prompt",
                "warning": "Use interrupt only when the worker is drifting, doing the wrong thing, or must be reclaimed immediately."
            })
        }
        CommandType::QueueSupervisorInstruction => {
            let action = command
                .payload
                .get("action")
                .and_then(Value::as_str)
                .unwrap_or("send");
            let text = command
                .payload
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("");
            let queued = queue_supervisor_instruction(state, session_id, action, text)?;
            json!({
                "ok": true,
                "status": "queued_until_next_hook_injection",
                "delivery": "PostToolBatch_or_next_context_hook",
                "instruction": queued,
                "hint": "Claude Code does not reliably accept new prompts mid-turn. AgentCall queued this instruction for hook additionalContext instead of blindly typing into the PTY."
            })
        }
        CommandType::SelectOption | CommandType::SendInput | CommandType::RequestReport => {
            let text = command
                .payload
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let enter = command.payload.get("enter").and_then(Value::as_bool);
            actor_write_input(state, session_id, writer, &text, enter.unwrap_or(true))?;
            json!({"ok": true, "status": "input_sent_by_actor"})
        }
        CommandType::CancelCommand | CommandType::RefreshProjection => {
            json!({"ok": true, "status": "command_acknowledged"})
        }
    };
    let (event_type, message, awaiting_observation) = command_terminal_event(&command.command_type);
    append_agent_event(
        state,
        event_type,
        message,
        json!({
            "session_id": session_id,
            "command_id": command.command_id,
            "idempotency_key": command.idempotency_key,
            "command_type": format!("{:?}", command.command_type),
            "awaiting_observation": awaiting_observation,
        }),
    );
    Ok(result)
}

fn command_terminal_event(command_type: &CommandType) -> (&'static str, &'static str, bool) {
    match command_type {
        CommandType::InterruptTurn | CommandType::StopSession | CommandType::KillSession => (
            "command.awaiting_observation",
            "Session actor dispatched command and is waiting for observed worker state.",
            true,
        ),
        _ => (
            "command.completed",
            "Session actor completed command dispatch.",
            false,
        ),
    }
}

fn actor_write_input(
    state: &AppState,
    session_id: &str,
    writer: &mut PtyWriter,
    text: &str,
    enter: bool,
) -> Result<(), String> {
    writer.write_input(text, enter)?;
    append_agent_event(
        state,
        "pty.input_sent",
        "Input sent to PTY session.",
        json!({"name": session_id, "chars": text.len() + if enter { 1 } else { 0 }, "enter": enter, "submit_split": enter}),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{CommandEnvelopeV1, CommandType};
    use crate::config::LocalConfig;

    #[test]
    fn submit_session_command_uses_registered_actor_handle() {
        let state = AppState::new(
            std::env::temp_dir().join(format!("agentcall-actor-test-{}", std::process::id())),
            LocalConfig::default(),
            None,
        );
        let (tx, rx) = mpsc::channel();
        state.actors.lock().unwrap().insert(
            "worker-a".to_string(),
            ActorHandle {
                session_id: "worker-a".to_string(),
                sender: tx,
            },
        );
        thread::spawn(move || {
            let ActorControlCommand::Submit(command, reply) = rx.recv().unwrap() else {
                return;
            };
            let _ = reply.send(Ok(json!({
                "ok": true,
                "command_id": command.command_id,
            })));
        });
        let result = submit_session_command(&state, "worker-a", test_command()).unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["command_id"], "cmd-1");
    }

    #[test]
    fn actor_control_command_can_carry_refresh_projection_signal() {
        let command = ActorControlCommand::RefreshProjection;
        assert!(matches!(command, ActorControlCommand::RefreshProjection));
    }

    #[test]
    fn interrupt_sent_does_not_mark_command_completed() {
        let (event_type, _, awaiting_observation) =
            command_terminal_event(&CommandType::InterruptTurn);
        assert_eq!(event_type, "command.awaiting_observation");
        assert!(awaiting_observation);

        let (send_event_type, _, send_awaiting) = command_terminal_event(&CommandType::SendInput);
        assert_eq!(send_event_type, "command.completed");
        assert!(!send_awaiting);
    }

    fn test_command() -> CommandEnvelopeV1 {
        CommandEnvelopeV1 {
            schema_version: 1,
            command_id: "cmd-1".to_string(),
            session_id: "worker-a".to_string(),
            run_id: None,
            owner_id: "codex".to_string(),
            owner_lease_id: "lease".to_string(),
            lease_generation: 1,
            idempotency_key: "idem".to_string(),
            command_type: CommandType::SendInput,
            payload: json!({"text": "hello"}),
            precondition: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}
