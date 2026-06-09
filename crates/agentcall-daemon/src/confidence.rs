use crate::events::EventEnvelopeV1;
use crate::state::AppState;
use crate::store::EventQuery;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EvidenceItem {
    pub(crate) kind: String,
    pub(crate) source: String,
    pub(crate) path: Option<String>,
    pub(crate) status: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ConfidenceLedger {
    pub(crate) band: String,
    pub(crate) reasons: Vec<String>,
    pub(crate) evidence: Vec<EvidenceItem>,
    pub(crate) contradictions: Vec<String>,
    pub(crate) required_review_actions: Vec<String>,
}

impl ConfidenceLedger {
    pub(crate) fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or_else(|_| json!({"band": "low"}))
    }
}

pub(crate) fn attach_confidence_to_reports(state: &AppState, reports: Value) -> Value {
    let Some(items) = reports.as_array() else {
        return reports;
    };
    let enriched: Vec<Value> = items
        .iter()
        .map(|report| {
            let mut report = report.clone();
            let events = evidence_events_for_report(state, &report);
            report["confidence"] = confidence_for_report(&report, &events).to_value();
            report
        })
        .collect();
    json!(enriched)
}

pub(crate) fn confidence_for_report(
    report: &Value,
    events: &[EventEnvelopeV1],
) -> ConfidenceLedger {
    let mut evidence = vec![];
    let mut reasons = vec![];
    let mut contradictions = vec![];
    let mut required_review_actions = vec![];

    if report_path(report).is_some() {
        evidence.push(EvidenceItem {
            kind: "report_artifact_present".to_string(),
            source: "report_index".to_string(),
            path: report_path(report),
            status: report_status(report),
        });
    }

    let report_claim_success = report_claims_success(report);
    if report_claim_success {
        evidence.push(EvidenceItem {
            kind: "report_claim_success".to_string(),
            source: "structured_report_status".to_string(),
            path: report_path(report),
            status: report_status(report),
        });
    }

    for event in events {
        collect_event_evidence(event, &mut evidence);
    }

    let has_file_write = evidence.iter().any(|item| item.kind == "file_written");
    let has_test_pass = evidence.iter().any(|item| item.kind == "test_passed");
    let has_test_failed = evidence.iter().any(|item| item.kind == "test_failed");
    let has_policy_block = evidence.iter().any(|item| item.kind == "policy_block");
    let has_permission_denied = evidence.iter().any(|item| item.kind == "permission_denied");

    if report_claim_success && has_test_failed {
        contradictions.push("report claims success but daemon observed a failed test".to_string());
    }
    if report_claim_success && has_policy_block {
        contradictions.push("report claims success but daemon observed a policy block".to_string());
    }
    if report_claim_success && has_permission_denied {
        contradictions
            .push("report claims success but daemon observed permission denial".to_string());
    }

    let band = if !contradictions.is_empty() {
        required_review_actions.push("inspect_conflicting_evidence".to_string());
        "low"
    } else if report_claim_success && (has_file_write || has_test_pass) {
        reasons.push("structured report is backed by daemon-observed evidence".to_string());
        "high"
    } else if report_path(report).is_some() && !evidence.is_empty() {
        reasons.push(
            "report artifact exists but deterministic backing evidence is incomplete".to_string(),
        );
        "medium"
    } else {
        required_review_actions.push("request_daemon_observed_evidence".to_string());
        reasons.push("natural-language or unbacked report claims are low confidence".to_string());
        "low"
    }
    .to_string();

    ConfidenceLedger {
        band,
        reasons,
        evidence,
        contradictions,
        required_review_actions,
    }
}

fn evidence_events_for_report(state: &AppState, report: &Value) -> Vec<EventEnvelopeV1> {
    let session_id = report
        .get("session_id")
        .or_else(|| report.get("session"))
        .or_else(|| report.get("wrapper_session"))
        .and_then(Value::as_str)
        .map(str::to_string);
    state
        .store
        .get_events(EventQuery {
            session_id,
            after_global_seq: None,
            event_types: vec![],
            limit: 500,
        })
        .unwrap_or_default()
}

fn collect_event_evidence(event: &EventEnvelopeV1, evidence: &mut Vec<EvidenceItem>) {
    let event_type = event.event_type.as_str();
    if event_type == "policy_denial.blocked" || event_type.contains("policy.blocked") {
        evidence.push(EvidenceItem {
            kind: "policy_block".to_string(),
            source: event.event_id.clone(),
            path: None,
            status: Some("blocked".to_string()),
        });
    }
    if event_type.contains("permission_denied")
        || event
            .payload
            .get("decision")
            .and_then(|decision| decision.get("allowed"))
            .and_then(Value::as_bool)
            == Some(false)
            && event
                .payload
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .contains("permission")
    {
        evidence.push(EvidenceItem {
            kind: "permission_denied".to_string(),
            source: event.event_id.clone(),
            path: None,
            status: Some("denied".to_string()),
        });
    }

    if event_type == "hook.PostToolUse" || event_type == "hook.PreToolUse" {
        let reason = event
            .payload
            .get("decision")
            .and_then(|decision| decision.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let tool_name = event
            .payload
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or("");
        if reason.contains("write observed")
            || reason.contains("file claims acquired")
            || matches!(tool_name, "Write" | "Edit" | "MultiEdit")
        {
            for path in collect_paths(&event.payload) {
                evidence.push(EvidenceItem {
                    kind: "file_written".to_string(),
                    source: event.event_id.clone(),
                    path: Some(path),
                    status: Some("observed".to_string()),
                });
            }
        }
    }

    let status = event
        .payload
        .get("status")
        .or_else(|| event.payload.get("test_status"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    if event_type.contains("test") || event_type.contains("validation") {
        if matches!(status.as_str(), "passed" | "pass" | "ok" | "success") {
            evidence.push(EvidenceItem {
                kind: "test_passed".to_string(),
                source: event.event_id.clone(),
                path: None,
                status: Some(status),
            });
        } else if matches!(status.as_str(), "failed" | "fail" | "error") {
            evidence.push(EvidenceItem {
                kind: "test_failed".to_string(),
                source: event.event_id.clone(),
                path: None,
                status: Some(status),
            });
        }
    }
}

fn collect_paths(value: &Value) -> Vec<String> {
    let mut paths = vec![];
    for pointer in [
        "/decision/files",
        "/files",
        "/raw/tool_input/file_path",
        "/raw/tool_input/path",
        "/raw/toolInput/file_path",
        "/raw/toolInput/path",
    ] {
        let Some(value) = value.pointer(pointer) else {
            continue;
        };
        if let Some(path) = value.as_str() {
            paths.push(path.to_string());
        } else if let Some(items) = value.as_array() {
            paths.extend(items.iter().filter_map(Value::as_str).map(str::to_string));
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn report_path(report: &Value) -> Option<String> {
    report
        .get("report_path")
        .or_else(|| report.get("path"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn report_status(report: &Value) -> Option<String> {
    report
        .get("status")
        .or_else(|| report.get("verdict"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn report_claims_success(report: &Value) -> bool {
    let Some(status) = report_status(report) else {
        return false;
    };
    matches!(
        status.to_ascii_lowercase().as_str(),
        "completed" | "complete" | "accepted" | "success" | "succeeded" | "pass" | "passed" | "ok"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_matches_daemon_file_write_high_confidence() {
        let report = json!({
            "status": "completed",
            "report_path": "docs/report.md",
            "session_id": "worker-a"
        });
        let events = vec![event(
            "evt-1",
            "hook.PostToolUse",
            json!({
                "wrapper_session": "worker-a",
                "tool_name": "Write",
                "decision": {"reason": "write observed", "files": ["src/app.rs"]}
            }),
        )];
        let ledger = confidence_for_report(&report, &events);
        assert_eq!(ledger.band, "high");
        assert!(
            ledger
                .evidence
                .iter()
                .any(|item| item.kind == "file_written")
        );
        assert!(ledger.contradictions.is_empty());
    }

    #[test]
    fn policy_block_contradicts_success_claim() {
        let report = json!({
            "status": "success",
            "report_path": "docs/report.md",
            "session_id": "worker-a"
        });
        let events = vec![event(
            "evt-2",
            "policy_denial.blocked",
            json!({"wrapper_session": "worker-a", "reason": "outside allowed paths"}),
        )];
        let ledger = confidence_for_report(&report, &events);
        assert_eq!(ledger.band, "low");
        assert!(!ledger.contradictions.is_empty());
        assert!(
            ledger
                .required_review_actions
                .iter()
                .any(|item| item == "inspect_conflicting_evidence")
        );
    }

    #[test]
    fn unbacked_report_claim_stays_low_confidence() {
        let report = json!({
            "summary": "Looks good",
            "status": "completed"
        });
        let ledger = confidence_for_report(&report, &[]);
        assert_eq!(ledger.band, "low");
        assert!(
            ledger
                .required_review_actions
                .iter()
                .any(|item| item == "request_daemon_observed_evidence")
        );
    }

    fn event(id: &str, event_type: &str, payload: Value) -> EventEnvelopeV1 {
        EventEnvelopeV1 {
            schema_version: 1,
            event_id: id.to_string(),
            global_seq: 1,
            session_seq: Some(1),
            session_id: Some("worker-a".to_string()),
            run_id: None,
            owner_id: None,
            ts: "2026-06-09T00:00:00Z".to_string(),
            source: "test".to_string(),
            event_type: event_type.to_string(),
            severity: "info".to_string(),
            command_id: None,
            idempotency_key: None,
            trace_id: None,
            message: event_type.to_string(),
            payload,
        }
    }
}
