#![allow(dead_code)]

use crate::commands::CommandEnvelopeV1;
use crate::events::EventEnvelopeV1;
use crate::ownership::{OwnerLease, WorkspaceLease};
use crate::projection::{ProjectionUpdate, SessionProjectionV1};
use crate::state::{read_json_file, write_json_file};
use crate::store::{
    AppendResult, ArtifactIndexRecord, BoardQuery, CommandRecord, CommandStatus, EventQuery,
    IdempotencyDecisionV1, ReportIndexRecord, RouteDecisionV1, RuntimeStore, SessionRecord,
};
use serde_json::{Value, json};
use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const RECENT_EVENT_MAX_BYTES: u64 = 2 * 1024 * 1024;

pub(crate) struct JsonRuntimeStore {
    workspace: PathBuf,
}

impl JsonRuntimeStore {
    pub(crate) fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }

    fn agent_dir(&self) -> PathBuf {
        self.workspace.join(".agentcall")
    }
}

impl RuntimeStore for JsonRuntimeStore {
    fn backend_name(&self) -> &'static str {
        "json"
    }

    fn get_events(&self, query: EventQuery) -> Result<Vec<EventEnvelopeV1>, String> {
        let text = fs::read_to_string(self.agent_dir().join("events").join("recent.ndjson"))
            .or_else(|_| fs::read_to_string(self.agent_dir().join("events.ndjson")))
            .unwrap_or_default();
        let mut events = Vec::new();
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(event) = EventEnvelopeV1::from_value(&value) else {
                continue;
            };
            if let Some(after) = query.after_global_seq {
                if event.global_seq <= after {
                    continue;
                }
            }
            if let Some(session_id) = &query.session_id {
                if event.session_id.as_deref() != Some(session_id.as_str()) {
                    continue;
                }
            }
            if !query.event_types.is_empty()
                && !query
                    .event_types
                    .iter()
                    .any(|event_type| event_type == &event.event_type)
            {
                continue;
            }
            events.push(event);
        }
        if query.limit > 0 && events.len() > query.limit {
            events = events.split_off(events.len() - query.limit);
        }
        Ok(events)
    }

    fn get_session_projection(
        &self,
        session_id: &str,
    ) -> Result<Option<SessionProjectionV1>, String> {
        let value = read_json_file(&projection_path(&self.workspace, session_id), json!(null));
        if value.is_null() {
            return Ok(None);
        }
        serde_json::from_value(value)
            .map(Some)
            .map_err(|err| err.to_string())
    }

    fn list_board_projection(&self, query: BoardQuery) -> Result<Value, String> {
        let dir = self
            .agent_dir()
            .join("state")
            .join("projections")
            .join("sessions");
        let mut sessions = Vec::new();
        if let Ok(items) = fs::read_dir(dir) {
            for item in items.flatten() {
                let value = read_json_file(&item.path(), json!(null));
                if value.is_null() {
                    continue;
                }
                if query.attention_only
                    && value.get("needs_attention").and_then(Value::as_bool) != Some(true)
                {
                    continue;
                }
                if !projection_matches_owner(&value, query.owner_id.as_deref()) {
                    continue;
                }
                sessions.push(value);
            }
        }
        Ok(json!({"projection_only": true, "sessions": sessions}))
    }

    fn get_idempotency(&self, owner: &str, key: &str) -> Result<Option<CommandRecord>, String> {
        let value = read_commands_index(&self.workspace)?;
        let scope = format!("{owner}:{key}");
        Ok(value.get(&scope).and_then(command_record_from_value))
    }

    fn save_report_index(&self, report: &ReportIndexRecord) -> Result<(), String> {
        upsert_index_record(
            &self.agent_dir().join("state").join("reports.index.json"),
            &report.report_id,
            json!({
                "report_id": report.report_id,
                "session_id": report.session_id,
                "path": report.path,
                "status": report.status,
                "updated_at": report.updated_at,
            }),
        )
    }

    fn save_artifact_index(&self, artifact: &ArtifactIndexRecord) -> Result<(), String> {
        upsert_index_record(
            &self.agent_dir().join("state").join("artifacts.index.json"),
            &artifact.artifact_id,
            json!({
                "artifact_id": artifact.artifact_id,
                "session_id": artifact.session_id,
                "kind": artifact.kind,
                "path": artifact.path,
                "created_at": artifact.created_at,
            }),
        )
    }

    fn upsert_owner_lease(&self, lease: &OwnerLease) -> Result<(), String> {
        upsert_index_record(
            &self
                .agent_dir()
                .join("state")
                .join("owner_leases.index.json"),
            &lease.session_id,
            json!({
                "lease": lease,
                "status": format!("{:?}", lease.status),
                "updated_at": chrono::Utc::now().to_rfc3339(),
            }),
        )
    }

    fn release_owner_lease(&self, session_id: &str, reason: &str) -> Result<(), String> {
        patch_index_record(
            &self
                .agent_dir()
                .join("state")
                .join("owner_leases.index.json"),
            session_id,
            json!({
                "status": "Released",
                "released_at": chrono::Utc::now().to_rfc3339(),
                "release_reason": reason,
            }),
        )
    }

    fn upsert_workspace_lease(&self, lease: &WorkspaceLease) -> Result<(), String> {
        upsert_index_record(
            &self
                .agent_dir()
                .join("state")
                .join("workspace_leases.index.json"),
            &lease.session_id,
            json!({
                "lease": lease,
                "status": "Active",
                "updated_at": chrono::Utc::now().to_rfc3339(),
            }),
        )
    }

    fn release_workspace_lease(&self, session_id: &str, reason: &str) -> Result<(), String> {
        patch_index_record(
            &self
                .agent_dir()
                .join("state")
                .join("workspace_leases.index.json"),
            session_id,
            json!({
                "status": "Released",
                "released_at": chrono::Utc::now().to_rfc3339(),
                "release_reason": reason,
            }),
        )
    }

    fn renew_owner_lease(&self, lease_id: &str) -> Result<(), String> {
        append_ndjson(
            &self.agent_dir().join("state").join("lease-renewals.ndjson"),
            &json!({"lease_id": lease_id, "renewed_at": chrono::Utc::now().to_rfc3339()}),
        )
    }

    fn record_file_read(&self, session_id: &str, path: &str) -> Result<(), String> {
        append_file_access(&self.workspace, session_id, path, "read")
    }

    fn record_file_write(&self, session_id: &str, path: &str) -> Result<(), String> {
        append_file_access(&self.workspace, session_id, path, "write")
    }

    fn append_event_and_update_projection(
        &self,
        event: &EventEnvelopeV1,
        projection_update: ProjectionUpdate,
    ) -> Result<AppendResult, String> {
        append_rotating_ndjson(
            &self.agent_dir().join("events").join("recent.ndjson"),
            &event.to_compat_json(),
            RECENT_EVENT_MAX_BYTES,
        )?;
        let projection_updated = projection_update.changed;
        if projection_updated {
            write_json_file(
                &projection_path(&self.workspace, &projection_update.projection.session_id),
                &json!(projection_update.projection),
            )?;
        }
        Ok(AppendResult {
            global_seq: event.global_seq,
            projection_updated,
        })
    }

    fn register_command_idempotently(
        &self,
        command: &CommandEnvelopeV1,
    ) -> Result<IdempotencyDecisionV1, String> {
        let scope = format!("{}:{}", command.owner_id, command.idempotency_key);
        let fingerprint = command_fingerprint(command);
        let index_path = commands_index_path(&self.workspace);
        let mut index = read_commands_index(&self.workspace)?;
        if let Some(existing) = index.get(&scope).and_then(command_record_from_value) {
            if existing.fingerprint == fingerprint {
                return Ok(IdempotencyDecisionV1::Deduped(existing));
            }
            return Ok(IdempotencyDecisionV1::RejectedDifferentFingerprint(
                existing,
            ));
        }
        let record = CommandRecord {
            command_id: command.command_id.clone(),
            owner_id: command.owner_id.clone(),
            idempotency_key: command.idempotency_key.clone(),
            fingerprint,
            status: "accepted".to_string(),
        };
        append_ndjson(
            &self.agent_dir().join("state").join("commands.ndjson"),
            &command_record_to_value(&scope, &record),
        )?;
        index[&scope] = command_record_to_value(&scope, &record);
        write_json_file(&index_path, &index)?;
        Ok(IdempotencyDecisionV1::Recorded(record))
    }

    fn complete_command_with_event(
        &self,
        command_id: &str,
        status: CommandStatus,
        event: &EventEnvelopeV1,
        projection_update: ProjectionUpdate,
    ) -> Result<(), String> {
        let status_text = command_status_text(status);
        append_ndjson(
            &self.agent_dir().join("state").join("command-status.ndjson"),
            &json!({
                "command_id": command_id,
                "status": status_text,
                "updated_at": chrono::Utc::now().to_rfc3339(),
            }),
        )?;
        patch_command_status(
            &commands_index_path(&self.workspace),
            command_id,
            status_text,
        )?;
        self.append_event_and_update_projection(event, projection_update)
            .map(|_| ())
    }

    fn acquire_route_leases_and_create_session(
        &self,
        session: &SessionRecord,
        owner_lease: &OwnerLease,
        workspace_lease: Option<&WorkspaceLease>,
    ) -> Result<RouteDecisionV1, String> {
        self.upsert_owner_lease(owner_lease)?;
        if let Some(workspace_lease) = workspace_lease {
            self.upsert_workspace_lease(workspace_lease)?;
        }
        upsert_index_record(
            &self.agent_dir().join("state").join("sessions.index.json"),
            &session.session_id,
            json!({
                "session_id": session.session_id,
                "owner_id": session.owner_id,
                "workspace": session.workspace,
                "workspace_key": session.workspace_key,
                "runtime": session.runtime,
                "owner_lease": owner_lease,
                "workspace_lease": workspace_lease,
                "created_at": chrono::Utc::now().to_rfc3339(),
            }),
        )?;
        Ok(RouteDecisionV1::Created)
    }
}

fn append_file_access(
    workspace: &Path,
    session_id: &str,
    path: &str,
    access_kind: &str,
) -> Result<(), String> {
    append_ndjson(
        &workspace
            .join(".agentcall")
            .join("state")
            .join("file_access.ndjson"),
        &json!({
            "session_id": session_id,
            "path": path,
            "access_kind": access_kind,
            "ts": chrono::Utc::now().to_rfc3339(),
        }),
    )
}

fn append_ndjson(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let text = serde_json::to_string(value).map_err(|err| err.to_string())?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| err.to_string())?;
    writeln!(file, "{text}").map_err(|err| err.to_string())
}

fn append_rotating_ndjson(path: &Path, value: &Value, max_bytes: u64) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    if fs::metadata(path).map(|meta| meta.len()).unwrap_or(0) > max_bytes {
        let mut existing = fs::OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|err| err.to_string())?;
        let keep = max_bytes / 2;
        if existing.metadata().map(|meta| meta.len()).unwrap_or(0) > keep {
            existing
                .seek(SeekFrom::End(-(keep as i64)))
                .map_err(|err| err.to_string())?;
        }
        let mut tail = String::new();
        use std::io::Read;
        existing
            .read_to_string(&mut tail)
            .map_err(|err| err.to_string())?;
        if let Some(first_newline) = tail.find('\n') {
            tail = tail[first_newline + 1..].to_string();
        }
        fs::write(path, tail).map_err(|err| err.to_string())?;
    }
    append_ndjson(path, value)
}

fn upsert_index_record(path: &Path, key: &str, record: Value) -> Result<(), String> {
    let mut index = read_json_file(path, json!({}));
    if !index.is_object() {
        index = json!({});
    }
    index[key] = record;
    write_json_file(path, &index)
}

fn patch_index_record(path: &Path, key: &str, patch: Value) -> Result<(), String> {
    let mut index = read_json_file(path, json!({}));
    if !index.is_object() {
        index = json!({});
    }
    let mut record = index.get(key).cloned().unwrap_or_else(|| json!({}));
    if !record.is_object() {
        record = json!({});
    }
    if let (Some(target), Some(patch)) = (record.as_object_mut(), patch.as_object()) {
        for (key, value) in patch {
            target.insert(key.clone(), value.clone());
        }
    }
    index[key] = record;
    write_json_file(path, &index)
}

fn projection_path(workspace: &Path, session_id: &str) -> PathBuf {
    workspace
        .join(".agentcall")
        .join("state")
        .join("projections")
        .join("sessions")
        .join(format!("{}.json", safe_path_component(session_id)))
}

fn commands_index_path(workspace: &Path) -> PathBuf {
    workspace
        .join(".agentcall")
        .join("state")
        .join("commands.index.json")
}

fn commands_log_path(workspace: &Path) -> PathBuf {
    workspace
        .join(".agentcall")
        .join("state")
        .join("commands.ndjson")
}

fn command_status_log_path(workspace: &Path) -> PathBuf {
    workspace
        .join(".agentcall")
        .join("state")
        .join("command-status.ndjson")
}

fn read_commands_index(workspace: &Path) -> Result<Value, String> {
    let path = commands_index_path(workspace);
    let parsed = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(Value::is_object);
    if let Some(index) = parsed {
        return Ok(index);
    }
    let index = rebuild_commands_index(workspace)?;
    write_json_file(&path, &index)?;
    Ok(index)
}

fn rebuild_commands_index(workspace: &Path) -> Result<Value, String> {
    let mut index = json!({});
    if let Ok(text) = fs::read_to_string(commands_log_path(workspace)) {
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(scope) = value
                .get("scope")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            if command_record_from_value(&value).is_some() {
                index[&scope] = value;
            }
        }
    }
    if let Ok(text) = fs::read_to_string(command_status_log_path(workspace)) {
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(command_id) = value.get("command_id").and_then(Value::as_str) else {
                continue;
            };
            let Some(status) = value.get("status").and_then(Value::as_str) else {
                continue;
            };
            if let Some(records) = index.as_object_mut() {
                for record in records.values_mut() {
                    if record.get("command_id").and_then(Value::as_str) == Some(command_id) {
                        if let Some(object) = record.as_object_mut() {
                            object.insert("status".to_string(), json!(status));
                            if let Some(updated_at) = value.get("updated_at") {
                                object.insert("updated_at".to_string(), updated_at.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(index)
}

fn command_record_to_value(scope: &str, record: &CommandRecord) -> Value {
    json!({
        "scope": scope,
        "command_id": record.command_id,
        "owner_id": record.owner_id,
        "idempotency_key": record.idempotency_key,
        "fingerprint": record.fingerprint,
        "status": record.status,
    })
}

fn projection_matches_owner(value: &Value, owner_id: Option<&str>) -> bool {
    let Some(owner_id) = owner_id else {
        return true;
    };
    value.get("owner").and_then(Value::as_str) == Some(owner_id)
}

fn patch_command_status(path: &Path, command_id: &str, status: &str) -> Result<(), String> {
    let mut index = read_json_file(path, json!({}));
    let Some(records) = index.as_object_mut() else {
        return Ok(());
    };
    for value in records.values_mut() {
        if value.get("command_id").and_then(Value::as_str) == Some(command_id) {
            if let Some(object) = value.as_object_mut() {
                object.insert("status".to_string(), json!(status));
                object.insert(
                    "updated_at".to_string(),
                    json!(chrono::Utc::now().to_rfc3339()),
                );
            }
        }
    }
    write_json_file(path, &index)
}

fn command_status_text(status: CommandStatus) -> &'static str {
    match status {
        CommandStatus::Accepted => "accepted",
        CommandStatus::Completed => "completed",
        CommandStatus::Failed => "failed",
        CommandStatus::Rejected => "rejected",
    }
}

fn command_record_from_value(value: &Value) -> Option<CommandRecord> {
    Some(CommandRecord {
        command_id: value.get("command_id")?.as_str()?.to_string(),
        owner_id: value.get("owner_id")?.as_str()?.to_string(),
        idempotency_key: value.get("idempotency_key")?.as_str()?.to_string(),
        fingerprint: value.get("fingerprint")?.as_str()?.to_string(),
        status: value.get("status")?.as_str()?.to_string(),
    })
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

fn safe_path_component(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::CommandType;
    use crate::projection::apply_event_to_projection;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn json_store_appends_event_and_projection_together() {
        let root = test_workspace("event-projection");
        let store = JsonRuntimeStore::new(root.clone());
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
        assert!(root.join(".agentcall/events/recent.ndjson").exists());
        let projection = store.get_session_projection("worker-a").unwrap().unwrap();
        assert_eq!(projection.attention_status, "needs_permission");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn json_store_registers_command_idempotently() {
        let root = test_workspace("command-idempotency");
        let store = JsonRuntimeStore::new(root.clone());
        let command = command_for("cmd-1", "idem-1", "go");
        let first = store.register_command_idempotently(&command).unwrap();
        assert!(matches!(first, IdempotencyDecisionV1::Recorded(_)));
        let second = store.register_command_idempotently(&command).unwrap();
        assert!(matches!(second, IdempotencyDecisionV1::Deduped(_)));
        assert!(root.join(".agentcall/state/commands.ndjson").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn json_store_rebuilds_corrupt_command_index_from_logs() {
        let root = test_workspace("command-index-rebuild");
        let store = JsonRuntimeStore::new(root.clone());
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

        fs::write(commands_index_path(&root), "{not-json").unwrap();

        let rebuilt = store.get_idempotency("codex", "idem-2").unwrap().unwrap();
        assert_eq!(rebuilt.status, "completed");
        let deduped = store.register_command_idempotently(&command).unwrap();
        assert!(matches!(deduped, IdempotencyDecisionV1::Deduped(_)));
        let repaired_text = fs::read_to_string(commands_index_path(&root)).unwrap();
        assert!(repaired_text.contains("\"status\": \"completed\""));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn json_store_corrupt_projection_snapshot_is_not_returned_as_healthy() {
        let root = test_workspace("projection-corrupt");
        let store = JsonRuntimeStore::new(root.clone());
        let event = crate::events::build_event_envelope(
            "evt-000003".to_string(),
            3,
            Some(1),
            "pty.session_started",
            "started",
            json!({"session_id": "worker-a", "cwd": root.display().to_string()}),
        );
        let update = apply_event_to_projection(None, &event);
        store
            .append_event_and_update_projection(&event, update)
            .unwrap();
        fs::write(projection_path(&root, "worker-a"), "{not-json").unwrap();

        assert!(store.get_session_projection("worker-a").unwrap().is_none());
        let _ = fs::remove_dir_all(root);
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

    fn test_workspace(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("agentcall-json-store-{name}-{nonce}"))
    }
}
