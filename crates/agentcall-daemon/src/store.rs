#![allow(dead_code)]

use crate::commands::CommandEnvelopeV1;
use crate::events::EventEnvelopeV1;
use crate::ownership::{OwnerLease, WorkspaceLease};
use crate::projection::{ProjectionUpdate, SessionProjectionV1};
use serde_json::Value;
use std::collections::{HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
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
    pub(crate) owner_id: Option<String>,
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
    fn supports_parallel_writes(&self) -> bool {
        false
    }
    fn writer_threads(&self) -> usize {
        1
    }
    fn next_event_global_seq(&self, fallback: u64) -> Result<u64, String> {
        Ok(fallback)
    }
    fn next_session_event_numbers(
        &self,
        fallback: HashMap<String, u64>,
    ) -> Result<HashMap<String, u64>, String> {
        Ok(fallback)
    }

    fn get_events(&self, query: EventQuery) -> Result<Vec<EventEnvelopeV1>, String>;
    fn get_session_projection(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionProjectionV1>, String>;
    fn list_board_projection(&self, query: BoardQuery) -> Result<Value, String>;
    fn get_idempotency(&self, owner: &str, key: &str) -> Result<Option<CommandRecord>, String>;
    fn save_report_index(&self, report: &ReportIndexRecord) -> Result<(), String>;
    fn save_artifact_index(&self, artifact: &ArtifactIndexRecord) -> Result<(), String>;
    fn upsert_owner_lease(&self, lease: &OwnerLease) -> Result<(), String>;
    fn release_owner_lease(&self, session_id: &str, reason: &str) -> Result<(), String>;
    fn upsert_workspace_lease(&self, lease: &WorkspaceLease) -> Result<(), String>;
    fn release_workspace_lease(&self, session_id: &str, reason: &str) -> Result<(), String>;
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
    UpsertOwnerLease(OwnerLease, mpsc::Sender<Result<(), String>>),
    ReleaseOwnerLease(String, String, mpsc::Sender<Result<(), String>>),
    UpsertWorkspaceLease(WorkspaceLease, mpsc::Sender<Result<(), String>>),
    ReleaseWorkspaceLease(String, String, mpsc::Sender<Result<(), String>>),
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
    txs: Vec<mpsc::Sender<StoreWriteRequest>>,
    writer_threads: usize,
}

impl StoreWriterRuntimeStore {
    pub(crate) fn new(inner: Arc<dyn RuntimeStore>, requested_threads: usize) -> Self {
        let writer_threads = if inner.supports_parallel_writes() {
            requested_threads.clamp(1, 6)
        } else {
            1
        };
        let mut txs = Vec::with_capacity(writer_threads);
        for index in 0..writer_threads {
            let (tx, rx) = mpsc::channel::<StoreWriteRequest>();
            let writer_inner = Arc::clone(&inner);
            thread::Builder::new()
                .name(format!("agentcall-store-writer-{index}"))
                .spawn(move || store_writer_loop(writer_inner, rx))
                .expect("failed to spawn AgentCall store writer");
            txs.push(tx);
        }
        Self {
            inner,
            txs,
            writer_threads,
        }
    }

    fn send(&self, request: StoreWriteRequest) -> Result<(), String> {
        let index = request.shard_index(self.txs.len());
        self.txs
            .get(index)
            .ok_or_else(|| format!("store_writer_missing: index={index}"))?
            .send(request)
            .map_err(|err| format!("store_writer_closed: {err}"))
    }
}

impl RuntimeStore for StoreWriterRuntimeStore {
    fn backend_name(&self) -> &'static str {
        self.inner.backend_name()
    }

    fn supports_parallel_writes(&self) -> bool {
        self.inner.supports_parallel_writes()
    }

    fn writer_threads(&self) -> usize {
        self.writer_threads
    }

    fn next_event_global_seq(&self, fallback: u64) -> Result<u64, String> {
        self.inner.next_event_global_seq(fallback)
    }

    fn next_session_event_numbers(
        &self,
        fallback: HashMap<String, u64>,
    ) -> Result<HashMap<String, u64>, String> {
        self.inner.next_session_event_numbers(fallback)
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
        self.send(StoreWriteRequest::SaveReportIndex(report.clone(), tx))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn save_artifact_index(&self, artifact: &ArtifactIndexRecord) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(StoreWriteRequest::SaveArtifactIndex(artifact.clone(), tx))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn upsert_owner_lease(&self, lease: &OwnerLease) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(StoreWriteRequest::UpsertOwnerLease(lease.clone(), tx))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn release_owner_lease(&self, session_id: &str, reason: &str) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(StoreWriteRequest::ReleaseOwnerLease(
            session_id.to_string(),
            reason.to_string(),
            tx,
        ))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn upsert_workspace_lease(&self, lease: &WorkspaceLease) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(StoreWriteRequest::UpsertWorkspaceLease(lease.clone(), tx))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn release_workspace_lease(&self, session_id: &str, reason: &str) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(StoreWriteRequest::ReleaseWorkspaceLease(
            session_id.to_string(),
            reason.to_string(),
            tx,
        ))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn renew_owner_lease(&self, lease_id: &str) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(StoreWriteRequest::RenewOwnerLease(lease_id.to_string(), tx))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn record_file_read(&self, session_id: &str, path: &str) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(StoreWriteRequest::RecordFileRead(
            session_id.to_string(),
            path.to_string(),
            tx,
        ))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn record_file_write(&self, session_id: &str, path: &str) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.send(StoreWriteRequest::RecordFileWrite(
            session_id.to_string(),
            path.to_string(),
            tx,
        ))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn append_event_and_update_projection(
        &self,
        event: &EventEnvelopeV1,
        projection_update: ProjectionUpdate,
    ) -> Result<AppendResult, String> {
        let (tx, rx) = mpsc::channel();
        self.send(StoreWriteRequest::AppendEventAndUpdateProjection(
            event.clone(),
            projection_update,
            tx,
        ))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }

    fn register_command_idempotently(
        &self,
        command: &CommandEnvelopeV1,
    ) -> Result<IdempotencyDecisionV1, String> {
        let (tx, rx) = mpsc::channel();
        self.send(StoreWriteRequest::RegisterCommandIdempotently(
            command.clone(),
            tx,
        ))?;
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
        self.send(StoreWriteRequest::CompleteCommandWithEvent(
            command_id.to_string(),
            status,
            event.clone(),
            projection_update,
            tx,
        ))?;
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
        self.send(StoreWriteRequest::AcquireRouteLeasesAndCreateSession(
            session.clone(),
            owner_lease.clone(),
            workspace_lease.cloned(),
            tx,
        ))?;
        rx.recv()
            .map_err(|err| format!("store_writer_response_closed: {err}"))?
    }
}

impl StoreWriteRequest {
    fn shard_index(&self, shards: usize) -> usize {
        if shards <= 1 {
            return 0;
        }
        let mut hasher = DefaultHasher::new();
        self.shard_key().hash(&mut hasher);
        (hasher.finish() as usize) % shards
    }

    fn shard_key(&self) -> String {
        match self {
            Self::SaveReportIndex(report, _) => format!("report:{}", report.report_id),
            Self::SaveArtifactIndex(artifact, _) => format!("artifact:{}", artifact.artifact_id),
            Self::UpsertOwnerLease(lease, _) => format!("session:{}", lease.session_id),
            Self::ReleaseOwnerLease(session_id, _, _) => format!("session:{session_id}"),
            Self::UpsertWorkspaceLease(lease, _) => format!("workspace:{}", lease.workspace_key),
            Self::ReleaseWorkspaceLease(session_id, _, _) => format!("session:{session_id}"),
            Self::RenewOwnerLease(lease_id, _) => format!("lease:{lease_id}"),
            Self::RecordFileRead(session_id, _, _) | Self::RecordFileWrite(session_id, _, _) => {
                format!("session:{session_id}")
            }
            Self::AppendEventAndUpdateProjection(event, _, _) => event
                .session_id
                .as_deref()
                .map(|session_id| format!("session:{session_id}"))
                .unwrap_or_else(|| format!("event:{}", event.global_seq)),
            Self::RegisterCommandIdempotently(command, _) => {
                format!("session:{}", command.session_id)
            }
            Self::CompleteCommandWithEvent(command_id, _, event, _, _) => event
                .session_id
                .as_deref()
                .map(|session_id| format!("session:{session_id}"))
                .unwrap_or_else(|| format!("command:{command_id}")),
            Self::AcquireRouteLeasesAndCreateSession(session, _, _, _) => {
                format!("session:{}", session.session_id)
            }
        }
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
            StoreWriteRequest::UpsertOwnerLease(lease, reply) => {
                let _ = reply.send(inner.upsert_owner_lease(&lease));
            }
            StoreWriteRequest::ReleaseOwnerLease(session_id, reason, reply) => {
                let _ = reply.send(inner.release_owner_lease(&session_id, &reason));
            }
            StoreWriteRequest::UpsertWorkspaceLease(lease, reply) => {
                let _ = reply.send(inner.upsert_workspace_lease(&lease));
            }
            StoreWriteRequest::ReleaseWorkspaceLease(session_id, reason, reply) => {
                let _ = reply.send(inner.release_workspace_lease(&session_id, &reason));
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
    fn json_store_writer_stays_single_thread_even_when_parallel_requested() {
        let root = test_workspace("writer-commands");
        let store: Arc<dyn RuntimeStore> = Arc::new(StoreWriterRuntimeStore::new(
            Arc::new(JsonRuntimeStore::new(root.clone())),
            6,
        ));
        assert_eq!(store.writer_threads(), 1);
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

    #[test]
    fn sqlite_store_writer_stays_single_thread_to_avoid_busy_writer_contention() {
        let root = test_workspace("writer-sqlite-commands");
        let store: Arc<dyn RuntimeStore> = Arc::new(StoreWriterRuntimeStore::new(
            Arc::new(crate::store_sqlite::SqliteRuntimeStore::new(root.clone()).unwrap()),
            6,
        ));
        assert_eq!(store.writer_threads(), 1);
        let mut workers = Vec::new();
        for idx in 0..24 {
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
        let events = store
            .get_idempotency("codex", "idem-23")
            .unwrap()
            .expect("last command idempotency should be recorded");
        assert_eq!(events.idempotency_key, "idem-23");
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
            control_epoch: None,
            control_token_hash: None,
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
