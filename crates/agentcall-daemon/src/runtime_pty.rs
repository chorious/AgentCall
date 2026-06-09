use crate::actor::submit_session_command;
use crate::commands::CommandEnvelopeV1;
use crate::events::EventEnvelopeV1;
use crate::runtime::{AgentRuntime, EventCursor, RuntimeCapabilities, RuntimeSession, StartSpec};
use crate::session::{StartRequest, start_session};
use crate::state::AppState;
use crate::store::{ArtifactIndexRecord, EventQuery};
use serde_json::Value;
use std::sync::Arc;

pub(crate) struct ClaudeCodePtyRuntime {
    state: Arc<AppState>,
}

impl ClaudeCodePtyRuntime {
    pub(crate) fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

impl AgentRuntime for ClaudeCodePtyRuntime {
    fn id(&self) -> &'static str {
        "claude_code_pty"
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities {
            runtime_id: self.id(),
            supports_pty: true,
            supports_sdk: false,
            command_path: "SessionActor",
        }
    }

    fn start(&self, spec: StartSpec) -> Result<RuntimeSession, String> {
        let info = start_session(
            &self.state,
            StartRequest {
                name: spec.name,
                command: spec.command,
                cwd: spec.cwd,
                cols: spec.cols,
                rows: spec.rows,
            },
        )?;
        Ok(RuntimeSession {
            session_id: info.name.clone(),
            runtime: self.id().to_string(),
            info,
        })
    }

    fn submit_command(
        &self,
        session_id: &str,
        command: CommandEnvelopeV1,
    ) -> Result<Value, String> {
        submit_session_command(&self.state, session_id, command)
    }

    fn observe_events(
        &self,
        session_id: &str,
        cursor: EventCursor,
    ) -> Result<Vec<EventEnvelopeV1>, String> {
        self.state.store.get_events(EventQuery {
            session_id: Some(session_id.to_string()),
            after_global_seq: cursor.after_global_seq,
            event_types: cursor.event_types,
            limit: cursor.limit,
        })
    }

    fn collect_artifacts(
        &self,
        _session: &RuntimeSession,
    ) -> Result<Vec<ArtifactIndexRecord>, String> {
        Ok(vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::{ActorControlCommand, ActorHandle};
    use crate::commands::{CommandEnvelopeV1, CommandType};
    use crate::config::LocalConfig;
    use crate::state::AppState;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::mpsc;

    #[test]
    fn pty_runtime_capability_uses_actor_command_path() {
        let runtime = ClaudeCodePtyRuntime::new(Arc::new(test_state()));
        let capabilities = runtime.capabilities();
        assert_eq!(capabilities.runtime_id, "claude_code_pty");
        assert_eq!(capabilities.command_path, "SessionActor");
        assert!(capabilities.supports_pty);
        assert!(!capabilities.supports_sdk);
    }

    #[test]
    fn pty_runtime_submit_command_delegates_to_actor_registry() {
        let state = Arc::new(test_state());
        let (tx, rx) = mpsc::channel();
        state.actors.lock().unwrap().insert(
            "worker-a".to_string(),
            ActorHandle {
                session_id: "worker-a".to_string(),
                sender: tx,
            },
        );
        let runtime = ClaudeCodePtyRuntime::new(Arc::clone(&state));
        let worker = std::thread::spawn(move || {
            runtime
                .submit_command("worker-a", test_command())
                .expect("runtime should submit through actor registry")
        });
        let received = rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("actor should receive command");
        match received {
            ActorControlCommand::Submit(command, reply) => {
                assert_eq!(command.command_id, "cmd-runtime");
                let _ = reply.send(Ok(json!({"status": "accepted"})));
            }
            _ => panic!("runtime submit must use ActorControlCommand::Submit"),
        }
        let result = worker.join().unwrap();
        assert_eq!(result["status"], "accepted");
    }

    fn test_state() -> AppState {
        let root = PathBuf::from(format!(
            "{}\\agentcall-runtime-pty-test-{}",
            std::env::temp_dir().display(),
            std::process::id()
        ));
        AppState::new(
            root.clone(),
            LocalConfig {
                claude_workspace: Some(root),
                ..LocalConfig::default()
            },
            None,
        )
    }

    fn test_command() -> CommandEnvelopeV1 {
        CommandEnvelopeV1 {
            schema_version: 1,
            command_id: "cmd-runtime".to_string(),
            session_id: "worker-a".to_string(),
            run_id: None,
            owner_id: "codex".to_string(),
            owner_lease_id: "lease-worker-a-1".to_string(),
            lease_generation: 1,
            idempotency_key: "idem-runtime".to_string(),
            command_type: CommandType::SendInput,
            payload: json!({"text": "hello", "enter": true}),
            precondition: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}
