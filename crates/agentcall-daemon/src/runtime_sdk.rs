use crate::commands::CommandEnvelopeV1;
use crate::events::EventEnvelopeV1;
use crate::runtime::{AgentRuntime, EventCursor, RuntimeCapabilities, RuntimeSession, StartSpec};
use crate::state::AppState;
use crate::store::{ArtifactIndexRecord, EventQuery};
use serde_json::Value;
use std::sync::Arc;

pub(crate) struct ClaudeCodeSdkRuntime {
    state: Arc<AppState>,
}

impl ClaudeCodeSdkRuntime {
    pub(crate) fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

impl AgentRuntime for ClaudeCodeSdkRuntime {
    fn id(&self) -> &'static str {
        "claude_code_sdk"
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        RuntimeCapabilities {
            runtime_id: self.id(),
            supports_pty: false,
            supports_sdk: true,
            command_path: "EventEnvelopeProjectionContract",
        }
    }

    fn start(&self, _spec: StartSpec) -> Result<RuntimeSession, String> {
        Err(
            "sdk_runtime_experimental_stub: native SDK worker start is gated and not implemented"
                .to_string(),
        )
    }

    fn submit_command(
        &self,
        _session_id: &str,
        _command: CommandEnvelopeV1,
    ) -> Result<Value, String> {
        Err(
            "sdk_runtime_experimental_stub: submit_command cannot bypass AgentRuntime contract"
                .to_string(),
        )
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

pub(crate) fn sdk_runtime_enabled(state: &AppState) -> bool {
    state.config.experimental_sdk_runtime.unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocalConfig;
    use std::path::PathBuf;

    #[test]
    fn sdk_runtime_capability_uses_event_contract_not_raw_writer() {
        let runtime = ClaudeCodeSdkRuntime::new(Arc::new(test_state(false)));
        let capabilities = runtime.capabilities();
        assert_eq!(capabilities.runtime_id, "claude_code_sdk");
        assert!(capabilities.supports_sdk);
        assert!(!capabilities.supports_pty);
        assert_eq!(capabilities.command_path, "EventEnvelopeProjectionContract");
    }

    #[test]
    fn sdk_runtime_stub_rejects_start_and_submit_without_bypass() {
        let runtime = ClaudeCodeSdkRuntime::new(Arc::new(test_state(true)));
        assert!(
            runtime
                .start(StartSpec {
                    name: "sdk-a".to_string(),
                    command: vec![],
                    cwd: None,
                    cols: None,
                    rows: None,
                })
                .unwrap_err()
                .contains("experimental_stub")
        );
    }

    fn test_state(enabled: bool) -> AppState {
        let root = PathBuf::from(format!(
            "{}\\agentcall-runtime-sdk-test-{}-{enabled}",
            std::env::temp_dir().display(),
            std::process::id()
        ));
        AppState::new(
            root.clone(),
            LocalConfig {
                claude_workspace: Some(root),
                experimental_sdk_runtime: Some(enabled),
                ..LocalConfig::default()
            },
            None,
        )
    }
}
