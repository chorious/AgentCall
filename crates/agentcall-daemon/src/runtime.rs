#![allow(dead_code)]

use crate::commands::CommandEnvelopeV1;
use crate::events::EventEnvelopeV1;
use crate::session::SessionInfo;
use crate::store::ArtifactIndexRecord;
use serde_json::Value;

#[derive(Clone, Debug)]
pub(crate) struct RuntimeCapabilities {
    pub(crate) runtime_id: &'static str,
    pub(crate) supports_pty: bool,
    pub(crate) supports_sdk: bool,
    pub(crate) command_path: &'static str,
}

#[derive(Clone, Debug)]
pub(crate) struct StartSpec {
    pub(crate) name: String,
    pub(crate) command: Vec<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) cols: Option<u16>,
    pub(crate) rows: Option<u16>,
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimeSession {
    pub(crate) session_id: String,
    pub(crate) runtime: String,
    pub(crate) info: SessionInfo,
}

#[derive(Clone, Debug)]
pub(crate) struct EventCursor {
    pub(crate) after_global_seq: Option<u64>,
    pub(crate) limit: usize,
    pub(crate) event_types: Vec<String>,
}

pub(crate) trait AgentRuntime: Send + Sync {
    fn id(&self) -> &'static str;
    fn capabilities(&self) -> RuntimeCapabilities;
    fn start(&self, spec: StartSpec) -> Result<RuntimeSession, String>;
    fn submit_command(&self, session_id: &str, command: CommandEnvelopeV1)
    -> Result<Value, String>;
    fn observe_events(
        &self,
        session_id: &str,
        cursor: EventCursor,
    ) -> Result<Vec<EventEnvelopeV1>, String>;
    fn collect_artifacts(
        &self,
        session: &RuntimeSession,
    ) -> Result<Vec<ArtifactIndexRecord>, String>;
}
