#![allow(dead_code)]

use crate::commands::CommandEnvelopeV1;
use crate::events::EventEnvelopeV1;
use crate::ownership::{OwnerLease, WorkspaceLease};
use crate::projection::{ProjectionUpdate, SessionProjectionV1};
use crate::store::{
    AppendResult, ArtifactIndexRecord, BoardQuery, CommandRecord, CommandStatus, EventQuery,
    IdempotencyDecisionV1, ReportIndexRecord, RouteDecisionV1, RuntimeStore, SessionRecord,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) struct SqliteRuntimeStore {
    db_path: PathBuf,
}

impl SqliteRuntimeStore {
    pub(crate) fn new(workspace: PathBuf) -> Result<Self, String> {
        let db_path = workspace
            .join(".agentcall")
            .join("state")
            .join("runtime.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        let store = Self { db_path };
        store.migrate()?;
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) fn new_at_path(db_path: PathBuf) -> Result<Self, String> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        let store = Self { db_path };
        store.migrate()?;
        Ok(store)
    }

    fn connect(&self) -> Result<Connection, String> {
        open_connection(&self.db_path)
    }

    fn migrate(&self) -> Result<(), String> {
        let conn = self.connect()?;
        conn.execute_batch(SQLITE_SCHEMA)
            .map_err(|err| err.to_string())
    }
}

impl RuntimeStore for SqliteRuntimeStore {
    fn backend_name(&self) -> &'static str {
        "sqlite"
    }

    fn get_events(&self, query: EventQuery) -> Result<Vec<EventEnvelopeV1>, String> {
        let conn = self.connect()?;
        let requested_limit = if query.limit == 0 {
            100
        } else {
            query.limit.min(1000)
        };
        let scan_limit = if query.event_types.is_empty() {
            requested_limit
        } else {
            requested_limit.saturating_mul(10).min(5000)
        } as i64;
        let mut events = Vec::new();
        if let Some(session_id) = query.session_id {
            let after = query.after_global_seq.unwrap_or(0) as i64;
            let mut stmt = conn
                .prepare(
                    "SELECT payload_json FROM events \
                     WHERE session_id = ?1 AND global_seq > ?2 \
                     ORDER BY global_seq ASC LIMIT ?3",
                )
                .map_err(|err| err.to_string())?;
            let rows = stmt
                .query_map(params![session_id, after, scan_limit], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|err| err.to_string())?;
            for row in rows {
                push_event_json(&mut events, &row.map_err(|err| err.to_string())?);
            }
        } else {
            let after = query.after_global_seq.unwrap_or(0) as i64;
            let mut stmt = conn
                .prepare(
                    "SELECT payload_json FROM events \
                     WHERE global_seq > ?1 ORDER BY global_seq ASC LIMIT ?2",
                )
                .map_err(|err| err.to_string())?;
            let rows = stmt
                .query_map(params![after, scan_limit], |row| row.get::<_, String>(0))
                .map_err(|err| err.to_string())?;
            for row in rows {
                push_event_json(&mut events, &row.map_err(|err| err.to_string())?);
            }
        }
        if !query.event_types.is_empty() {
            events.retain(|event| {
                query
                    .event_types
                    .iter()
                    .any(|event_type| event_type == &event.event_type)
            });
        }
        if events.len() > requested_limit {
            events.truncate(requested_limit);
        }
        Ok(events)
    }

    fn get_session_projection(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionProjectionV1>, String> {
        let conn = self.connect()?;
        let projection_json = conn
            .query_row(
                "SELECT projection_json FROM projections WHERE session_id = ?1",
                params![session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|err| err.to_string())?;
        projection_json
            .map(|text| serde_json::from_str(&text).map_err(|err| err.to_string()))
            .transpose()
    }

    fn list_board_projection(&self, query: BoardQuery) -> Result<Value, String> {
        let conn = self.connect()?;
        let sql = if query.attention_only {
            "SELECT projection_json FROM projections WHERE needs_attention = 1 ORDER BY updated_at DESC"
        } else {
            "SELECT projection_json FROM projections ORDER BY updated_at DESC"
        };
        let mut stmt = conn.prepare(sql).map_err(|err| err.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|err| err.to_string())?;
        let mut sessions = Vec::new();
        for row in rows {
            if let Ok(value) = serde_json::from_str::<Value>(&row.map_err(|err| err.to_string())?) {
                sessions.push(value);
            }
        }
        Ok(json!({"projection_only": true, "store_backend": "sqlite", "sessions": sessions}))
    }

    fn get_idempotency(&self, owner: &str, key: &str) -> Result<Option<CommandRecord>, String> {
        let conn = self.connect()?;
        let scope = idempotency_scope(owner, key);
        conn.query_row(
            "SELECT command_id, owner_id, idempotency_key, fingerprint, status \
             FROM commands WHERE idempotency_scope = ?1 AND idempotency_key = ?2",
            params![scope, key],
            |row| {
                Ok(CommandRecord {
                    command_id: row.get(0)?,
                    owner_id: row.get(1)?,
                    idempotency_key: row.get(2)?,
                    fingerprint: row.get(3)?,
                    status: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(|err| err.to_string())
    }

    fn save_report_index(&self, report: &ReportIndexRecord) -> Result<(), String> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO reports(report_id, session_id, path, status, updated_at) \
             VALUES(?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(report_id) DO UPDATE SET \
             session_id=excluded.session_id, path=excluded.path, status=excluded.status, updated_at=excluded.updated_at",
            params![
                report.report_id,
                report.session_id,
                report.path,
                report.status,
                report.updated_at
            ],
        )
        .map(|_| ())
        .map_err(|err| err.to_string())
    }

    fn save_artifact_index(&self, artifact: &ArtifactIndexRecord) -> Result<(), String> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO artifacts(artifact_id, session_id, kind, path, created_at) \
             VALUES(?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(artifact_id) DO UPDATE SET \
             session_id=excluded.session_id, kind=excluded.kind, path=excluded.path, created_at=excluded.created_at",
            params![
                artifact.artifact_id,
                artifact.session_id,
                artifact.kind,
                artifact.path,
                artifact.created_at
            ],
        )
        .map(|_| ())
        .map_err(|err| err.to_string())
    }

    fn upsert_owner_lease(&self, lease: &OwnerLease) -> Result<(), String> {
        upsert_owner_lease(&self.connect()?, lease)
    }

    fn release_owner_lease(&self, session_id: &str, _reason: &str) -> Result<(), String> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE owner_leases SET status = 'Released', renewed_at = ?1 WHERE session_id = ?2",
            params![chrono::Utc::now().to_rfc3339(), session_id],
        )
        .map(|_| ())
        .map_err(|err| err.to_string())
    }

    fn upsert_workspace_lease(&self, lease: &WorkspaceLease) -> Result<(), String> {
        upsert_workspace_lease(&self.connect()?, lease)
    }

    fn release_workspace_lease(&self, session_id: &str, _reason: &str) -> Result<(), String> {
        let conn = self.connect()?;
        conn.execute(
            "DELETE FROM workspace_leases WHERE session_id = ?1",
            params![session_id],
        )
        .map(|_| ())
        .map_err(|err| err.to_string())
    }

    fn renew_owner_lease(&self, lease_id: &str) -> Result<(), String> {
        let conn = self.connect()?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "UPDATE owner_leases SET renewed_at = ?1, last_heartbeat_at = ?1 WHERE lease_id = ?2",
            params![now, lease_id],
        )
        .map(|_| ())
        .map_err(|err| err.to_string())
    }

    fn record_file_read(&self, session_id: &str, path: &str) -> Result<(), String> {
        record_file_access(&self.connect()?, session_id, path, "read")
    }

    fn record_file_write(&self, session_id: &str, path: &str) -> Result<(), String> {
        record_file_access(&self.connect()?, session_id, path, "write")
    }

    fn append_event_and_update_projection(
        &self,
        event: &EventEnvelopeV1,
        projection_update: ProjectionUpdate,
    ) -> Result<AppendResult, String> {
        let mut conn = self.connect()?;
        let tx = conn.transaction().map_err(|err| err.to_string())?;
        insert_event(&tx, event)?;
        let projection_updated = projection_update.changed;
        if projection_updated {
            upsert_projection(&tx, &projection_update.projection)?;
        }
        tx.commit().map_err(|err| err.to_string())?;
        Ok(AppendResult {
            global_seq: event.global_seq,
            projection_updated,
        })
    }

    fn register_command_idempotently(
        &self,
        command: &CommandEnvelopeV1,
    ) -> Result<IdempotencyDecisionV1, String> {
        let mut conn = self.connect()?;
        let tx = conn.transaction().map_err(|err| err.to_string())?;
        let scope = idempotency_scope(&command.owner_id, &command.idempotency_key);
        let fingerprint = command_fingerprint(command);
        let existing = tx
            .query_row(
                "SELECT command_id, owner_id, idempotency_key, fingerprint, status \
                 FROM commands WHERE idempotency_scope = ?1 AND idempotency_key = ?2",
                params![scope, command.idempotency_key],
                |row| {
                    Ok(CommandRecord {
                        command_id: row.get(0)?,
                        owner_id: row.get(1)?,
                        idempotency_key: row.get(2)?,
                        fingerprint: row.get(3)?,
                        status: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|err| err.to_string())?;
        if let Some(record) = existing {
            tx.commit().map_err(|err| err.to_string())?;
            if record.fingerprint == fingerprint {
                return Ok(IdempotencyDecisionV1::Deduped(record));
            }
            return Ok(IdempotencyDecisionV1::RejectedDifferentFingerprint(record));
        }
        let command_json = serde_json::to_string(command).map_err(|err| err.to_string())?;
        tx.execute(
            "INSERT INTO commands(command_id, session_id, owner_id, owner_lease_id, lease_generation, \
             idempotency_scope, idempotency_key, fingerprint, status, command_json, created_at, updated_at) \
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'accepted', ?9, ?10, ?10)",
            params![
                command.command_id,
                command.session_id,
                command.owner_id,
                command.owner_lease_id,
                command.lease_generation as i64,
                scope,
                command.idempotency_key,
                fingerprint,
                command_json,
                command.created_at
            ],
        )
        .map_err(|err| err.to_string())?;
        tx.commit().map_err(|err| err.to_string())?;
        Ok(IdempotencyDecisionV1::Recorded(CommandRecord {
            command_id: command.command_id.clone(),
            owner_id: command.owner_id.clone(),
            idempotency_key: command.idempotency_key.clone(),
            fingerprint,
            status: "accepted".to_string(),
        }))
    }

    fn complete_command_with_event(
        &self,
        command_id: &str,
        status: CommandStatus,
        event: &EventEnvelopeV1,
        projection_update: ProjectionUpdate,
    ) -> Result<(), String> {
        let mut conn = self.connect()?;
        let tx = conn.transaction().map_err(|err| err.to_string())?;
        let updated = tx
            .execute(
                "UPDATE commands SET status = ?1, updated_at = ?2 WHERE command_id = ?3",
                params![
                    command_status_text(status),
                    chrono::Utc::now().to_rfc3339(),
                    command_id
                ],
            )
            .map_err(|err| err.to_string())?;
        if updated == 0 {
            return Err(format!("unknown_command_id: {command_id}"));
        }
        insert_event(&tx, event)?;
        if projection_update.changed {
            upsert_projection(&tx, &projection_update.projection)?;
        }
        tx.commit().map_err(|err| err.to_string())
    }

    fn acquire_route_leases_and_create_session(
        &self,
        session: &SessionRecord,
        owner_lease: &OwnerLease,
        workspace_lease: Option<&WorkspaceLease>,
    ) -> Result<RouteDecisionV1, String> {
        let mut conn = self.connect()?;
        let tx = conn.transaction().map_err(|err| err.to_string())?;
        tx.execute(
            "INSERT INTO sessions(session_id, owner_id, workspace, workspace_key, runtime, process_state, turn_state, attention_state, created_at, updated_at) \
             VALUES(?1, ?2, ?3, ?4, ?5, 'spawning', 'idle', 'none', ?6, ?6) \
             ON CONFLICT(session_id) DO UPDATE SET updated_at=excluded.updated_at",
            params![
                session.session_id,
                session.owner_id,
                session.workspace,
                session.workspace_key,
                session.runtime,
                chrono::Utc::now().to_rfc3339()
            ],
        )
        .map_err(|err| err.to_string())?;
        upsert_owner_lease(&tx, owner_lease)?;
        if let Some(workspace_lease) = workspace_lease {
            upsert_workspace_lease(&tx, workspace_lease)?;
        }
        tx.commit().map_err(|err| err.to_string())?;
        Ok(RouteDecisionV1::Created)
    }
}

const SQLITE_SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA busy_timeout = 5000;

CREATE TABLE IF NOT EXISTS sessions (
  session_id TEXT PRIMARY KEY,
  run_id TEXT,
  owner_id TEXT NOT NULL,
  workspace TEXT NOT NULL,
  workspace_key TEXT NOT NULL,
  runtime TEXT NOT NULL,
  process_state TEXT NOT NULL,
  turn_state TEXT NOT NULL,
  attention_state TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
  global_seq INTEGER PRIMARY KEY,
  event_id TEXT UNIQUE NOT NULL,
  session_id TEXT,
  session_seq INTEGER,
  run_id TEXT,
  owner_id TEXT,
  schema_version INTEGER NOT NULL,
  ts TEXT NOT NULL,
  source TEXT NOT NULL,
  event_type TEXT NOT NULL,
  severity TEXT NOT NULL,
  command_id TEXT,
  idempotency_key TEXT,
  trace_id TEXT,
  payload_json TEXT NOT NULL,
  UNIQUE(session_id, session_seq)
);

CREATE INDEX IF NOT EXISTS idx_events_session_seq ON events(session_id, session_seq);
CREATE INDEX IF NOT EXISTS idx_events_global_seq ON events(global_seq);
CREATE INDEX IF NOT EXISTS idx_events_type_ts ON events(event_type, ts);

CREATE TABLE IF NOT EXISTS projections (
  session_id TEXT PRIMARY KEY,
  projection_version INTEGER NOT NULL,
  last_global_seq INTEGER NOT NULL,
  last_session_seq INTEGER NOT NULL,
  stale INTEGER NOT NULL,
  needs_attention INTEGER NOT NULL,
  projection_json TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS commands (
  command_id TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  owner_id TEXT NOT NULL,
  owner_lease_id TEXT NOT NULL,
  lease_generation INTEGER NOT NULL,
  idempotency_scope TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  status TEXT NOT NULL,
  command_json TEXT NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  UNIQUE(idempotency_scope, idempotency_key)
);

CREATE TABLE IF NOT EXISTS owner_leases (
  lease_id TEXT PRIMARY KEY,
  owner_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  lease_generation INTEGER NOT NULL,
  acquired_at TEXT NOT NULL,
  last_heartbeat_at TEXT NOT NULL,
  renewed_at TEXT NOT NULL,
  expires_at TEXT NOT NULL,
  status TEXT NOT NULL,
  recoverable INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS workspace_leases (
  lease_id TEXT PRIMARY KEY,
  workspace TEXT NOT NULL,
  workspace_key TEXT NOT NULL,
  mode TEXT NOT NULL,
  owner_id TEXT NOT NULL,
  session_id TEXT NOT NULL,
  expires_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS reports (
  report_id TEXT PRIMARY KEY,
  session_id TEXT,
  path TEXT NOT NULL,
  status TEXT NOT NULL,
  confidence_band TEXT,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS artifacts (
  artifact_id TEXT PRIMARY KEY,
  session_id TEXT,
  kind TEXT NOT NULL,
  path TEXT NOT NULL,
  size_bytes INTEGER,
  sha256 TEXT,
  created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS file_access (
  session_id TEXT NOT NULL,
  path TEXT NOT NULL,
  access_kind TEXT NOT NULL,
  ts TEXT NOT NULL
);
"#;

fn open_connection(path: &Path) -> Result<Connection, String> {
    let conn = Connection::open(path).map_err(|err| err.to_string())?;
    conn.busy_timeout(Duration::from_millis(5000))
        .map_err(|err| err.to_string())?;
    Ok(conn)
}

fn insert_event(conn: &Connection, event: &EventEnvelopeV1) -> Result<(), String> {
    conn.execute(
        "INSERT INTO events(global_seq, event_id, session_id, session_seq, run_id, owner_id, schema_version, ts, source, event_type, severity, command_id, idempotency_key, trace_id, payload_json) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        params![
            event.global_seq as i64,
            event.event_id,
            event.session_id,
            event.session_seq.map(|value| value as i64),
            event.run_id,
            event.owner_id,
            event.schema_version as i64,
            event.ts,
            event.source,
            event.event_type,
            event.severity,
            event.command_id,
            event.idempotency_key,
            event.trace_id,
            serde_json::to_string(&event.to_compat_json()).map_err(|err| err.to_string())?,
        ],
    )
    .map(|_| ())
    .map_err(|err| err.to_string())
}

fn upsert_projection(conn: &Connection, projection: &SessionProjectionV1) -> Result<(), String> {
    conn.execute(
        "INSERT INTO projections(session_id, projection_version, last_global_seq, last_session_seq, stale, needs_attention, projection_json, updated_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
         ON CONFLICT(session_id) DO UPDATE SET \
         projection_version=excluded.projection_version, last_global_seq=excluded.last_global_seq, \
         last_session_seq=excluded.last_session_seq, stale=excluded.stale, needs_attention=excluded.needs_attention, \
         projection_json=excluded.projection_json, updated_at=excluded.updated_at",
        params![
            projection.session_id,
            projection.projection_version as i64,
            projection.projection_last_global_seq as i64,
            projection.projection_last_session_seq as i64,
            projection.projection_stale as i64,
            projection.needs_attention as i64,
            serde_json::to_string(projection).map_err(|err| err.to_string())?,
            projection.projection_last_updated_at,
        ],
    )
    .map(|_| ())
    .map_err(|err| err.to_string())
}

fn upsert_owner_lease(conn: &Connection, lease: &OwnerLease) -> Result<(), String> {
    conn.execute(
        "INSERT INTO owner_leases(lease_id, owner_id, session_id, lease_generation, acquired_at, last_heartbeat_at, renewed_at, expires_at, status, recoverable) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
         ON CONFLICT(lease_id) DO UPDATE SET last_heartbeat_at=excluded.last_heartbeat_at, renewed_at=excluded.renewed_at, expires_at=excluded.expires_at, status=excluded.status",
        params![
            lease.lease_id,
            lease.owner_id,
            lease.session_id,
            lease.lease_generation as i64,
            lease.acquired_at,
            lease.last_heartbeat_at,
            lease.renewed_at,
            lease.expires_at,
            format!("{:?}", lease.status),
            lease.recoverable as i64,
        ],
    )
    .map(|_| ())
    .map_err(|err| err.to_string())
}

fn upsert_workspace_lease(conn: &Connection, lease: &WorkspaceLease) -> Result<(), String> {
    conn.execute(
        "INSERT INTO workspace_leases(lease_id, workspace, workspace_key, mode, owner_id, session_id, expires_at) \
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT(lease_id) DO UPDATE SET expires_at=excluded.expires_at",
        params![
            lease.lease_id,
            lease.workspace,
            lease.workspace_key,
            format!("{:?}", lease.mode),
            lease.owner_id,
            lease.session_id,
            lease.expires_at,
        ],
    )
    .map(|_| ())
    .map_err(|err| err.to_string())
}

fn record_file_access(
    conn: &Connection,
    session_id: &str,
    path: &str,
    access_kind: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO file_access(session_id, path, access_kind, ts) VALUES(?1, ?2, ?3, ?4)",
        params![
            session_id,
            path,
            access_kind,
            chrono::Utc::now().to_rfc3339()
        ],
    )
    .map(|_| ())
    .map_err(|err| err.to_string())
}

fn push_event_json(events: &mut Vec<EventEnvelopeV1>, text: &str) {
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return;
    };
    if let Some(event) = EventEnvelopeV1::from_value(&value) {
        events.push(event);
    }
}

fn idempotency_scope(owner: &str, key: &str) -> String {
    format!("{owner}:{key}")
}

fn command_fingerprint(command: &CommandEnvelopeV1) -> String {
    serde_json::to_string(&json!({
        "session_id": command.session_id,
        "command_type": command.command_type,
        "payload": command.payload,
        "precondition": command.precondition,
    }))
    .unwrap_or_default()
}

fn command_status_text(status: CommandStatus) -> &'static str {
    match status {
        CommandStatus::Accepted => "accepted",
        CommandStatus::Completed => "completed",
        CommandStatus::Failed => "failed",
        CommandStatus::Rejected => "rejected",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{CommandEnvelopeV1, CommandType};
    use crate::projection::apply_event_to_projection;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn sqlite_migrations_apply_with_required_tables() {
        let store = test_store("migrations");
        let conn = store.connect().unwrap();
        let owner_not_null: i64 = conn
            .query_row(
                "SELECT [notnull] FROM pragma_table_info('commands') WHERE name = 'owner_id'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap()
            .unwrap_or(0);
        assert_eq!(owner_not_null, 1);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('events','projections','commands','sessions')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 4);
    }

    #[test]
    fn sqlite_event_projection_transaction_roundtrips() {
        let store = test_store("event-projection");
        let event = crate::events::build_event_envelope(
            "evt-000001".to_string(),
            1,
            Some(1),
            "hook.Notification",
            "permission requested",
            json!({"wrapper_session": "worker-a", "status": "needs_permission"}),
        );
        let update = apply_event_to_projection(None, &event);
        let result = store
            .append_event_and_update_projection(&event, update)
            .unwrap();
        assert_eq!(result.global_seq, 1);
        assert!(result.projection_updated);
        assert_eq!(
            store
                .get_events(EventQuery {
                    session_id: None,
                    after_global_seq: None,
                    event_types: vec![],
                    limit: 10,
                })
                .unwrap()
                .len(),
            1
        );
        let projection = store.get_session_projection("worker-a").unwrap().unwrap();
        assert_eq!(projection.attention_status, "needs_permission");
    }

    #[test]
    fn sqlite_idempotency_owner_scope_is_non_nullable_and_dedupes() {
        let store = test_store("idempotency");
        let command = command_for("cmd-1", "idem-1", "go");
        let first = store.register_command_idempotently(&command).unwrap();
        assert!(matches!(first, IdempotencyDecisionV1::Recorded(_)));
        let second = store.register_command_idempotently(&command).unwrap();
        assert!(matches!(second, IdempotencyDecisionV1::Deduped(_)));
        let fetched = store.get_idempotency("codex", "idem-1").unwrap().unwrap();
        assert_eq!(fetched.owner_id, "codex");
        assert_eq!(fetched.idempotency_key, "idem-1");
    }

    #[test]
    fn sqlite_command_completion_updates_command_and_event_transactionally() {
        let store = test_store("complete-command");
        let command = command_for("cmd-2", "idem-2", "go");
        store.register_command_idempotently(&command).unwrap();
        let event = crate::events::build_event_envelope(
            "evt-000002".to_string(),
            2,
            Some(1),
            "command.completed",
            "done",
            json!({"wrapper_session": "worker-a", "command_id": "cmd-2"}),
        );
        let update = apply_event_to_projection(None, &event);
        store
            .complete_command_with_event("cmd-2", CommandStatus::Completed, &event, update)
            .unwrap();
        let fetched = store.get_idempotency("codex", "idem-2").unwrap().unwrap();
        assert_eq!(fetched.status, "completed");
        assert_eq!(
            store
                .get_events(EventQuery {
                    session_id: Some("worker-a".to_string()),
                    after_global_seq: Some(0),
                    event_types: vec![],
                    limit: 10,
                })
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn sqlite_command_completion_rejects_unknown_command_without_event() {
        let store = test_store("complete-unknown-command");
        let event = crate::events::build_event_envelope(
            "evt-000003".to_string(),
            3,
            Some(1),
            "command.completed",
            "done",
            json!({"wrapper_session": "worker-a", "command_id": "missing-cmd"}),
        );
        let update = apply_event_to_projection(None, &event);
        let err = store
            .complete_command_with_event("missing-cmd", CommandStatus::Completed, &event, update)
            .unwrap_err();
        assert!(err.contains("unknown_command_id"));
        assert!(
            store
                .get_events(EventQuery {
                    session_id: Some("worker-a".to_string()),
                    after_global_seq: Some(0),
                    event_types: vec![],
                    limit: 10,
                })
                .unwrap()
                .is_empty()
        );
        assert!(store.get_session_projection("worker-a").unwrap().is_none());
    }

    #[test]
    fn sqlite_command_completion_rolls_back_when_event_insert_fails() {
        let store = test_store("complete-rollback");
        let command = command_for("cmd-3", "idem-3", "go");
        store.register_command_idempotently(&command).unwrap();
        let existing_event = crate::events::build_event_envelope(
            "evt-000004".to_string(),
            4,
            Some(1),
            "pty.session_started",
            "started",
            json!({"wrapper_session": "worker-a"}),
        );
        let existing_update = apply_event_to_projection(None, &existing_event);
        store
            .append_event_and_update_projection(&existing_event, existing_update)
            .unwrap();

        let conflicting_event = crate::events::build_event_envelope(
            "evt-000005".to_string(),
            4,
            Some(2),
            "command.completed",
            "done",
            json!({"wrapper_session": "worker-a", "command_id": "cmd-3"}),
        );
        let update = apply_event_to_projection(None, &conflicting_event);
        let err = store
            .complete_command_with_event(
                "cmd-3",
                CommandStatus::Completed,
                &conflicting_event,
                update,
            )
            .unwrap_err();
        assert!(err.contains("UNIQUE") || err.contains("constraint"));
        let fetched = store.get_idempotency("codex", "idem-3").unwrap().unwrap();
        assert_eq!(fetched.status, "accepted");
        let events = store
            .get_events(EventQuery {
                session_id: Some("worker-a".to_string()),
                after_global_seq: Some(0),
                event_types: vec![],
                limit: 10,
            })
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "pty.session_started");
    }

    fn command_for(command_id: &str, idempotency_key: &str, text: &str) -> CommandEnvelopeV1 {
        CommandEnvelopeV1 {
            schema_version: 1,
            command_id: command_id.to_string(),
            session_id: "worker-a".to_string(),
            run_id: None,
            owner_id: "codex".to_string(),
            owner_lease_id: "lease-worker-a-1".to_string(),
            lease_generation: 1,
            idempotency_key: idempotency_key.to_string(),
            command_type: CommandType::SendInput,
            payload: json!({"text": text}),
            precondition: None,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    fn test_store(name: &str) -> SqliteRuntimeStore {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let db_path = std::env::temp_dir()
            .join(format!("agentcall-sqlite-store-{name}-{nonce}"))
            .join("runtime.db");
        SqliteRuntimeStore::new_at_path(db_path).unwrap()
    }
}
