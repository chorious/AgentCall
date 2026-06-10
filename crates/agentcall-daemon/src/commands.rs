use crate::ownership::{attach_or_validate_owner_lease, validate_owner_lease_precondition};
use crate::projection::read_session_projection;
use crate::state::{AppState, append_agent_event};
#[cfg(test)]
use crate::state::{read_json_file, write_json_file};
use crate::store::IdempotencyDecisionV1;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(test)]
use std::fs;
#[cfg(test)]
use std::io::Write;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CommandEnvelopeV1 {
    pub(crate) schema_version: u16,
    pub(crate) command_id: String,
    pub(crate) session_id: String,
    pub(crate) run_id: Option<String>,
    pub(crate) owner_id: String,
    pub(crate) owner_lease_id: String,
    pub(crate) lease_generation: u64,
    pub(crate) idempotency_key: String,
    pub(crate) command_type: CommandType,
    pub(crate) payload: Value,
    pub(crate) precondition: Option<CommandPrecondition>,
    pub(crate) created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum CommandType {
    SendInput,
    QueueSupervisorInstruction,
    SelectOption,
    InterruptTurn,
    CancelCommand,
    StopSession,
    KillSession,
    RequestReport,
    RefreshProjection,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct CommandPrecondition {
    pub(crate) projection_last_session_seq: Option<u64>,
    pub(crate) turn_state: Option<String>,
    pub(crate) owner_id: Option<String>,
    pub(crate) owner_lease_id: Option<String>,
    pub(crate) lease_generation: Option<u64>,
}

pub(crate) struct SessionSendSafety {
    pub(crate) requires_idempotency: bool,
    pub(crate) requires_precondition: bool,
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) enum IdempotencyDecision {
    Recorded,
    Deduped(Value),
}

pub(crate) enum PreparedCommand {
    Submit(CommandEnvelopeV1),
    Deduped(Value),
}

pub(crate) fn prepare_session_send_command(
    state: &AppState,
    session: &str,
    action: &str,
    args: &Value,
) -> Result<PreparedCommand, String> {
    let safety = classify_session_send_action(action);
    let idempotency_key = args
        .get("idempotency_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if safety.requires_idempotency && idempotency_key.is_none() {
        return Err(format!(
            "rejected_missing_idempotency_key: action={action} requires idempotency_key"
        ));
    }
    if safety.requires_precondition {
        validate_required_destructive_precondition(state, session, action, args)?;
    }
    let idempotency_key = idempotency_key.unwrap_or("read-only");
    let enriched_args = attach_or_validate_owner_lease(state, session, args)?;
    let command = build_session_send_command(session, action, idempotency_key, &enriched_args);
    validate_projection_precondition(state, &command)?;
    match state.store.register_command_idempotently(&command)? {
        IdempotencyDecisionV1::Recorded(_) => Ok(PreparedCommand::Submit(command)),
        IdempotencyDecisionV1::Deduped(previous) => Ok(PreparedCommand::Deduped(json!({
            "ok": true,
            "status": "command_deduped",
            "idempotency_key": idempotency_key,
            "previous": {
                "command_id": previous.command_id,
                "owner_id": previous.owner_id,
                "status": previous.status,
            }
        }))),
        IdempotencyDecisionV1::RejectedDifferentFingerprint(_) => Err(format!(
            "rejected_idempotency_key_reuse_with_different_payload: key={idempotency_key}"
        )),
    }
}

fn validate_projection_precondition(
    state: &AppState,
    command: &CommandEnvelopeV1,
) -> Result<(), String> {
    if let Some(precondition) = command.precondition.as_ref() {
        if let Some(expected_owner_id) = precondition.owner_id.as_deref() {
            if expected_owner_id != command.owner_id {
                return Err(format!(
                    "rejected_owner_mismatch: expected owner_id={} got={}",
                    command.owner_id, expected_owner_id
                ));
            }
        }
        if let Some(expected_lease_id) = precondition.owner_lease_id.as_deref() {
            if expected_lease_id != command.owner_lease_id {
                return Err(format!(
                    "rejected_stale_lease: expected owner_lease_id={} got={}",
                    command.owner_lease_id, expected_lease_id
                ));
            }
        }
        if let Some(expected_generation) = precondition.lease_generation {
            if expected_generation != command.lease_generation {
                return Err(format!(
                    "rejected_stale_lease_generation: expected lease_generation={} got={}",
                    command.lease_generation, expected_generation
                ));
            }
        }
    }
    let Some(provided_seq) = command
        .precondition
        .as_ref()
        .and_then(|precondition| precondition.projection_last_session_seq)
    else {
        return Ok(());
    };
    let current_seq = read_session_projection(state, &command.session_id)
        .map(|projection| projection.projection_last_session_seq)
        .unwrap_or(0);
    if provided_seq == current_seq {
        return Ok(());
    }
    append_agent_event(
        state,
        "command.rejected_precondition",
        "Session command rejected because projection precondition is stale.",
        json!({
            "session_id": command.session_id,
            "command_id": command.command_id,
            "idempotency_key": command.idempotency_key,
            "owner_id": command.owner_id,
            "expected_projection_last_session_seq": current_seq,
            "provided_projection_last_session_seq": provided_seq,
            "reason": "rejected_stale_projection"
        }),
    );
    Err(format!(
        "rejected_stale_projection: projection_last_session_seq expected={current_seq} got={provided_seq}"
    ))
}

fn validate_required_destructive_precondition(
    state: &AppState,
    session: &str,
    action: &str,
    args: &Value,
) -> Result<(), String> {
    let Some(precondition) = args.get("precondition").and_then(Value::as_object) else {
        return Err(format!(
            "rejected_missing_precondition: action={action} requires precondition"
        ));
    };
    let mut missing = Vec::new();
    let projection_seq = precondition
        .get("projection_last_session_seq")
        .and_then(Value::as_u64)
        .or_else(|| {
            missing.push("projection_last_session_seq");
            None
        });
    let owner_id = precondition
        .get("owner_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            missing.push("owner_id");
            None
        });
    let owner_lease_id = precondition
        .get("owner_lease_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            missing.push("owner_lease_id");
            None
        });
    let lease_generation = precondition
        .get("lease_generation")
        .and_then(Value::as_u64)
        .or_else(|| {
            missing.push("lease_generation");
            None
        });
    if !missing.is_empty() {
        return Err(format!(
            "rejected_missing_precondition: action={action} missing={}",
            missing.join(",")
        ));
    }
    let _ = projection_seq;
    validate_owner_lease_precondition(
        state,
        session,
        owner_id.unwrap(),
        owner_lease_id.unwrap(),
        lease_generation.unwrap(),
    )
}

pub(crate) fn classify_session_send_action(action: &str) -> SessionSendSafety {
    match action {
        "stop" | "kill" | "interrupt" | "approve_plan" | "start_auto" => SessionSendSafety {
            requires_idempotency: true,
            requires_precondition: true,
        },
        "send" | "continue" | "request_report" | "revise_plan" | "select_option" => {
            SessionSendSafety {
                requires_idempotency: true,
                requires_precondition: false,
            }
        }
        _ => SessionSendSafety {
            requires_idempotency: true,
            requires_precondition: false,
        },
    }
}

pub(crate) fn command_type_for_session_send(action: &str) -> CommandType {
    match action {
        "stop" => CommandType::StopSession,
        "kill" => CommandType::KillSession,
        "interrupt" => CommandType::InterruptTurn,
        "select_option" => CommandType::SelectOption,
        "request_report" => CommandType::RequestReport,
        "continue" | "send" | "revise_plan" | "approve_plan" | "start_auto" => {
            CommandType::SendInput
        }
        _ => CommandType::SendInput,
    }
}

pub(crate) fn build_session_send_command(
    session: &str,
    action: &str,
    idempotency_key: &str,
    args: &Value,
) -> CommandEnvelopeV1 {
    let precondition = args
        .get("precondition")
        .cloned()
        .and_then(|value| serde_json::from_value::<CommandPrecondition>(value).ok());
    let owner_id = args
        .get("owner_id")
        .and_then(Value::as_str)
        .unwrap_or("codex")
        .to_string();
    let owner_lease_id = precondition
        .as_ref()
        .and_then(|value| value.owner_lease_id.clone())
        .or_else(|| {
            args.get("owner_lease_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unleased".to_string());
    let lease_generation = precondition
        .as_ref()
        .and_then(|value| value.lease_generation)
        .or_else(|| args.get("lease_generation").and_then(Value::as_u64))
        .unwrap_or(0);
    CommandEnvelopeV1 {
        schema_version: 1,
        command_id: format!(
            "cmd-{}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ),
        session_id: session.to_string(),
        run_id: args
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        owner_id,
        owner_lease_id,
        lease_generation,
        idempotency_key: idempotency_key.to_string(),
        command_type: command_type_for_session_send(action),
        payload: session_send_payload(action, args),
        precondition,
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

pub(crate) fn session_send_payload(action: &str, args: &Value) -> Value {
    let mut payload = serde_json::Map::new();
    payload.insert("action".to_string(), json!(action));
    if let Some(text) = args.get("text").and_then(Value::as_str) {
        payload.insert("text".to_string(), json!(text));
    }
    if let Some(enter) = args.get("enter").and_then(Value::as_bool) {
        payload.insert("enter".to_string(), json!(enter));
    }
    Value::Object(payload)
}

#[cfg(test)]
pub(crate) fn check_or_record_idempotency(
    state: &AppState,
    session: &str,
    key: &str,
    fingerprint: &str,
) -> Result<IdempotencyDecision, String> {
    let _guard = state.state_writer.lock().unwrap();
    let index_path = commands_index_path(state);
    let log_path = commands_log_path(state);
    let mut ledger = read_json_file(&index_path, json!(null));
    if !ledger.is_object() {
        ledger = rebuild_commands_index_from_log(&log_path);
        write_json_file(&index_path, &ledger)?;
    }
    if !ledger.is_object() {
        ledger = json!({});
    }
    let scope = format!("session_send:{session}:{key}");
    if let Some(previous) = ledger.get(&scope) {
        let previous_fingerprint = previous
            .get("fingerprint")
            .and_then(Value::as_str)
            .unwrap_or("");
        if previous_fingerprint != fingerprint {
            append_command_registry_line(
                &log_path,
                &json!({
                    "scope": scope,
                    "session": session,
                    "idempotency_key": key,
                    "fingerprint": fingerprint,
                    "status": "rejected",
                    "reason": "different_payload",
                    "created_at": chrono::Utc::now().to_rfc3339()
                }),
            )?;
            return Err(format!(
                "rejected_idempotency_key_reuse_with_different_payload: key={key}"
            ));
        }
        append_command_registry_line(
            &log_path,
            &json!({
                "scope": scope,
                "session": session,
                "idempotency_key": key,
                "fingerprint": fingerprint,
                "status": "deduped",
                "created_at": chrono::Utc::now().to_rfc3339()
            }),
        )?;
        return Ok(IdempotencyDecision::Deduped(previous.clone()));
    }
    let record = json!({
        "scope": scope,
        "session": session,
        "idempotency_key": key,
        "fingerprint": fingerprint,
        "status": "accepted",
        "recorded_at": chrono::Utc::now().to_rfc3339()
    });
    append_command_registry_line(&log_path, &record)?;
    ledger[&scope] = record;
    write_json_file(&index_path, &ledger)?;
    Ok(IdempotencyDecision::Recorded)
}

#[cfg(test)]
fn commands_log_path(state: &AppState) -> std::path::PathBuf {
    state
        .workspace
        .join(".agentcall")
        .join("state")
        .join("commands.ndjson")
}

#[cfg(test)]
fn commands_index_path(state: &AppState) -> std::path::PathBuf {
    state
        .workspace
        .join(".agentcall")
        .join("state")
        .join("commands.index.json")
}

#[cfg(test)]
fn append_command_registry_line(
    path: &std::path::Path,
    record: &serde_json::Value,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|err| err.to_string())?;
    let text = serde_json::to_string(record).map_err(|err| err.to_string())?;
    writeln!(file, "{text}").map_err(|err| err.to_string())
}

#[cfg(test)]
fn rebuild_commands_index_from_log(path: &std::path::Path) -> serde_json::Value {
    let Ok(text) = fs::read_to_string(path) else {
        return json!({});
    };
    let mut index = serde_json::Map::new();
    for line in text.lines() {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if record.get("status").and_then(Value::as_str) != Some("accepted") {
            continue;
        }
        let Some(scope) = record.get("scope").and_then(Value::as_str) else {
            continue;
        };
        index.insert(scope.to_string(), record);
    }
    Value::Object(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projection::stale_projection_for_session_name;
    use crate::util::now_ms;
    use std::fs;

    fn test_state(name: &str) -> AppState {
        let root = std::env::temp_dir().join(format!(
            "agentcall-commands-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".agentcall").join("state")).unwrap();
        AppState::test(root)
    }

    #[test]
    fn session_send_safety_classifies_destructive_actions() {
        let stop = classify_session_send_action("stop");
        assert!(stop.requires_idempotency);
        assert!(stop.requires_precondition);

        let kill = classify_session_send_action("kill");
        assert!(kill.requires_idempotency);
        assert!(kill.requires_precondition);

        let interrupt = classify_session_send_action("interrupt");
        assert!(interrupt.requires_idempotency);
        assert!(interrupt.requires_precondition);

        let approve_plan = classify_session_send_action("approve_plan");
        assert!(approve_plan.requires_idempotency);
        assert!(approve_plan.requires_precondition);

        let start_auto = classify_session_send_action("start_auto");
        assert!(start_auto.requires_idempotency);
        assert!(start_auto.requires_precondition);

        let send = classify_session_send_action("send");
        assert!(send.requires_idempotency);
        assert!(!send.requires_precondition);
    }

    #[test]
    fn session_send_idempotency_dedupes_and_rejects_key_reuse() {
        let state = test_state("idempotency");
        let first = check_or_record_idempotency(&state, "worker-a", "cmd-1", "payload-a").unwrap();
        assert!(matches!(first, IdempotencyDecision::Recorded));

        let second = check_or_record_idempotency(&state, "worker-a", "cmd-1", "payload-a").unwrap();
        let IdempotencyDecision::Deduped(previous) = second else {
            panic!("expected dedupe");
        };
        assert_eq!(previous["fingerprint"], "payload-a");

        let reused =
            check_or_record_idempotency(&state, "worker-a", "cmd-1", "payload-b").unwrap_err();
        assert!(reused.contains("different_payload"));
        let _ = fs::remove_dir_all(&state.workspace);
    }

    #[test]
    fn commands_index_rebuilds_from_append_only_log() {
        let state = test_state("commands-index-rebuild");
        let first = check_or_record_idempotency(&state, "worker-a", "cmd-1", "payload-a").unwrap();
        assert!(matches!(first, IdempotencyDecision::Recorded));

        fs::remove_file(commands_index_path(&state)).unwrap();
        let rebuilt =
            check_or_record_idempotency(&state, "worker-a", "cmd-1", "payload-a").unwrap();
        let IdempotencyDecision::Deduped(previous) = rebuilt else {
            panic!("expected rebuilt dedupe");
        };
        assert_eq!(previous["fingerprint"], "payload-a");

        let log_text = fs::read_to_string(commands_log_path(&state)).unwrap();
        assert!(log_text.contains(r#""status":"accepted""#));
        assert!(log_text.contains(r#""status":"deduped""#));
        let _ = fs::remove_dir_all(&state.workspace);
    }

    #[test]
    fn session_send_action_maps_to_command_type() {
        assert_eq!(
            command_type_for_session_send("interrupt"),
            CommandType::InterruptTurn
        );
        assert_eq!(
            command_type_for_session_send("kill"),
            CommandType::KillSession
        );
        assert_eq!(
            command_type_for_session_send("select_option"),
            CommandType::SelectOption
        );
        assert_eq!(
            command_type_for_session_send("request_report"),
            CommandType::RequestReport
        );
    }

    #[test]
    fn command_envelope_v1_carries_owner_and_precondition() {
        let envelope = CommandEnvelopeV1 {
            schema_version: 1,
            command_id: "cmd-1".to_string(),
            session_id: "worker-a".to_string(),
            run_id: Some("run-1".to_string()),
            owner_id: "codex-main".to_string(),
            owner_lease_id: "lease-1".to_string(),
            lease_generation: 3,
            idempotency_key: "idem-1".to_string(),
            command_type: CommandType::SendInput,
            payload: serde_json::json!({"text": "continue"}),
            precondition: Some(CommandPrecondition {
                projection_last_session_seq: Some(42),
                turn_state: Some("Idle".to_string()),
                owner_id: Some("codex-main".to_string()),
                owner_lease_id: Some("lease-1".to_string()),
                lease_generation: Some(3),
            }),
            created_at: "2026-06-09T00:00:00Z".to_string(),
        };
        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["command_type"], "SendInput");
        assert_eq!(value["owner_lease_id"], "lease-1");
        assert_eq!(value["precondition"]["projection_last_session_seq"], 42);
    }

    #[test]
    fn session_send_precondition_seq_mismatch_rejected() {
        let state = test_state("precondition-mismatch");
        let mut projection = stale_projection_for_session_name("worker-a");
        projection.projection_stale = false;
        projection.projection_last_session_seq = 7;
        state
            .projections
            .lock()
            .unwrap()
            .insert("worker-a".to_string(), projection);
        let err = match prepare_session_send_command(
            &state,
            "worker-a",
            "send",
            &serde_json::json!({
                "text": "continue",
                "idempotency_key": "stale-send",
                "precondition": {"projection_last_session_seq": 6}
            }),
        ) {
            Ok(_) => panic!("expected stale projection precondition to be rejected"),
            Err(err) => err,
        };
        assert!(err.contains("rejected_stale_projection"));
        let _ = fs::remove_dir_all(&state.workspace);
    }

    #[test]
    fn destructive_plan_transition_requires_precondition() {
        let state = test_state("plan-transition-precondition");
        let err = match prepare_session_send_command(
            &state,
            "worker-a",
            "start_auto",
            &serde_json::json!({
                "text": "1",
                "idempotency_key": "start-auto-without-precondition"
            }),
        ) {
            Ok(_) => panic!("expected start_auto without precondition to be rejected"),
            Err(err) => err,
        };
        assert!(err.contains("rejected_missing_precondition"));
        let err = match prepare_session_send_command(
            &state,
            "worker-a",
            "start_auto",
            &serde_json::json!({
                "text": "1",
                "idempotency_key": "start-auto-empty-precondition",
                "precondition": {}
            }),
        ) {
            Ok(_) => panic!("expected start_auto with empty precondition to be rejected"),
            Err(err) => err,
        };
        assert!(err.contains("projection_last_session_seq"));
        assert!(err.contains("owner_id"));
        assert!(err.contains("owner_lease_id"));
        assert!(err.contains("lease_generation"));
        let _ = fs::remove_dir_all(&state.workspace);
    }

    #[test]
    fn destructive_command_requires_current_owner_lease_precondition() {
        let state = test_state("destructive-current-lease");
        let _ = attach_or_validate_owner_lease(&state, "worker-a", &json!({})).unwrap();
        let mut projection = stale_projection_for_session_name("worker-a");
        projection.projection_stale = false;
        projection.projection_last_session_seq = 7;
        state
            .projections
            .lock()
            .unwrap()
            .insert("worker-a".to_string(), projection);

        let prepared = prepare_session_send_command(
            &state,
            "worker-a",
            "kill",
            &serde_json::json!({
                "idempotency_key": "kill-current",
                "owner_id": "codex",
                "precondition": {
                    "projection_last_session_seq": 7,
                    "owner_id": "codex",
                    "owner_lease_id": "lease-worker-a-1",
                    "lease_generation": 1
                }
            }),
        )
        .unwrap();
        assert!(matches!(prepared, PreparedCommand::Submit(_)));
        let _ = fs::remove_dir_all(&state.workspace);
    }

    #[test]
    fn destructive_command_rejects_owner_mismatch() {
        let state = test_state("destructive-owner-mismatch");
        let _ = attach_or_validate_owner_lease(&state, "worker-a", &json!({})).unwrap();
        let err = match prepare_session_send_command(
            &state,
            "worker-a",
            "kill",
            &serde_json::json!({
                "idempotency_key": "kill-owner-mismatch",
                "precondition": {
                    "projection_last_session_seq": 0,
                    "owner_id": "other-codex",
                    "owner_lease_id": "lease-worker-a-1",
                    "lease_generation": 1
                }
            }),
        ) {
            Ok(_) => panic!("expected owner mismatch rejection"),
            Err(err) => err,
        };
        assert!(err.contains("rejected_owner_mismatch"));
        let _ = fs::remove_dir_all(&state.workspace);
    }

    #[test]
    fn precondition_match_allows_command() {
        let state = test_state("precondition-match");
        let mut projection = stale_projection_for_session_name("worker-a");
        projection.projection_stale = false;
        projection.projection_last_session_seq = 7;
        state
            .projections
            .lock()
            .unwrap()
            .insert("worker-a".to_string(), projection);
        let prepared = prepare_session_send_command(
            &state,
            "worker-a",
            "send",
            &serde_json::json!({
                "text": "continue",
                "idempotency_key": "fresh-send",
                "precondition": {"projection_last_session_seq": 7}
            }),
        )
        .unwrap();
        assert!(matches!(prepared, PreparedCommand::Submit(_)));
        let _ = fs::remove_dir_all(&state.workspace);
    }
}
