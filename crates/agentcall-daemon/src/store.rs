#![allow(dead_code)]

use crate::commands::CommandEnvelopeV1;
use crate::events::EventEnvelopeV1;
use crate::ownership::{OwnerLease, WorkspaceLease};
use crate::projection::{ProjectionUpdate, SessionProjectionV1};
use serde_json::Value;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;

#[derive(Clone, Debug)]
pub(crate) struct EventQuery {
    pub(crate) session_id: Option<String>,
    pub(crate) after_global_seq: Option<u64>,
    pub(crate) event_types: Vec<String>,
    pub(crate) limit: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct BoardQuery {
    pub(crate) attention_only: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ReportIndexRecord {
    pub(crate) report_id: String,
    pub(crate) session_id: Option<String>,
    pub(crate) path: String,
    pub(crate) status: String,
    pub(crate) updated_at: String,
}

#[derive(Clone, Debug)]
pub(crate) struct ArtifactIndexRecord {
    pub(crate) artifact_id: String,
    pub(crate) session_id: Option<String>,
    pub(crate) kind: String,
    pub(crate) path: String,
    pub(crate) created_at: String,
}

#[derive(Clone, Debug)]
pub(crate) struct SessionRecord {
    pub(crate) session_id: String,
    pub(crate) owner_id: String,
    pub(crate) workspace: String,
    pub(crate) workspace_key: String,
    pub(crate) runtime: String,
}

#[derive(Clone, Debug)]
pub(crate) struct AppendResult {
    pub(crate) global_seq: u64,
    pub(crate) projection_updated: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct CommandRecord {
    pub(crate) command_id: String,
    pub(crate) owner_id: String,
    pub(crate) idempotency_key: String,
    pub(crate) fingerprint: String,
    pub(crate) status: String,
}

#[derive(Clone, Debug)]
pub(crate) enum IdempotencyDecisionV1 {
    Recorded(CommandRecord),
    Deduped(CommandRecord),
    RejectedDifferentFingerprint(CommandRecord),
}

#[derive(Clone, Debug)]
pub(crate) enum CommandStatus {
    Accepted,
    Completed,
    Failed,
    Rejected,
}

#[derive(Clone, Debug)]
pub(crate) enum RouteDecisionV1 {
    Created,
    Rejected(String),
}

pub(crate) trait RuntimeStore: Send + Sync {
    fn backend_name(&self) -> &'static str;

    fn get_events(&self, query: EventQuery) -> Result<Vec<EventEnvelopeV1>, String>;
    fn get_session_projection(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionProjectionV1>, String>;
    fn list_board_projection(&self, query: BoardQuery) -> Result<Value, String>;
    fn get_idempotency(&self, owner: &str, key: &str) -> Result<Option<CommandRecord>, String>;
    fn save_report_index(&self, report: &ReportIndexRecord) -> Result<(), String>;
    fn save_artifact_index(&self, artifact: &ArtifactIndexRecord) -> Result<(), String>;
    fn renew_owner_lease(&self, lease_id: &str) -> Result<(), String>;
    fn record_file_read(&self, session_id: &str, path: &str) -> Result<(), String>;
    fn record_file_write(&self, session_id: &str, path: &str) -> Result<(), String>;

    fn append_event_and_update_projection(
        &self,
        event: &EventEnvelopeV1,
        projection_update: ProjectionUpdate,
    ) -> Result<AppendResult, String>;

    fn register_command_idempotently(
        &self,
        command: &CommandEnvelopeV1,
    ) -> Result<IdempotencyDecisionV1, String>;

    fn complete_command_with_event(
        &self,
        command_id: &str,
        status: CommandStatus,
        event: &EventEnvelopeV1,
        projection_update: ProjectionUpdate,
    ) -> Result<(), String>;

    fn acquire_route_leases_and_create_session(
        &self,
        session: &SessionRecord,
        owner_lease: &OwnerLease,
        workspace_lease: Option<&WorkspaceLease>,
    ) -> Result<RouteDecisionV1, String>;
}

enum StoreWriteRequest {
    SaveReportIndex(ReportIndexRecord, mpsc::Sender<Result<(), String>>),
    SaveArtifactIndex(ArtifactIndexRecord, mpsc::Sender<Result<(), String>>),
    RenewOwnerLease(String, mpsc::Sender<Result<(), String>>),
    RecordFileRead(String, String, mpsc::Sender<Result<(), String>>),
    RecordFileWrite(String, String, mpsc::Sender<Result<(), String>>),
    AppendEventAndUpdateProjection(
        EventEnvelopeV1,
        ProjectionUpdate,
        mpsc::Sender<Result<AppendResult, String>>,
    ),
    RegisterCommandIdempotently(
        CommandEnvelopeV1,
        mpsc::Sender<Result<IdempotencyDecisionV1, String>>,
    ),
    CompleteCommandWithEvent(
        String,
        CommandStatus,
        EventEnvelopeV1,
        ProjectionUpdate,
        mpsc::Sender<Result<(), String>>,
    ),
    AcquireRouteLeasesAndCreateSession(
        SessionRecord,
        OwnerLease,
        Option<WorkspaceLease>,
        mpsc::Sender<Result<RouteDecisionV1, String>>,
    ),
}

pub(crate) struct StoreWriterRuntimeStore {
    inner: Arc<dyn RuntimeStore>,
    tx: mpsc::Sender<StoreWriteRequest>,
}

impl StoreWriterRuntimeStore {
    pub(crate) fn new(inner: Arc<dyn RuntimeStore>) -> Self {
        let (tx, rx) = mpsc::channel::<StoreWriteRequest>();
        let writer_inner = Arc::clone(&inner);
        thread::Builder::new()
            .name("agentcall-store-writer".to_string())
            .spawn(move || store_writer_loop(writer_inner, rx))
            .expect("failed to spawn AgentCall store writer");
        Self { inner, tx }
    }
}

impl RuntimeStore for StoreWriterRuntimeStore {
    fn backend_name(&self) -> &'static str {
        self.inner.backend_name()
    }

    fn get_events(&self, query: EventQuery) -> Result<Vec<EventEnvelopeV1>, String> {
        self.inner.get_events(query)
    }

    fn get_session_projection(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionProjectionV1>, String> {
        self.inner.get_session_projection(session_id)
    }

    fn list_board_projection(&self, query: BoardQuery) -> Result<Value, String> {
        self.inner.list_board_projection(query)
    }

    fn get_idempotency(&self, owner: &str, key: &str) -> Result<Option<CommandRecord>, String> {
        self.inner.get_idempotency(owner, key)
    }

    fn save_report_index(&self, report: &ReportIndexRecord) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(StoreWriteRequest::SaveReportIndex(report.clone(), tx))
            .map_err(|err| format!("store_writer_closed: {err}"))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn save_artifact_index(&self, artifact: &ArtifactIndexRecord) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(StoreWriteRequest::SaveArtifactIndex(artifact.clone(), tx))
            .map_err(|err| format!("store_writer_closed: {err}"))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn renew_owner_lease(&self, lease_id: &str) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(StoreWriteRequest::RenewOwnerLease(lease_id.to_string(), tx))
            .map_err(|err| format!("store_writer_closed: {err}"))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn record_file_read(&self, session_id: &str, path: &str) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(StoreWriteRequest::RecordFileRead(
                session_id.to_string(),
                path.to_string(),
                tx,
            ))
            .map_err(|err| format!("store_writer_closed: {err}"))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn record_file_write(&self, session_id: &str, path: &str) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(StoreWriteRequest::RecordFileWrite(
                session_id.to_string(),
                path.to_string(),
                tx,
            ))
            .map_err(|err| format!("store_writer_closed: {err}"))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn append_event_and_update_projection(
        &self,
        event: &EventEnvelopeV1,
        projection_update: ProjectionUpdate,
    ) -> Result<AppendResult, String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(StoreWriteRequest::AppendEventAndUpdateProjection(
                event.clone(),
                projection_update,
                tx,
            ))
            .map_err(|err| format!("store_writer_closed: {err}"))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn register_command_idempotently(
        &self,
        command: &CommandEnvelopeV1,
    ) -> Result<IdempotencyDecisionV1, String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(StoreWriteRequest::RegisterCommandIdempotently(
                command.clone(),
                tx,
            ))
            .map_err(|err| format!("store_writer_closed: {err}"))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn complete_command_with_event(
        &self,
        command_id: &str,
        status: CommandStatus,
        event: &EventEnvelopeV1,
        projection_update: ProjectionUpdate,
    ) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(StoreWriteRequest::CompleteCommandWithEvent(
                command_id.to_string(),
                status,
                event.clone(),
                projection_update,
                tx,
            ))
            .map_err(|err| format!("store_writer_closed: {err}"))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn acquire_route_leases_and_create_session(
        &self,
        session: &SessionRecord,
        owner_lease: &OwnerLease,
        workspace_lease: Option<&WorkspaceLease>,
    ) -> Result<RouteDecisionV1, String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(StoreWriteRequest::AcquireRouteLeasesAndCreateSession(
                session.clone(),
                owner_lease.clone(),
                workspace_lease.cloned(),
                tx,
            ))
            .map_err(|err| format!("store_writer_closed: {err}"))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }
}

fn store_writer_loop(inner: Arc<dyn RuntimeStore>, rx: mpsc::Receiver<StoreWriteRequest>) {
    for request in rx {
        match request {
            StoreWriteRequest::SaveReportIndex(report, reply) => {
                let _ = reply.send(inner.save_report_index(&report));
            }
            StoreWriteRequest::SaveArtifactIndex(artifact, reply) => {
                let _ = reply.send(inner.save_artifact_index(&artifact));
            }
            StoreWriteRequest::RenewOwnerLease(lease_id, reply) => {
                let _ = reply.send(inner.renew_owner_lease(&lease_id));
            }
            StoreWriteRequest::RecordFileRead(session_id, path, reply) => {
                let _ = reply.send(inner.record_file_read(&session_id, &path));
            }
            StoreWriteRequest::RecordFileWrite(session_id, path, reply) => {
                let _ = reply.send(inner.record_file_write(&session_id, &path));
            }
            StoreWriteRequest::AppendEventAndUpdateProjection(event, projection_update, reply) => {
                let _ =
                    reply.send(inner.append_event_and_update_projection(&event, projection_update));
            }
            StoreWriteRequest::RegisterCommandIdempotently(command, reply) => {
                let _ = reply.send(inner.register_command_idempotently(&command));
            }
            StoreWriteRequest::CompleteCommandWithEvent(
                command_id,
                status,
                event,
                projection_update,
                reply,
            ) => {
                let _ = reply.send(inner.complete_command_with_event(
                    &command_id,
                    status,
                    &event,
                    projection_update,
                ));
            }
            StoreWriteRequest::AcquireRouteLeasesAndCreateSession(
                session,
                owner_lease,
                workspace_lease,
                reply,
            ) => {
                let _ = reply.send(inner.acquire_route_leases_and_create_session(
                    &session,
                    &owner_lease,
                    workspace_lease.as_ref(),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{CommandEnvelopeV1, CommandType};
    use crate::store_json::JsonRuntimeStore;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn store_writer_serializes_concurrent_command_writes() {
        let root = test_workspace("writer-commands");
        let store: Arc<dyn RuntimeStore> = Arc::new(StoreWriterRuntimeStore::new(Arc::new(
            JsonRuntimeStore::new(root.clone()),
        )));
        let mut workers = Vec::new();
        for idx in 0..8 {
            let store = Arc::clone(&store);
            workers.push(std::thread::spawn(move || {
                let command = command_for(idx);
                store.register_command_idempotently(&command).unwrap()
            }));
        }
        for worker in workers {
            assert!(matches!(
                worker.join().unwrap(),
                IdempotencyDecisionV1::Recorded(_)
            ));
        }
        let commands_log = fs::read_to_string(root.join(".agentcall/state/commands.ndjson"))
            .expect("commands log should exist");
        assert_eq!(commands_log.lines().count(), 8);
        let _ = fs::remove_dir_all(root);
    }

    fn command_for(idx: usize) -> CommandEnvelopeV1 {
        CommandEnvelopeV1 {
            schema_version: 1,
            command_id: format!("cmd-{idx}"),
            session_id: "worker-a".to_string(),
            run_id: None,
            owner_id: "codex".to_string(),
            owner_lease_id: "lease-worker-a-1".to_string(),
            lease_generation: 1,
            idempotency_key: format!("idem-{idx}"),
            command_type: CommandType::SendInput,
            payload: json!({"text": format!("go-{idx}")}),
            precondition: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    fn test_workspace(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("agentcall-store-writer-{name}-{nonce}"))
    }
}
