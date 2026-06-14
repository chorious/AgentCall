use crate::routes::{patch_route_record, route_for_wrapper_session};
use crate::session::{get_session, request_stop_session};
use crate::state::{AppState, append_agent_event};
use crate::util::now_ms;
use serde_json::{Value, json};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub(crate) const ACCEPTED_LIVE_AUTO_CLOSE_GRACE_MS: u64 = 5 * 60 * 1000;

pub(crate) fn accepted_live_auto_close_grace_seconds() -> u64 {
    ACCEPTED_LIVE_AUTO_CLOSE_GRACE_MS / 1000
}

pub(crate) fn accepted_report_age_ms(route: &Value, now: u64) -> Option<u64> {
    if route
        .pointer("/result/report/status")
        .and_then(Value::as_str)
        != Some("report_accepted")
    {
        return None;
    }
    route
        .pointer("/result/report/accepted_at_ms")
        .and_then(Value::as_u64)
        .map(|accepted_at| now.saturating_sub(accepted_at))
}

pub(crate) fn accepted_live_auto_close_projection(route: &Value, now: u64) -> Value {
    let Some(age_ms) = accepted_report_age_ms(route, now) else {
        return Value::Null;
    };
    let remaining_ms = ACCEPTED_LIVE_AUTO_CLOSE_GRACE_MS.saturating_sub(age_ms);
    json!({
        "enabled": true,
        "grace_seconds": accepted_live_auto_close_grace_seconds(),
        "age_seconds": age_ms / 1000,
        "remaining_seconds": remaining_ms / 1000,
        "deadline_reached": age_ms >= ACCEPTED_LIVE_AUTO_CLOSE_GRACE_MS,
        "status": route
            .pointer("/result/report/auto_close/status")
            .and_then(Value::as_str)
            .unwrap_or("pending")
    })
}

pub(crate) fn schedule_accepted_live_auto_close(
    state: &Arc<AppState>,
    session_id: String,
    accepted_at_ms: u64,
) {
    let state = Arc::clone(state);
    thread::spawn(move || {
        let now = now_ms();
        let target = accepted_at_ms.saturating_add(ACCEPTED_LIVE_AUTO_CLOSE_GRACE_MS);
        if target > now {
            thread::sleep(Duration::from_millis(target - now));
        }
        let _ = maybe_auto_close_accepted_live_session(&state, &session_id);
    });
}

pub(crate) fn maybe_auto_close_accepted_live_session(
    state: &AppState,
    session_id: &str,
) -> Result<bool, String> {
    let Some((route_id, route)) = route_for_wrapper_session(state, session_id) else {
        return Ok(false);
    };
    let Some(age_ms) = accepted_report_age_ms(&route, now_ms()) else {
        return Ok(false);
    };
    if age_ms < ACCEPTED_LIVE_AUTO_CLOSE_GRACE_MS {
        return Ok(false);
    }
    if route
        .pointer("/result/report/auto_close/status")
        .and_then(Value::as_str)
        .is_some_and(|status| matches!(status, "stop_requested" | "not_live"))
    {
        return Ok(false);
    }
    if get_session(state, session_id).is_none() {
        patch_route_record(
            state,
            &route_id,
            json!({
                "updated_at": now_ms(),
                "result": {
                    "report": {
                        "auto_close": {
                            "status": "not_live",
                            "checked_at_ms": now_ms(),
                            "grace_seconds": accepted_live_auto_close_grace_seconds()
                        }
                    }
                }
            }),
        )?;
        return Ok(false);
    }
    let stop_result = request_stop_session(state, session_id)?;
    let requested_at_ms = now_ms();
    patch_route_record(
        state,
        &route_id,
        json!({
            "status": "accepted_auto_close_requested",
            "updated_at": requested_at_ms,
            "required_next_step": "wait_for_process_exit_or_inspect_session",
            "result": {
                "report": {
                    "auto_close": {
                        "status": "stop_requested",
                        "requested_at_ms": requested_at_ms,
                        "grace_seconds": accepted_live_auto_close_grace_seconds(),
                        "reason": "report_accepted_worker_still_live",
                        "stop_result": stop_result
                    }
                }
            }
        }),
    )?;
    append_agent_event(
        state,
        "report.accepted_live_auto_close",
        "Report was accepted and the live PTY worker reached the auto-close grace period.",
        json!({
            "session_id": session_id,
            "route_id": route_id,
            "grace_seconds": accepted_live_auto_close_grace_seconds()
        }),
    );
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_report_age_uses_report_status_and_timestamp() {
        let route = json!({
            "result": {
                "report": {
                    "status": "report_accepted",
                    "accepted_at_ms": 1000
                }
            }
        });
        assert_eq!(accepted_report_age_ms(&route, 2500), Some(1500));
    }

    #[test]
    fn accepted_live_projection_reports_remaining_grace() {
        let route = json!({
            "result": {
                "report": {
                    "status": "report_accepted",
                    "accepted_at_ms": 1000
                }
            }
        });
        let projection = accepted_live_auto_close_projection(&route, 61_000);
        assert_eq!(projection["enabled"], true);
        assert_eq!(projection["age_seconds"], 60);
        assert_eq!(projection["remaining_seconds"], 240);
        assert_eq!(projection["deadline_reached"], false);
    }
}
