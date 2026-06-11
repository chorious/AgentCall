use crate::projection::session_projection_summary;
use crate::prompt_gate::{
    PromptGateState, PromptGateView, refresh_prompt_gate_timeouts_for_session,
};
use crate::routes::route_for_wrapper_session;
use crate::session::configured_claude_workspace;
use crate::state::AppState;
use crate::util::now_ms;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkerStateKind {
    Starting,
    PromptPending,
    PromptMissing,
    PromptCommitUnacknowledged,
    PromptSubmitted,
    Working,
    IdleAfterTurn,
    NeedsPermission,
    BlockedByPolicy,
    CheckpointDue,
    ReportRequested,
    ReportDrafting,
    ReportOverdue,
    ReportReady,
    ReportAccepted,
    Stopping,
    Done,
    Failed,
}

impl WorkerStateKind {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::PromptPending => "prompt_pending",
            Self::PromptMissing => "prompt_missing",
            Self::PromptCommitUnacknowledged => "prompt_commit_unacknowledged",
            Self::PromptSubmitted => "prompt_submitted",
            Self::Working => "working",
            Self::IdleAfterTurn => "idle_after_turn",
            Self::NeedsPermission => "needs_permission",
            Self::BlockedByPolicy => "blocked_by_policy",
            Self::CheckpointDue => "checkpoint_due",
            Self::ReportRequested => "report_requested",
            Self::ReportDrafting => "report_drafting",
            Self::ReportOverdue => "report_overdue",
            Self::ReportReady => "report_ready",
            Self::ReportAccepted => "report_accepted",
            Self::Stopping => "stopping",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct WorkerStateView {
    pub(crate) worker: String,
    pub(crate) state: WorkerStateKind,
    pub(crate) why: String,
    pub(crate) can_wait: bool,
    pub(crate) next_actions: Vec<Value>,
    pub(crate) report: Value,
    pub(crate) workspace: Value,
    pub(crate) debug_refs: Value,
    pub(crate) prompt_gate: PromptGateView,
}

impl WorkerStateView {
    pub(crate) fn to_summary_value(&self, control: Value) -> Value {
        json!({
            "schema_version": 2,
            "view": "summary",
            "worker": self.worker,
            "session": self.worker,
            "state": self.state.as_str(),
            "why": self.why,
            "can_wait": self.can_wait,
            "next_action": self.next_actions.first().and_then(|item| item.get("kind")).cloned().unwrap_or(Value::Null),
            "next_actions": self.next_actions,
            "report": self.report,
            "workspace": self.workspace,
            "pending_interaction": pending_interaction_for_state(&self.state, &self.why),
            "control": control,
            "prompt_gate": self.prompt_gate.to_value(),
            "debug_refs": self.debug_refs,
        })
    }

    pub(crate) fn to_board_worker(&self) -> Value {
        json!({
            "name": self.worker,
            "worker": self.worker,
            "state": self.state.as_str(),
            "why": self.why,
            "can_wait": self.can_wait,
            "next_action": self.next_actions.first().and_then(|item| item.get("kind")).cloned().unwrap_or(Value::Null),
            "next_actions": self.next_actions,
            "report": self.report,
            "workspace": self.workspace,
            "pending_interaction": pending_interaction_for_state(&self.state, &self.why),
        })
    }
}

pub(crate) fn worker_state_for_session(state: &AppState, session_name: &str) -> WorkerStateView {
    let projection = session_projection_summary(state, session_name);
    let prompt_gate = refresh_prompt_gate_timeouts_for_session(state, session_name);
    let route = route_for_wrapper_session(state, session_name).map(|(_, route)| route);
    let workspace = workspace_projection(state, route.as_ref());
    let mut report = report_projection_from_route(route.as_ref());
    let report_ready = projection
        .get("report_ready")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || route
            .as_ref()
            .and_then(|route| route.pointer("/result/report_ready"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    if report_ready {
        report["ready"] = json!(true);
        if report.get("status").and_then(Value::as_str) != Some("report_accepted") {
            report["status"] = json!("report_ready");
        }
    }
    let report_status = report
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("report_not_requested")
        .to_string();
    let debug_refs = json!({
        "tui": {"view": "tui"},
        "events": {"view": "events"},
        "raw": {"view": "raw"},
    });
    let attention = projection
        .get("attention_status")
        .and_then(Value::as_str)
        .unwrap_or("none");
    let liveness = projection
        .get("liveness_status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let terminal = projection
        .get("terminal")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let live = state.sessions.lock().unwrap().contains_key(session_name);

    let (state_kind, why, can_wait) = if terminal
        || matches!(liveness, "completed" | "stopped" | "killed")
        || !live
    {
        (
            WorkerStateKind::Done,
            "The daemon no longer has a live PTY worker for this session.".to_string(),
            false,
        )
    } else if matches!(liveness, "failed" | "failed_or_orphaned") || attention == "failed" {
        (
            WorkerStateKind::Failed,
            "The worker hit a terminal failure or orphaned session state.".to_string(),
            false,
        )
    } else if report_status == "report_accepted" {
        (
            WorkerStateKind::ReportAccepted,
            "The worker report was accepted by the supervisor.".to_string(),
            false,
        )
    } else if report_ready || attention == "report_ready" {
        (
            WorkerStateKind::ReportReady,
            "The worker wrote the expected report or produced report-ready evidence.".to_string(),
            false,
        )
    } else if report_is_overdue(&report) {
        (
            WorkerStateKind::ReportOverdue,
            "A report was requested but no report-ready evidence arrived before the deadline."
                .to_string(),
            false,
        )
    } else if report_status == "report_drafting" {
        (
            WorkerStateKind::ReportDrafting,
            "A report was requested and the worker has produced tool/hook progress since then."
                .to_string(),
            true,
        )
    } else if report_status == "report_requested" {
        (
            WorkerStateKind::ReportRequested,
            "A report has been requested; waiting for report write evidence.".to_string(),
            true,
        )
    } else if prompt_gate.state == PromptGateState::PromptCommitUnacknowledged {
        (
            WorkerStateKind::PromptCommitUnacknowledged,
            "A prompt commit signal was sent, but UserPromptSubmit or worker progress was not observed before the deadline.".to_string(),
            false,
        )
    } else if prompt_gate.state == PromptGateState::PromptMissing {
        (
            WorkerStateKind::PromptMissing,
            "Route prompt was written to the PTY but UserPromptSubmit was not observed before the ack deadline.".to_string(),
            false,
        )
    } else if matches!(prompt_gate.state, PromptGateState::CommitSignalSent) {
        (
            WorkerStateKind::PromptPending,
            "A prompt commit signal was sent; waiting for UserPromptSubmit or worker progress."
                .to_string(),
            true,
        )
    } else if prompt_gate.is_prompt_gate_active() {
        (
            WorkerStateKind::PromptPending,
            "PTY worker was spawned; waiting for Claude Code to emit UserPromptSubmit.".to_string(),
            true,
        )
    } else if prompt_gate.state == PromptGateState::PromptSubmitted && liveness == "unknown" {
        (
            WorkerStateKind::PromptSubmitted,
            "The route prompt was submitted; waiting for hook or tool progress.".to_string(),
            true,
        )
    } else if attention == "needs_permission" {
        (
            WorkerStateKind::NeedsPermission,
            "Claude Code is showing a permission or menu prompt.".to_string(),
            false,
        )
    } else if attention == "blocked_by_policy" {
        (
            WorkerStateKind::BlockedByPolicy,
            "The worker repeated or hit a denied policy action.".to_string(),
            false,
        )
    } else if attention == "checkpoint_due" {
        (
            WorkerStateKind::CheckpointDue,
            "Claude Code reached a checkpoint or subagent stop; inspect report/progress before continuing.".to_string(),
            false,
        )
    } else if attention == "waiting_input" || liveness == "waiting_input" {
        (
            WorkerStateKind::IdleAfterTurn,
            "Claude Code is idle after a turn; inspect report/progress before sending more text."
                .to_string(),
            false,
        )
    } else if matches!(liveness, "stopping" | "killing") {
        (
            WorkerStateKind::Stopping,
            "A stop or kill command has been dispatched; waiting for observed process exit."
                .to_string(),
            true,
        )
    } else if liveness == "working" {
        (
            WorkerStateKind::Working,
            "UserPromptSubmit was observed or the worker is running tool work.".to_string(),
            true,
        )
    } else {
        (
            WorkerStateKind::Starting,
            "The worker is starting and has not produced enough structured progress yet."
                .to_string(),
            true,
        )
    };

    let next_actions = next_actions_for_state(session_name, &state_kind, report_ready);
    WorkerStateView {
        worker: session_name.to_string(),
        state: state_kind,
        why,
        can_wait,
        next_actions,
        report,
        workspace,
        debug_refs,
        prompt_gate,
    }
}

fn workspace_projection(state: &AppState, route: Option<&Value>) -> Value {
    let target = route
        .and_then(|route| route.get("workspace"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| state.workspace.display().to_string());
    let claude_cwd = configured_claude_workspace(state)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| state.workspace.display().to_string());
    let report_workspace = route
        .and_then(|route| route.pointer("/result/report/report_workspace"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            route
                .and_then(|route| route.pointer("/result/report/abs_path"))
                .and_then(Value::as_str)
                .and_then(|path| PathBuf::from(path).parent().map(|parent| parent.display().to_string()))
        });
    json!({
        "daemon": state.workspace.display().to_string(),
        "daemon_workspace": state.workspace.display().to_string(),
        "target": target.clone(),
        "target_workspace": target,
        "claude_cwd": claude_cwd,
        "report_workspace": report_workspace.unwrap_or_else(|| state.workspace.display().to_string())
    })
}

fn report_projection_from_route(route: Option<&Value>) -> Value {
    let Some(route) = route else {
        return json!({
            "status": "report_not_requested",
            "ready": false,
            "path": Value::Null,
            "rel_path": Value::Null,
            "abs_path": Value::Null,
            "source": "none"
        });
    };
    let mut report = route
        .pointer("/result/report")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if !report.is_object() {
        report = json!({});
    }
    if report.get("status").is_none() {
        report["status"] = json!("report_not_requested");
    }
    if report.get("ready").is_none() {
        report["ready"] = json!(false);
    }
    if report.get("path").is_none() {
        report["path"] = route_report_path(route).map(Value::String).unwrap_or(Value::Null);
    }
    if report.get("rel_path").is_none() {
        report["rel_path"] = report.get("path").cloned().unwrap_or(Value::Null);
    }
    if report.get("target_workspace").is_none() {
        report["target_workspace"] = route.get("workspace").cloned().unwrap_or(Value::Null);
    }
    report
}

fn route_report_path(route: &Value) -> Option<String> {
    route
        .pointer("/result/report/path")
        .and_then(Value::as_str)
        .or_else(|| {
            route
                .pointer("/result/context_packet/report_path")
                .and_then(Value::as_str)
        })
        .or_else(|| route.pointer("/result/prompt_gate/report_path").and_then(Value::as_str))
        .or_else(|| route.pointer("/result/report_path").and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn report_is_overdue(report: &Value) -> bool {
    let status = report
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("report_not_requested");
    if !matches!(status, "report_requested" | "report_drafting") {
        return false;
    }
    report
        .get("deadline_at_ms")
        .and_then(Value::as_u64)
        .is_some_and(|deadline| deadline <= now_ms())
}

fn next_actions_for_state(
    session_name: &str,
    state: &WorkerStateKind,
    report_ready: bool,
) -> Vec<Value> {
    match state {
        WorkerStateKind::PromptMissing => vec![
            action(
                session_name,
                "submit_pending_prompt",
                "agentcall_session_send",
                false,
            ),
            action(session_name, "stop", "agentcall_session_send", true),
        ],
        WorkerStateKind::PromptCommitUnacknowledged => vec![
            action(
                session_name,
                "submit_pending_prompt",
                "agentcall_session_send",
                false,
            ),
            json!({"kind": "inspect_screen_or_retry_prompt_commit"}),
            action(session_name, "stop", "agentcall_session_send", true),
        ],
        WorkerStateKind::PromptPending | WorkerStateKind::Starting => vec![json!({"kind": "wait"})],
        WorkerStateKind::PromptSubmitted => vec![json!({"kind": "wait"})],
        WorkerStateKind::Working => vec![
            json!({"kind": "wait"}),
            action(
                session_name,
                "request_report",
                "agentcall_session_send",
                false,
            ),
        ],
        WorkerStateKind::IdleAfterTurn => vec![action(
            session_name,
            "request_report",
            "agentcall_session_send",
            false,
        )],
        WorkerStateKind::NeedsPermission => vec![
            json!({"kind": "select_option", "tool": "agentcall_session_send", "args": {"name": session_name, "action": "select_option"}, "choice_required": true}),
            action(session_name, "interrupt", "agentcall_session_send", true),
        ],
        WorkerStateKind::BlockedByPolicy => vec![
            action(
                session_name,
                "request_report",
                "agentcall_session_send",
                false,
            ),
            action(session_name, "interrupt", "agentcall_session_send", true),
            action(session_name, "stop", "agentcall_session_send", true),
        ],
        WorkerStateKind::CheckpointDue => vec![
            json!({"kind": "inspect_or_accept_report"}),
            action(
                session_name,
                "request_report",
                "agentcall_session_send",
                false,
            ),
        ],
        WorkerStateKind::ReportRequested | WorkerStateKind::ReportDrafting => {
            vec![json!({"kind": "wait"})]
        }
        WorkerStateKind::ReportOverdue => vec![
            json!({"kind": "inspect_session"}),
            action(session_name, "interrupt", "agentcall_session_send", true),
            action(session_name, "stop", "agentcall_session_send", true),
        ],
        WorkerStateKind::ReportReady => vec![
            json!({"kind": "accept_report", "tool": "agentcall_report", "args": {"action": "accept", "session_id": session_name}}),
            action(session_name, "stop", "agentcall_session_send", true),
        ],
        WorkerStateKind::ReportAccepted => {
            vec![action(session_name, "stop", "agentcall_session_send", true)]
        }
        WorkerStateKind::Stopping => vec![json!({"kind": "wait"})],
        WorkerStateKind::Done => {
            if report_ready {
                vec![
                    json!({"kind": "accept_report", "tool": "agentcall_report", "args": {"action": "accept", "session_id": session_name}}),
                ]
            } else {
                vec![]
            }
        }
        WorkerStateKind::Failed => {
            vec![action(session_name, "kill", "agentcall_session_send", true)]
        }
    }
}

fn pending_interaction_for_state(state: &WorkerStateKind, why: &str) -> Value {
    let kind = match state {
        WorkerStateKind::IdleAfterTurn => "idle_after_turn",
        WorkerStateKind::NeedsPermission => "permission_menu",
        WorkerStateKind::ReportReady => "report_written_waiting_accept",
        WorkerStateKind::ReportRequested => "report_requested",
        WorkerStateKind::ReportDrafting => "report_drafting",
        WorkerStateKind::ReportOverdue => "report_overdue",
        WorkerStateKind::CheckpointDue => "checkpoint_due",
        WorkerStateKind::PromptCommitUnacknowledged => "prompt_commit_unacknowledged",
        _ => "none",
    };
    if kind == "none" {
        Value::Null
    } else {
        json!({
            "kind": kind,
            "why": why,
        })
    }
}

fn action(session_name: &str, kind: &str, tool: &str, requires_control: bool) -> Value {
    json!({
        "kind": kind,
        "tool": tool,
        "args": {
            "name": session_name,
            "action": kind,
        },
        "requires_control_token": requires_control,
    })
}
