use crate::accepted_live::{
    accepted_live_auto_close_projection, maybe_auto_close_accepted_live_session,
};
use crate::projection::session_projection_summary;
use crate::prompt_gate::{
    PromptGateState, PromptGateView, prompt_gate_for_session,
    refresh_prompt_gate_timeouts_for_session,
};
use crate::routes::route_for_wrapper_session;
use crate::session::configured_claude_workspace;
use crate::state::AppState;
use crate::util::now_ms;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::atomic::Ordering;

const PATIENCE_FLOOR_SECONDS: u64 = 60;

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
    AcceptedLive,
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
            Self::AcceptedLive => "accepted_live",
            Self::Stopping => "stopping",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn is_terminal(&self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::ReportReady)
    }
}

#[allow(dead_code)]
pub(crate) fn worker_transition_allowed(from: &WorkerStateKind, to: &WorkerStateKind) -> bool {
    if from == to {
        return true;
    }
    match from {
        WorkerStateKind::Starting => matches!(
            to,
            WorkerStateKind::PromptPending
                | WorkerStateKind::PromptMissing
                | WorkerStateKind::PromptSubmitted
                | WorkerStateKind::Working
                | WorkerStateKind::Failed
                | WorkerStateKind::Done
        ),
        WorkerStateKind::PromptPending => matches!(
            to,
            WorkerStateKind::PromptCommitUnacknowledged
                | WorkerStateKind::PromptMissing
                | WorkerStateKind::PromptSubmitted
                | WorkerStateKind::Working
                | WorkerStateKind::Failed
                | WorkerStateKind::Done
        ),
        WorkerStateKind::PromptMissing | WorkerStateKind::PromptCommitUnacknowledged => {
            matches!(
                to,
                WorkerStateKind::PromptPending
                    | WorkerStateKind::PromptSubmitted
                    | WorkerStateKind::Working
                    | WorkerStateKind::Stopping
                    | WorkerStateKind::Failed
                    | WorkerStateKind::Done
            )
        }
        WorkerStateKind::PromptSubmitted
        | WorkerStateKind::Working
        | WorkerStateKind::IdleAfterTurn
        | WorkerStateKind::NeedsPermission
        | WorkerStateKind::BlockedByPolicy
        | WorkerStateKind::CheckpointDue => matches!(
            to,
            WorkerStateKind::Working
                | WorkerStateKind::IdleAfterTurn
                | WorkerStateKind::NeedsPermission
                | WorkerStateKind::BlockedByPolicy
                | WorkerStateKind::CheckpointDue
                | WorkerStateKind::ReportRequested
                | WorkerStateKind::ReportDrafting
                | WorkerStateKind::ReportReady
                | WorkerStateKind::ReportOverdue
                | WorkerStateKind::AcceptedLive
                | WorkerStateKind::Stopping
                | WorkerStateKind::Failed
                | WorkerStateKind::Done
        ),
        WorkerStateKind::ReportRequested | WorkerStateKind::ReportDrafting => matches!(
            to,
            WorkerStateKind::ReportDrafting
                | WorkerStateKind::ReportReady
                | WorkerStateKind::AcceptedLive
                | WorkerStateKind::ReportOverdue
                | WorkerStateKind::Stopping
                | WorkerStateKind::Failed
                | WorkerStateKind::Done
        ),
        WorkerStateKind::ReportOverdue => matches!(
            to,
            WorkerStateKind::ReportReady
                | WorkerStateKind::Stopping
                | WorkerStateKind::Failed
                | WorkerStateKind::Done
        ),
        WorkerStateKind::ReportReady => matches!(
            to,
            WorkerStateKind::AcceptedLive | WorkerStateKind::Stopping | WorkerStateKind::Done
        ),
        WorkerStateKind::AcceptedLive => {
            matches!(to, WorkerStateKind::Stopping | WorkerStateKind::Done)
        }
        WorkerStateKind::Stopping => matches!(
            to,
            WorkerStateKind::Done | WorkerStateKind::Failed | WorkerStateKind::Stopping
        ),
        WorkerStateKind::Done | WorkerStateKind::Failed => to.is_terminal(),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct WorkerStateView {
    pub(crate) worker: String,
    pub(crate) state: WorkerStateKind,
    pub(crate) why: String,
    pub(crate) can_wait: bool,
    pub(crate) primary_action: Value,
    pub(crate) available_actions: Vec<Value>,
    pub(crate) debug_actions: Vec<Value>,
    pub(crate) patience: Value,
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
            "primary_action": self.primary_action,
            "available_actions": self.available_actions,
            "debug_actions": self.debug_actions,
            "patience": self.patience,
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
            "primary_action": self.primary_action,
            "available_actions": self.available_actions,
            "debug_actions": self.debug_actions,
            "patience": self.patience,
            "report": self.report,
            "workspace": self.workspace,
            "pending_interaction": pending_interaction_for_state(&self.state, &self.why),
        })
    }
}

pub(crate) fn worker_state_for_session(state: &AppState, session_name: &str) -> WorkerStateView {
    worker_state_for_session_with_gate(
        state,
        session_name,
        refresh_prompt_gate_timeouts_for_session(state, session_name),
    )
}

pub(crate) fn worker_snapshot_for_session(state: &AppState, session_name: &str) -> WorkerStateView {
    worker_state_for_session_with_gate(
        state,
        session_name,
        prompt_gate_for_session(state, session_name),
    )
}

fn worker_state_for_session_with_gate(
    state: &AppState,
    session_name: &str,
    prompt_gate: PromptGateView,
) -> WorkerStateView {
    let _ = maybe_auto_close_accepted_live_session(state, session_name);
    let projection = session_projection_summary(state, session_name);
    let route = route_for_wrapper_session(state, session_name).map(|(_, route)| route);
    let workspace = workspace_projection(state, route.as_ref());
    let mut report = report_projection_from_route(route.as_ref());
    if let Some(route) = route.as_ref() {
        let auto_close = accepted_live_auto_close_projection(route, now_ms());
        if !auto_close.is_null() {
            report["auto_close"] = auto_close;
        }
    }
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
    let session_updated_at = {
        let sessions = state.sessions.lock().unwrap();
        sessions
            .get(session_name)
            .map(|session| session.updated_at.load(Ordering::Relaxed))
    };
    let live = session_updated_at.is_some();

    let decision = decide_worker_state(WorkerDecisionInput {
        terminal,
        live,
        liveness,
        attention,
        report_status: &report_status,
        report_ready,
        report: &report,
        prompt_gate: &prompt_gate,
    });
    let state_kind = decision.state;
    let why = decision.why;
    let can_wait = decision.can_wait;

    let last_progress_age_seconds = session_updated_at
        .map(|updated_at| now_ms().saturating_sub(updated_at) / 1000)
        .or_else(|| {
            projection
                .get("last_progress_age_seconds")
                .and_then(Value::as_u64)
        })
        .unwrap_or(0);
    let patience = patience_for_state(&state_kind, last_progress_age_seconds);
    let (primary_action, available_actions, debug_actions) =
        actions_for_state(session_name, &state_kind, report_ready, &patience);
    WorkerStateView {
        worker: session_name.to_string(),
        state: state_kind,
        why,
        can_wait,
        primary_action,
        available_actions,
        debug_actions,
        patience,
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
                .and_then(|path| {
                    PathBuf::from(path)
                        .parent()
                        .map(|parent| parent.display().to_string())
                })
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
        report["path"] = route_report_path(route)
            .map(Value::String)
            .unwrap_or(Value::Null);
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
        .or_else(|| {
            route
                .pointer("/result/prompt_gate/report_path")
                .and_then(Value::as_str)
        })
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

struct WorkerDecisionInput<'a> {
    terminal: bool,
    live: bool,
    liveness: &'a str,
    attention: &'a str,
    report_status: &'a str,
    report_ready: bool,
    report: &'a Value,
    prompt_gate: &'a PromptGateView,
}

struct WorkerDecision {
    state: WorkerStateKind,
    why: String,
    can_wait: bool,
}

fn decide_worker_state(input: WorkerDecisionInput<'_>) -> WorkerDecision {
    if input.terminal || matches!(input.liveness, "completed" | "stopped" | "killed") || !input.live
    {
        return worker_decision(
            WorkerStateKind::Done,
            "The daemon no longer has a live PTY worker for this session.",
            false,
        );
    }
    if matches!(input.liveness, "failed" | "failed_or_orphaned") || input.attention == "failed" {
        return worker_decision(
            WorkerStateKind::Failed,
            "The worker hit a terminal failure or orphaned session state.",
            false,
        );
    }
    if input.report_status == "report_accepted" {
        return worker_decision(
            WorkerStateKind::AcceptedLive,
            "The worker report was accepted, but the PTY worker is still live and occupying capacity. Stop it now, or daemon will auto-close it after the accepted-live grace period.",
            false,
        );
    }
    if input.report_ready || input.attention == "report_ready" {
        return worker_decision(
            WorkerStateKind::ReportReady,
            "The worker wrote the expected report or produced report-ready evidence.",
            false,
        );
    }
    if report_is_overdue(input.report) {
        return worker_decision(
            WorkerStateKind::ReportOverdue,
            "A report was requested but no report-ready evidence arrived before the deadline.",
            false,
        );
    }
    if input.report_status == "report_drafting" {
        return worker_decision(
            WorkerStateKind::ReportDrafting,
            "A report was requested and the worker has produced tool/hook progress since then.",
            true,
        );
    }
    if input.report_status == "report_requested" {
        return worker_decision(
            WorkerStateKind::ReportRequested,
            "A report has been requested; waiting for report write evidence.",
            true,
        );
    }
    if let Some(decision) = decide_prompt_gate_state(input.prompt_gate, input.liveness) {
        return decision;
    }
    if input.attention == "needs_permission" {
        return worker_decision(
            WorkerStateKind::NeedsPermission,
            "Claude Code is showing a permission or menu prompt.",
            false,
        );
    }
    if input.attention == "blocked_by_policy" {
        return worker_decision(
            WorkerStateKind::BlockedByPolicy,
            "The worker repeated or hit a denied policy action.",
            false,
        );
    }
    if input.attention == "checkpoint_due" {
        return worker_decision(
            WorkerStateKind::CheckpointDue,
            "Claude Code reached a checkpoint or subagent stop; inspect report/progress before continuing.",
            false,
        );
    }
    if input.attention == "waiting_input" || input.liveness == "waiting_input" {
        return worker_decision(
            WorkerStateKind::IdleAfterTurn,
            "Claude Code is idle after a turn; inspect report/progress before sending more text.",
            false,
        );
    }
    if matches!(input.liveness, "stopping" | "killing") {
        return worker_decision(
            WorkerStateKind::Stopping,
            "A stop or kill command has been dispatched; waiting for observed process exit.",
            true,
        );
    }
    if input.liveness == "working" {
        return worker_decision(
            WorkerStateKind::Working,
            "UserPromptSubmit was observed or the worker is running tool work.",
            true,
        );
    }
    worker_decision(
        WorkerStateKind::Starting,
        "The worker is starting and has not produced enough structured progress yet.",
        true,
    )
}

fn decide_prompt_gate_state(
    prompt_gate: &PromptGateView,
    liveness: &str,
) -> Option<WorkerDecision> {
    match prompt_gate.state {
        PromptGateState::PromptCommitUnacknowledged => Some(worker_decision(
            WorkerStateKind::PromptCommitUnacknowledged,
            "A prompt commit signal was sent, but UserPromptSubmit or worker progress was not observed before the deadline.",
            false,
        )),
        PromptGateState::PromptMissing => Some(worker_decision(
            WorkerStateKind::PromptMissing,
            "Route prompt was written to the PTY but UserPromptSubmit was not observed before the ack deadline; daemon auto-commit has already been attempted.",
            false,
        )),
        PromptGateState::CommitSignalSent => Some(worker_decision(
            WorkerStateKind::PromptPending,
            "A prompt commit signal was sent; waiting for UserPromptSubmit or worker progress.",
            true,
        )),
        _ if prompt_gate.is_prompt_gate_active() => Some(worker_decision(
            WorkerStateKind::PromptPending,
            "PTY worker was spawned; daemon is waiting for or auto-committing the route prompt.",
            true,
        )),
        PromptGateState::PromptSubmitted if liveness == "unknown" => Some(worker_decision(
            WorkerStateKind::PromptSubmitted,
            "The route prompt was submitted; waiting for hook or tool progress.",
            true,
        )),
        _ => None,
    }
}

fn worker_decision(state: WorkerStateKind, why: &str, can_wait: bool) -> WorkerDecision {
    WorkerDecision {
        state,
        why: why.to_string(),
        can_wait,
    }
}

fn patience_for_state(state: &WorkerStateKind, last_progress_age_seconds: u64) -> Value {
    let can_wait = matches!(
        state,
        WorkerStateKind::Starting
            | WorkerStateKind::PromptPending
            | WorkerStateKind::PromptSubmitted
            | WorkerStateKind::Working
            | WorkerStateKind::ReportRequested
            | WorkerStateKind::ReportDrafting
            | WorkerStateKind::Stopping
    );
    if !can_wait {
        return json!({
            "status": "not_waitable",
            "last_progress_age_seconds": last_progress_age_seconds,
            "suggested_wait_seconds": 0,
            "do_not_retry_before_seconds": 0,
            "hard_gate": false
        });
    }
    let remaining = PATIENCE_FLOOR_SECONDS.saturating_sub(last_progress_age_seconds);
    if remaining > 0 {
        json!({
            "status": "inside_patience_window",
            "last_progress_age_seconds": last_progress_age_seconds,
            "suggested_wait_seconds": remaining,
            "do_not_retry_before_seconds": remaining,
            "hard_gate": true,
            "hint": "Worker is inside the patience window. Wait before sending continue or requesting a report unless the user explicitly wants to close the task."
        })
    } else {
        json!({
            "status": "patience_window_elapsed",
            "last_progress_age_seconds": last_progress_age_seconds,
            "suggested_wait_seconds": 0,
            "do_not_retry_before_seconds": 0,
            "hard_gate": true
        })
    }
}

fn actions_for_state(
    session_name: &str,
    state: &WorkerStateKind,
    report_ready: bool,
    patience: &Value,
) -> (Value, Vec<Value>, Vec<Value>) {
    let wait = wait_action(patience);
    match state {
        WorkerStateKind::PromptMissing => (
            wait.clone(),
            vec![],
            vec![
                action(
                    session_name,
                    WorkerActionKind::SubmitPendingPrompt,
                    "agentcall_session_send",
                    false,
                ),
                action(
                    session_name,
                    WorkerActionKind::Stop,
                    "agentcall_session_send",
                    true,
                ),
            ],
        ),
        WorkerStateKind::PromptCommitUnacknowledged => (
            json!({"kind": "inspect_screen_or_retry_prompt_commit"}),
            vec![],
            vec![
                action(
                    session_name,
                    WorkerActionKind::SubmitPendingPrompt,
                    "agentcall_session_send",
                    false,
                ),
                action(
                    session_name,
                    WorkerActionKind::Stop,
                    "agentcall_session_send",
                    true,
                ),
            ],
        ),
        WorkerStateKind::PromptPending
        | WorkerStateKind::Starting
        | WorkerStateKind::PromptSubmitted
        | WorkerStateKind::Working
        | WorkerStateKind::Stopping => (
            wait,
            vec![],
            vec![json!({"kind": "inspect_events", "view": "events"})],
        ),
        WorkerStateKind::IdleAfterTurn => (
            action(
                session_name,
                WorkerActionKind::RequestReport,
                "agentcall_session_send",
                false,
            ),
            vec![],
            vec![json!({"kind": "inspect_events", "view": "events"})],
        ),
        WorkerStateKind::NeedsPermission => (
            json!({"kind": "select_option", "tool": "agentcall_session_send", "args": {"name": session_name, "action": "select_option"}, "choice_required": true}),
            vec![],
            vec![action(
                session_name,
                WorkerActionKind::Interrupt,
                "agentcall_session_send",
                true,
            )],
        ),
        WorkerStateKind::BlockedByPolicy => (
            action(
                session_name,
                WorkerActionKind::RequestReport,
                "agentcall_session_send",
                false,
            ),
            vec![],
            vec![
                action(
                    session_name,
                    WorkerActionKind::Interrupt,
                    "agentcall_session_send",
                    true,
                ),
                action(
                    session_name,
                    WorkerActionKind::Stop,
                    "agentcall_session_send",
                    true,
                ),
            ],
        ),
        WorkerStateKind::CheckpointDue => (
            json!({"kind": "inspect_or_accept_report"}),
            vec![action(
                session_name,
                WorkerActionKind::RequestReport,
                "agentcall_session_send",
                false,
            )],
            vec![json!({"kind": "inspect_events", "view": "events"})],
        ),
        WorkerStateKind::ReportRequested | WorkerStateKind::ReportDrafting => (
            wait,
            vec![],
            vec![json!({"kind": "inspect_events", "view": "events"})],
        ),
        WorkerStateKind::ReportOverdue => (
            json!({"kind": "inspect_session"}),
            vec![],
            vec![
                action(
                    session_name,
                    WorkerActionKind::Interrupt,
                    "agentcall_session_send",
                    true,
                ),
                action(
                    session_name,
                    WorkerActionKind::Stop,
                    "agentcall_session_send",
                    true,
                ),
            ],
        ),
        WorkerStateKind::ReportReady => (
            json!({"kind": "accept_report", "tool": "agentcall_report", "args": {"action": "accept", "session_id": session_name}}),
            vec![],
            vec![action(
                session_name,
                WorkerActionKind::Stop,
                "agentcall_session_send",
                true,
            )],
        ),
        WorkerStateKind::AcceptedLive => (
            action(
                session_name,
                WorkerActionKind::Stop,
                "agentcall_session_send",
                true,
            ),
            vec![],
            vec![],
        ),
        WorkerStateKind::Done => {
            if report_ready {
                (
                    json!({"kind": "accept_report", "tool": "agentcall_report", "args": {"action": "accept", "session_id": session_name}}),
                    vec![],
                    vec![],
                )
            } else {
                (Value::Null, vec![], vec![])
            }
        }
        WorkerStateKind::Failed => (
            action(
                session_name,
                WorkerActionKind::Kill,
                "agentcall_session_send",
                true,
            ),
            vec![],
            vec![],
        ),
    }
}

fn wait_action(patience: &Value) -> Value {
    json!({
        "kind": "wait",
        "sleep_seconds": patience
            .get("suggested_wait_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(PATIENCE_FLOOR_SECONDS)
    })
}

fn pending_interaction_for_state(state: &WorkerStateKind, why: &str) -> Value {
    let kind = match state {
        WorkerStateKind::IdleAfterTurn => "idle_after_turn",
        WorkerStateKind::NeedsPermission => "permission_menu",
        WorkerStateKind::ReportReady => "report_written_waiting_accept",
        WorkerStateKind::AcceptedLive => "accepted_live_waiting_close",
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerActionKind {
    RequestReport,
    SubmitPendingPrompt,
    Interrupt,
    Stop,
    Kill,
}

impl WorkerActionKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::RequestReport => "request_report",
            Self::SubmitPendingPrompt => "submit_pending_prompt",
            Self::Interrupt => "interrupt",
            Self::Stop => "stop",
            Self::Kill => "kill",
        }
    }
}

fn action(session_name: &str, kind: WorkerActionKind, tool: &str, requires_control: bool) -> Value {
    let kind = kind.as_str();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_transition_table_allows_expected_report_flow() {
        assert!(worker_transition_allowed(
            &WorkerStateKind::Working,
            &WorkerStateKind::ReportRequested
        ));
        assert!(worker_transition_allowed(
            &WorkerStateKind::ReportRequested,
            &WorkerStateKind::ReportDrafting
        ));
        assert!(worker_transition_allowed(
            &WorkerStateKind::ReportDrafting,
            &WorkerStateKind::ReportReady
        ));
        assert!(worker_transition_allowed(
            &WorkerStateKind::ReportReady,
            &WorkerStateKind::AcceptedLive
        ));
        assert!(worker_transition_allowed(
            &WorkerStateKind::AcceptedLive,
            &WorkerStateKind::Stopping
        ));
    }

    #[test]
    fn worker_transition_table_rejects_terminal_regression() {
        assert!(!worker_transition_allowed(
            &WorkerStateKind::Done,
            &WorkerStateKind::Working
        ));
        assert!(!worker_transition_allowed(
            &WorkerStateKind::Failed,
            &WorkerStateKind::ReportDrafting
        ));
    }
}
