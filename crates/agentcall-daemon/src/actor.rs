use crate::commands::{CommandEnvelopeV1, CommandType};
use crate::control::{command_type_needs_actor_revalidation, validate_envelope_control_at_actor};
use crate::hooks::queue_supervisor_instruction;
use crate::session::{Session, kill_session, request_stop_session};
use crate::state::{AppState, append_agent_event, complete_command_event};
use crate::store::CommandStatus;
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
    #[cfg(test)]
    PanicForTest,
    #[allow(dead_code)]
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
        run_session_actor_with_panic_guard(state, session.name.clone(), writer, receiver)
    });
}

fn run_session_actor_with_panic_guard(
    state: Arc<AppState>,
    session_id: String,
    writer: Box<dyn Write + Send>,
    receiver: Receiver<ActorControlCommand>,
) {
    let actor_state = Arc::clone(&state);
    let actor_session_id = session_id.clone();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        session_actor_loop(state, session_id, PtyWriter::new(writer), receiver)
    }));
    if result.is_err() {
        append_actor_failure_event(
            &actor_state,
            &actor_session_id,
            "session.actor_failed",
            "Session actor panicked.",
            "actor panic",
        );
    }
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
            append_actor_failure_event(
                state,
                session_id,
                "session.orphaned",
                "Session actor is missing; session is orphaned or not actor-managed.",
                "missing_actor_handle",
            );
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
    let mut backlog = Vec::new();
    loop {
        if backlog.is_empty() {
            match receiver.recv() {
                Ok(command) => backlog.push(command),
                Err(_) => break,
            }
        }
        while let Ok(command) = receiver.try_recv() {
            backlog.push(command);
            if backlog.len() >= 256 {
                break;
            }
        }
        let Some(command) = pop_highest_priority(&mut backlog) else {
            continue;
        };
        handle_actor_control_command(&state, &session_id, &mut writer, command);
    }
}

fn pop_highest_priority(backlog: &mut Vec<ActorControlCommand>) -> Option<ActorControlCommand> {
    let (index, _) = backlog
        .iter()
        .enumerate()
        .min_by_key(|(_, command)| actor_command_priority(command))?;
    Some(backlog.remove(index))
}

fn actor_command_priority(command: &ActorControlCommand) -> u8 {
    match command {
        ActorControlCommand::Submit(envelope, _) => command_type_priority(&envelope.command_type),
        ActorControlCommand::RawWrite(_) | ActorControlCommand::RefreshProjection => 4,
        #[cfg(test)]
        ActorControlCommand::PanicForTest => 0,
    }
}

fn command_type_priority(command_type: &CommandType) -> u8 {
    match command_type {
        CommandType::KillSession => 0,
        CommandType::StopSession | CommandType::InterruptTurn | CommandType::CancelCommand => 1,
        CommandType::SelectOption => 2,
        CommandType::SendInput | CommandType::RequestReport => 3,
        CommandType::QueueSupervisorInstruction | CommandType::RefreshProjection => 4,
    }
}

fn handle_actor_control_command(
    state: &AppState,
    session_id: &str,
    writer: &mut PtyWriter,
    command: ActorControlCommand,
) {
    match command {
        ActorControlCommand::Submit(envelope, reply) => {
            let result = execute_command(state, session_id, &envelope, writer);
            let _ = reply.send(result);
        }
        ActorControlCommand::RawWrite(bytes) => {
            let _ = writer.write_raw(&bytes);
        }
        #[cfg(test)]
        ActorControlCommand::PanicForTest => panic!("actor panic test"),
        ActorControlCommand::RefreshProjection => {}
    }
}

fn execute_command(
    state: &AppState,
    session_id: &str,
    command: &CommandEnvelopeV1,
    writer: &mut PtyWriter,
) -> Result<Value, String> {
    if command.control_token_hash.is_some()
        || command_type_needs_actor_revalidation(&command.command_type)
    {
        if let Err(err) = validate_envelope_control_at_actor(state, session_id, command) {
            append_agent_event(
                state,
                "command.rejected_control",
                "Session actor rejected a stale or invalid control token.",
                json!({
                    "session_id": session_id,
                    "command_id": command.command_id,
                    "idempotency_key": command.idempotency_key,
                    "command_type": format!("{:?}", command.command_type),
                    "owner_id": command.owner_id,
                    "owner_lease_id": command.owner_lease_id,
                    "lease_generation": command.lease_generation,
                    "control_epoch": command.control_epoch,
                    "control_token_hash": command.control_token_hash,
                    "status": err.status,
                    "reason": err.reason,
                    "current": err.current
                }),
            );
            let _ = complete_command_event(
                state,
                &command.command_id,
                CommandStatus::Rejected,
                "command.completed",
                "Session command rejected by actor control validation.",
                json!({
                    "session_id": session_id,
                    "command_id": command.command_id,
                    "idempotency_key": command.idempotency_key,
                    "status": "rejected",
                    "reason": "actor_control_validation_failed"
                }),
            );
            return Ok(err.to_value());
        }
    }
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
            "control_epoch": command.control_epoch,
            "control_token_hash": command.control_token_hash,
        }),
    );
    let result = match command.command_type {
        CommandType::StopSession | CommandType::KillSession => {
            let (action, effect, process_tree_signal) = match command.command_type {
                CommandType::KillSession => {
                    let kill = kill_session(state, session_id)?;
                    ("kill", "kill_requested", kill)
                }
                _ => {
                    let stop = request_stop_session(state, session_id)?;
                    ("stop", "stop_requested", stop)
                }
            };
            json!({
                "ok": true,
                "action": action,
                "status": "stop_signal_sent",
                "command_status": "dispatched",
                "effect": effect,
                "process_tree_signal": process_tree_signal,
                "lease_release": "pending_process_exit",
                "awaiting_observation": true,
                "next_required_observation": "process_exited_or_session_ended",
                "next_observation": "agentcall_session(view=summary)"
            })
        }
        CommandType::InterruptTurn => {
            let redirect_text = command
                .payload
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Err(err) = writer.write_raw(b"\x1b") {
                append_actor_failure_event(
                    state,
                    session_id,
                    "session.writer_failed",
                    "PTY writer failed while sending interrupt.",
                    &err,
                );
                return Err(err);
            }
            append_agent_event(
                state,
                "pty.interrupt_sent",
                "Interrupt sent to PTY session.",
                json!({
                    "session_id": session_id,
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
                "command_status": "dispatched",
                "effect": "interrupt_requested",
                "awaiting_observation": true,
                "next_required_observation": "hook_idle_or_ready_prompt",
                "next_observation": "agentcall_session(view=summary)",
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
    complete_command_event(
        state,
        &command.command_id,
        if awaiting_observation {
            CommandStatus::Accepted
        } else {
            CommandStatus::Completed
        },
        event_type,
        message,
        json!({
            "session_id": session_id,
            "command_id": command.command_id,
            "idempotency_key": command.idempotency_key,
            "command_type": format!("{:?}", command.command_type),
            "awaiting_observation": awaiting_observation,
        }),
    )?;
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
    if let Err(err) = writer.write_input(text, enter) {
        append_actor_failure_event(
            state,
            session_id,
            "session.writer_failed",
            "PTY writer failed while sending input.",
            &err,
        );
        return Err(err);
    }
    append_agent_event(
        state,
        "pty.input_sent",
        "Input sent to PTY session.",
        json!({"session_id": session_id, "name": session_id, "chars": text.len() + if enter { 1 } else { 0 }, "enter": enter, "submit_split": enter}),
    );
    Ok(())
}

fn append_actor_failure_event(
    state: &AppState,
    session_id: &str,
    event_type: &str,
    message: &str,
    error: &str,
) {
    append_agent_event(
        state,
        event_type,
        message,
        json!({
            "session_id": session_id,
            "name": session_id,
            "error": error,
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{CommandEnvelopeV1, CommandType};
    use crate::config::LocalConfig;
    use std::io;

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
    fn actor_panic_guard_projects_actor_failed() {
        let state = Arc::new(AppState::new(
            std::env::temp_dir().join(format!("agentcall-actor-panic-{}", std::process::id())),
            LocalConfig::default(),
            None,
        ));
        let (tx, rx) = mpsc::channel();
        tx.send(ActorControlCommand::PanicForTest).unwrap();
        drop(tx);

        run_session_actor_with_panic_guard(
            Arc::clone(&state),
            "worker-a".to_string(),
            Box::new(Vec::<u8>::new()),
            rx,
        );

        let projection = crate::projection::read_session_projection(&state, "worker-a").unwrap();
        assert_eq!(projection.liveness_status, "failed_or_orphaned");
        assert_eq!(projection.attention_status, "failed");
        assert!(projection.needs_attention);
        let _ = std::fs::remove_dir_all(&state.workspace);
    }

    #[test]
    fn actor_backlog_pops_control_commands_before_normal_input_and_raw_write() {
        let (raw_tx, _raw_rx) = mpsc::channel();
        let (send_tx, _send_rx) = mpsc::channel();
        let (interrupt_tx, _interrupt_rx) = mpsc::channel();
        let (kill_tx, _kill_rx) = mpsc::channel();
        let mut backlog = vec![
            ActorControlCommand::RawWrite(vec![1, 2, 3]),
            ActorControlCommand::Submit(test_command_with_type(CommandType::SendInput), send_tx),
            ActorControlCommand::Submit(
                test_command_with_type(CommandType::InterruptTurn),
                interrupt_tx,
            ),
            ActorControlCommand::Submit(test_command_with_type(CommandType::KillSession), kill_tx),
            ActorControlCommand::Submit(
                test_command_with_type(CommandType::QueueSupervisorInstruction),
                raw_tx,
            ),
        ];

        let Some(ActorControlCommand::Submit(first, _)) = pop_highest_priority(&mut backlog) else {
            panic!("expected first command");
        };
        assert_eq!(first.command_type, CommandType::KillSession);

        let Some(ActorControlCommand::Submit(second, _)) = pop_highest_priority(&mut backlog)
        else {
            panic!("expected second command");
        };
        assert_eq!(second.command_type, CommandType::InterruptTurn);

        let Some(ActorControlCommand::Submit(third, _)) = pop_highest_priority(&mut backlog) else {
            panic!("expected third command");
        };
        assert_eq!(third.command_type, CommandType::SendInput);
    }

    #[test]
    fn interrupt_sent_does_not_mark_command_completed() {
        let (event_type, _, awaiting_observation) =
            command_terminal_event(&CommandType::InterruptTurn);
        assert_eq!(event_type, "command.awaiting_observation");
        assert!(awaiting_observation);

        let (stop_event_type, _, stop_awaiting) = command_terminal_event(&CommandType::StopSession);
        assert_eq!(stop_event_type, "command.awaiting_observation");
        assert!(stop_awaiting);

        let (kill_event_type, _, kill_awaiting) = command_terminal_event(&CommandType::KillSession);
        assert_eq!(kill_event_type, "command.awaiting_observation");
        assert!(kill_awaiting);

        let (send_event_type, _, send_awaiting) = command_terminal_event(&CommandType::SendInput);
        assert_eq!(send_event_type, "command.completed");
        assert!(!send_awaiting);
    }

    #[test]
    fn writer_error_marks_projection_failed() {
        struct FailingWriter;
        impl Write for FailingWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "writer closed"))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let state = AppState::new(
            std::env::temp_dir().join(format!(
                "agentcall-actor-writer-error-{}",
                std::process::id()
            )),
            LocalConfig::default(),
            None,
        );
        let mut writer = PtyWriter::new(Box::new(FailingWriter));
        let result = execute_command(&state, "worker-a", &test_command(), &mut writer);
        assert!(result.unwrap_err().contains("writer closed"));
        let projection = crate::projection::read_session_projection(&state, "worker-a").unwrap();
        assert_eq!(projection.liveness_status, "failed_or_orphaned");
        assert_eq!(projection.attention_status, "failed");
    }

    fn test_command() -> CommandEnvelopeV1 {
        test_command_with_type(CommandType::SendInput)
    }

    fn test_command_with_type(command_type: CommandType) -> CommandEnvelopeV1 {
        CommandEnvelopeV1 {
            schema_version: 1,
            command_id: "cmd-1".to_string(),
            session_id: "worker-a".to_string(),
            run_id: None,
            owner_id: "codex".to_string(),
            owner_lease_id: "lease".to_string(),
            lease_generation: 1,
            idempotency_key: "idem".to_string(),
            control_epoch: None,
            control_token_hash: None,
            command_type,
            payload: json!({"text": "hello"}),
            precondition: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}
