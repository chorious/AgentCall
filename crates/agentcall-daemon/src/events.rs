use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct EventEnvelopeV1 {
    pub(crate) schema_version: u16,
    pub(crate) event_id: String,
    pub(crate) global_seq: u64,
    pub(crate) session_seq: Option<u64>,
    pub(crate) session_id: Option<String>,
    pub(crate) run_id: Option<String>,
    pub(crate) owner_id: Option<String>,
    pub(crate) ts: String,
    pub(crate) source: String,
    pub(crate) event_type: String,
    pub(crate) severity: String,
    pub(crate) command_id: Option<String>,
    pub(crate) idempotency_key: Option<String>,
    pub(crate) trace_id: Option<String>,
    pub(crate) message: String,
    pub(crate) payload: Value,
}

impl EventEnvelopeV1 {
    pub(crate) fn to_compat_json(&self) -> Value {
        serde_json::json!({
            "schema_version": self.schema_version,
            "event_id": self.event_id,
            "id": self.event_id,
            "global_seq": self.global_seq,
            "session_seq": self.session_seq,
            "session_id": self.session_id,
            "session_key": self.session_id,
            "run_id": self.run_id,
            "owner_id": self.owner_id,
            "ts": self.ts,
            "source": self.source,
            "event_type": self.event_type,
            "type": self.event_type,
            "severity": self.severity,
            "command_id": self.command_id,
            "idempotency_key": self.idempotency_key,
            "trace_id": self.trace_id,
            "task_id": null,
            "message": self.message,
            "payload": self.payload,
            "data": self.payload,
        })
    }

    pub(crate) fn from_value(value: &Value) -> Option<Self> {
        let event_id = value
            .get("event_id")
            .or_else(|| value.get("id"))?
            .as_str()?
            .to_string();
        let event_type = value
            .get("event_type")
            .or_else(|| value.get("type"))?
            .as_str()?
            .to_string();
        Some(Self {
            schema_version: value
                .get("schema_version")
                .and_then(Value::as_u64)
                .unwrap_or(1) as u16,
            event_id,
            global_seq: value.get("global_seq").and_then(Value::as_u64).unwrap_or(0),
            session_seq: value.get("session_seq").and_then(Value::as_u64),
            session_id: value
                .get("session_id")
                .or_else(|| value.get("session_key"))
                .and_then(Value::as_str)
                .map(str::to_string),
            run_id: value
                .get("run_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            owner_id: value
                .get("owner_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            ts: value
                .get("ts")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            source: value
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("daemon")
                .to_string(),
            event_type,
            severity: value
                .get("severity")
                .and_then(Value::as_str)
                .unwrap_or("info")
                .to_string(),
            command_id: value
                .get("command_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            idempotency_key: value
                .get("idempotency_key")
                .or_else(|| value.pointer("/payload/idempotency_key"))
                .or_else(|| value.pointer("/data/idempotency_key"))
                .and_then(Value::as_str)
                .map(str::to_string),
            trace_id: value
                .get("trace_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            message: value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            payload: value
                .get("payload")
                .or_else(|| value.get("data"))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        })
    }
}

pub(crate) fn build_event_envelope(
    event_id: String,
    global_seq: u64,
    session_seq: Option<u64>,
    event_type: &str,
    message: &str,
    payload: Value,
) -> EventEnvelopeV1 {
    let session_id = event_session_key(&payload);
    EventEnvelopeV1 {
        schema_version: 1,
        event_id,
        global_seq,
        session_seq,
        session_id,
        run_id: payload
            .get("run_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        owner_id: payload
            .get("owner_id")
            .or_else(|| payload.get("owner"))
            .and_then(Value::as_str)
            .map(str::to_string),
        ts: chrono::Utc::now().to_rfc3339(),
        source: event_source(event_type).to_string(),
        event_type: event_type.to_string(),
        severity: event_severity(event_type).to_string(),
        command_id: payload
            .get("command_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        idempotency_key: payload
            .get("idempotency_key")
            .and_then(Value::as_str)
            .map(str::to_string),
        trace_id: payload
            .get("trace_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        message: message.to_string(),
        payload,
    }
}

pub(crate) fn event_session_key(payload: &Value) -> Option<String> {
    for key in [
        "wrapper_session",
        "session_id",
        "session",
        "session_name",
        "route_id",
        "invocation_id",
    ] {
        let Some(value) = payload.get(key).and_then(Value::as_str) else {
            continue;
        };
        let value = value.trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn event_source(event_type: &str) -> &'static str {
    if event_type.starts_with("hook.") {
        "hook"
    } else if event_type.starts_with("mcp.") {
        "mcp"
    } else if event_type.starts_with("session.") {
        "session"
    } else {
        "daemon"
    }
}

fn event_severity(event_type: &str) -> &'static str {
    if event_type.contains("failed")
        || event_type.contains("error")
        || event_type.contains("blocked")
        || event_type.contains("denied")
    {
        "warning"
    } else {
        "info"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_envelope_roundtrips_through_compat_json() {
        let envelope = build_event_envelope(
            "evt-000001".to_string(),
            1,
            Some(1),
            "hook.Notification",
            "permission",
            serde_json::json!({
                "wrapper_session": "worker-a",
                "idempotency_key": "cmd-1",
                "status": "needs_permission"
            }),
        );
        let value = envelope.to_compat_json();
        let parsed = EventEnvelopeV1::from_value(&value).unwrap();
        assert_eq!(parsed.schema_version, 1);
        assert_eq!(parsed.event_id, "evt-000001");
        assert_eq!(parsed.event_type, "hook.Notification");
        assert_eq!(parsed.session_id.as_deref(), Some("worker-a"));
        assert_eq!(parsed.idempotency_key.as_deref(), Some("cmd-1"));
        assert_eq!(value["id"], "evt-000001");
        assert_eq!(value["type"], "hook.Notification");
        assert_eq!(value["data"]["status"], "needs_permission");
    }
}
