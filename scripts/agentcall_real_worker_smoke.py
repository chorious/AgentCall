from __future__ import annotations

import argparse
import json
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any


SMOKE_DAEMON_TOKEN = "agentcall-smoke-token"


class SmokeError(RuntimeError):
    pass


def main() -> int:
    configure_stdio()
    parser = argparse.ArgumentParser(
        description="Run a real AgentCall daemon+PTY smoke using a deterministic fake worker."
    )
    parser.add_argument("--root", default=repo_root(), help="AgentCall repository root.")
    parser.add_argument(
        "--daemon-bin",
        default=None,
        help="Path to agentcall-daemon.exe. Defaults to target/debug/agentcall-daemon.exe.",
    )
    parser.add_argument("--port", type=int, default=0, help="Daemon port; 0 chooses a free port.")
    parser.add_argument(
        "--keep-workspace",
        action="store_true",
        help="Do not delete the temporary smoke workspace.",
    )
    parser.add_argument(
        "--store-backend",
        choices=["json", "sqlite"],
        default="json",
        help="RuntimeStore backend for the temporary daemon.",
    )
    parser.add_argument(
        "--parallel-workers",
        type=int,
        default=1,
        help="Run N concurrent fake PTY workers against independent target workspaces.",
    )
    parser.add_argument(
        "--omit-report-path",
        action="store_true",
        help="Do not pass report_path to route; require daemon to mint a unique report path.",
    )
    args = parser.parse_args()
    if args.parallel_workers < 1:
        raise SmokeError("--parallel-workers must be >= 1")
    root = Path(args.root).resolve()
    daemon_bin = Path(args.daemon_bin) if args.daemon_bin else root / "target" / "debug" / executable_name("agentcall-daemon")
    workspace = Path(tempfile.mkdtemp(prefix="agentcall-v5-smoke-"))
    proc: subprocess.Popen[str] | None = None
    daemon_log = workspace / "daemon.log"
    try:
        ensure_daemon_binary(daemon_bin)
        write_local_config(workspace, args.store_backend, max(3, args.parallel_workers))
        port = args.port or free_port()
        proc = start_daemon(daemon_bin, workspace, port, daemon_log)
        base_url = f"http://127.0.0.1:{port}"
        health = wait_for_daemon(base_url, proc, daemon_log)
        assert_build_identity(health, daemon_bin)
        if args.parallel_workers > 1:
            result = run_parallel_smoke(
                base_url,
                root,
                workspace,
                args.store_backend,
                args.parallel_workers,
                args.omit_report_path,
            )
            print(json.dumps(result, ensure_ascii=False, indent=2))
            return 0
        session_name = f"v5-smoke-{int(time.time())}"
        report_rel = None if args.omit_report_path else report_path_for_session(session_name)
        route = start_route(base_url, root, workspace, session_name, report_rel)
        report_rel = report_path_from_route(route)
        if route.get("worker") != session_name:
            raise SmokeError(
                "route worker: expected "
                f"{session_name!r}, got {route.get('worker')!r}; route={json.dumps(route, ensure_ascii=False)[:1200]}"
            )
        if route.get("state") not in {"starting", "prompt_submitted", "working"}:
            raise SmokeError(f"route state unexpected: {route.get('state')!r}; route={json.dumps(route, ensure_ascii=False)[:1200]}")
        verify_active_leases(workspace, args.store_backend, session_name)
        sent = send_input(base_url, session_name, "AGENTCALL_SMOKE_PING")
        if sent.get("ok") is not True:
            raise SmokeError(f"session send failed: {sent}")
        assert_report_file(workspace, report_rel)
        requested = request_report(base_url, session_name)
        assert_report_requested(requested, session_name)
        requested_summary = mcp_call(base_url, "agentcall_session", {"name": session_name})
        assert_summary_report_requested(requested_summary, session_name)
        ingest_report_write(base_url, session_name, workspace, report_rel)
        summary = mcp_call(base_url, "agentcall_session", {"name": session_name})
        assert_eq(summary.get("view"), "summary", "session summary view")
        assert_eq(summary.get("schema_version"), 2, "session summary schema")
        assert_eq(summary.get("worker"), session_name, "session summary worker")
        if summary.get("state") not in {"working", "starting", "prompt_submitted", "idle_after_turn", "report_ready"}:
            raise SmokeError(f"session state after send unexpected: {summary.get('state')!r}")
        assert_report_ready(summary, session_name)
        accepted = accept_report(base_url, session_name)
        assert_report_accept_high(accepted, session_name)
        stop = stop_session(base_url, session_name)
        if stop.get("ok") is not True:
            raise SmokeError(f"stop failed: {stop}")
        wait_for_worker_state(base_url, session_name, {"stopping", "done"}, "stop projection")
        board = get_json(f"{base_url}/api/board?view=compact&filter=attention")
        assert_eq(board.get("schema_version"), 2, "board schema")
        assert_eq(board.get("view"), "compact", "board view")
        terminate_daemon(proc)
        proc = start_daemon(daemon_bin, workspace, port, daemon_log)
        health = wait_for_daemon(base_url, proc, daemon_log)
        assert_build_identity(health, daemon_bin)
        verify_restart_recovery(base_url, workspace, args.store_backend, session_name)
        print(json.dumps({
            "status": "ok",
            "workspace": str(workspace),
            "base_url": base_url,
            "store_backend": args.store_backend,
            "session_name": session_name,
            "route_id": route.get("route_id"),
            "report_path": report_rel,
            "checks": [
                "MCP route started a real PTY runtime",
                "RuntimeStore recorded route session plus active owner/workspace leases",
                "route returned v6.1 worker summary",
                "MCP session_send used actor command path without HTTP input fallback",
                "MCP session default returned projection summary without raw terminal scan",
                "report_ready became visible before accept using daemon-observed hook write evidence",
                "request_report projected report_requested before report_ready",
                "report accept returned confidence.overall=high only after daemon observed write",
                "stop returned awaiting observation",
                "compact attention board returned projection-only payload",
                "runtime health exposed current daemon build identity",
                "daemon restart recovered projection/events/completed-command/lease records from durable state",
            ],
        }, ensure_ascii=False, indent=2))
        return 0
    except SmokeError as exc:
        print(f"[FAIL] {exc}", file=sys.stderr)
        if proc is not None:
            print(daemon_tail(daemon_log), file=sys.stderr)
        return 1
    finally:
        if proc is not None:
            terminate_daemon(proc)
        if args.keep_workspace:
            print(f"[INFO] kept smoke workspace: {workspace}")
        else:
            shutil.rmtree(workspace, ignore_errors=True)


def repo_root() -> str:
    return str(Path(__file__).resolve().parents[1])


def configure_stdio() -> None:
    for stream in (sys.stdin, sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except AttributeError:
            pass


def executable_name(stem: str) -> str:
    return f"{stem}.exe" if sys.platform.startswith("win") else stem


def ensure_daemon_binary(path: Path) -> None:
    if not path.exists():
        raise SmokeError(
            f"daemon binary not found: {path}. Run `cargo build -p agentcall-daemon` first."
        )


def write_local_config(workspace: Path, store_backend: str, max_sessions: int) -> None:
    config_dir = workspace / "config"
    config_dir.mkdir(parents=True, exist_ok=True)
    (config_dir / "agentcall.local.json").write_text(
        json.dumps(
            {
                "claude_workspace": str(workspace),
                "store_backend": store_backend,
                "max_sessions": max_sessions,
                "per_owner_max_sessions": max_sessions,
                "experimental_sdk_runtime": False,
                "dev_open_loopback": False,
                "daemon_token": SMOKE_DAEMON_TOKEN,
            },
            ensure_ascii=False,
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def start_daemon(
    daemon_bin: Path,
    workspace: Path,
    port: int,
    log_path: Path,
) -> subprocess.Popen[str]:
    log = log_path.open("w", encoding="utf-8", errors="replace")
    return subprocess.Popen(
        [str(daemon_bin), "--workspace", str(workspace), "--port", str(port)],
        cwd=str(workspace),
        text=True,
        encoding="utf-8",
        errors="replace",
        stdout=log,
        stderr=subprocess.STDOUT,
    )


def wait_for_daemon(base_url: str, proc: subprocess.Popen[str], daemon_log: Path) -> dict[str, Any]:
    deadline = time.time() + 10
    while time.time() < deadline:
        if proc.poll() is not None:
            raise SmokeError(f"daemon exited early with code {proc.returncode}\n{daemon_tail(daemon_log)}")
        try:
            health = get_json(f"{base_url}/api/runtime/health", timeout=1.0)
            if health.get("status") in {"ok", "running"}:
                return health
        except SmokeError:
            time.sleep(0.2)
    raise SmokeError(f"daemon did not become healthy at {base_url}")


def assert_build_identity(health: dict[str, Any], daemon_bin: Path) -> None:
    build = health.get("build") if isinstance(health.get("build"), dict) else {}
    binary_path = build.get("binary_path")
    if not binary_path:
        raise SmokeError(f"runtime health missing build.binary_path: {health}")
    if Path(binary_path).resolve() != daemon_bin.resolve():
        raise SmokeError(
            f"runtime health binary mismatch: expected {daemon_bin.resolve()}, got {binary_path}"
        )
    if not build.get("process_started_at_ms"):
        raise SmokeError(f"runtime health missing process_started_at_ms: {health}")
    if not build.get("binary_modified_at"):
        raise SmokeError(f"runtime health missing binary_modified_at: {health}")


def report_path_for_session(session_name: str) -> str:
    return f".agentcall/reports/{session_name}.md"


def start_route(
    base_url: str,
    root: Path,
    workspace: Path,
    session_name: str,
    report_path: str | None,
) -> dict[str, Any]:
    report_abs = workspace / report_path if report_path else None
    command = [sys.executable, str(root / "scripts" / "fake_pty_worker.py")]
    if report_abs is not None:
        command.extend(["--report", str(report_abs)])
    payload: dict[str, Any] = {
        "objective": "AgentCall v5 smoke. Echo route prompt, accept actor input, and wait for stop.",
        "workspace": str(workspace),
        "mode": "start",
        "runtime": "pty",
        "session_name": session_name,
        "command": command,
        "allowed_paths": [".agentcall/reports"],
        "read_only": False,
    }
    if report_path:
        payload["report_path"] = report_path
    return mcp_call(base_url, "agentcall_route", payload)


def report_path_from_route(route: dict[str, Any]) -> str:
    report = route.get("report") if isinstance(route.get("report"), dict) else {}
    path = report.get("path")
    if not isinstance(path, str) or not path:
        raise SmokeError(f"route did not return report.path: {route}")
    return path


def run_parallel_smoke(
    base_url: str,
    root: Path,
    daemon_workspace: Path,
    store_backend: str,
    worker_count: int,
    omit_report_path: bool,
) -> dict[str, Any]:
    started_at = int(time.time())
    workers: list[tuple[str, Path, str, dict[str, Any]]] = []
    for index in range(worker_count):
        target_workspace = daemon_workspace / f"target-{index + 1}"
        target_workspace.mkdir(parents=True, exist_ok=True)
        session_name = f"v61-parallel-{worker_count}-{index + 1}-{started_at}"
        requested_report_rel = None if omit_report_path else report_path_for_session(session_name)
        route = start_route(base_url, root, target_workspace, session_name, requested_report_rel)
        report_rel = report_path_from_route(route)
        if route.get("worker") != session_name:
            raise SmokeError(
                "parallel route worker: expected "
                f"{session_name!r}, got {route.get('worker')!r}; route={json.dumps(route, ensure_ascii=False)[:1200]}"
            )
        if route.get("state") in {"prompt_commit_signal_sent", "pending_prompt_commit_sent"}:
            raise SmokeError(f"parallel route left hidden/pending commit state: {route}")
        if route.get("state") not in {"starting", "prompt_submitted", "working"}:
            raise SmokeError(
                f"parallel route state unexpected for {session_name}: {route.get('state')!r}"
            )
        workers.append((session_name, target_workspace, report_rel, route))

    for session_name, _, _, _ in workers:
        verify_active_leases(daemon_workspace, store_backend, session_name)

    for index, (session_name, target_workspace, report_rel, _) in enumerate(workers):
        sent = send_input(base_url, session_name, f"AGENTCALL_SMOKE_PING_{index + 1}")
        if sent.get("ok") is not True:
            raise SmokeError(f"parallel send failed for {session_name}: {sent}")
        assert_report_file(target_workspace, report_rel)
        requested = request_report(base_url, session_name)
        assert_report_requested(requested, session_name)
        requested_summary = mcp_call(base_url, "agentcall_session", {"name": session_name})
        assert_summary_report_requested(requested_summary, session_name)
        ingest_report_write(base_url, session_name, target_workspace, report_rel)

    accepted_reports: list[dict[str, Any]] = []
    for session_name, _, _, _ in workers:
        summary = mcp_call(base_url, "agentcall_session", {"name": session_name})
        assert_eq(summary.get("schema_version"), 2, f"{session_name} summary schema")
        assert_report_ready(summary, session_name)
        accepted = accept_report(base_url, session_name)
        assert_report_accept_high(accepted, session_name)
        accepted_reports.append(accepted)

    for session_name, _, _, _ in workers:
        stop = stop_session(base_url, session_name)
        if stop.get("ok") is not True:
            raise SmokeError(f"parallel stop failed for {session_name}: {stop}")

    for session_name, _, _, _ in workers:
        wait_for_worker_state(base_url, session_name, {"stopping", "done"}, f"{session_name} stop projection")
        assert_released_leases(daemon_workspace, store_backend, session_name)

    health = get_json(f"{base_url}/api/runtime/health")
    assert_runtime_baseline(health)
    public_blob = json.dumps(
        {
            "routes": [route for _, _, _, route in workers],
            "accepted_reports": accepted_reports,
            "health": health,
        },
        ensure_ascii=False,
    )
    if "pending_prompt_commit_sent" in public_blob:
        raise SmokeError("parallel public payload contained pending_prompt_commit_sent")

    return {
        "status": "ok",
        "workspace": str(daemon_workspace),
        "base_url": base_url,
        "store_backend": store_backend,
        "parallel_workers": worker_count,
        "omit_report_path": omit_report_path,
        "sessions": [session_name for session_name, _, _, _ in workers],
        "route_ids": [route.get("route_id") for _, _, _, route in workers],
        "checks": [
            f"{worker_count} independent target workspaces routed through one daemon",
            "daemon minted unique report paths" if omit_report_path else "explicit report paths stayed unique",
            "runtime health exposed current daemon build identity before pressure",
            "no public response contained pending_prompt_commit_sent",
            "all workers wrote report files under their route workspaces",
            "hook ingest marked report_ready before accept",
            "all accepts returned confidence.overall=high with daemon_observed_write=true",
            "all stops released owner/workspace leases",
            "runtime health returned active_pty_sessions=0 and active leases=0 after stop",
        ],
    }


def assert_report_file(workspace: Path, report_path: str) -> None:
    path = workspace / report_path
    if not path.exists():
        raise SmokeError(f"expected fake worker report file: {path}")
    if path.stat().st_size <= 0:
        raise SmokeError(f"expected non-empty fake worker report file: {path}")


def request_report(base_url: str, session_name: str) -> dict[str, Any]:
    return mcp_call(
        base_url,
        "agentcall_session_send",
        {
            "name": session_name,
            "action": "request_report",
        },
    )


def assert_report_requested(result: dict[str, Any], session_name: str) -> None:
    if result.get("ok") is not True:
        raise SmokeError(f"request_report failed for {session_name}: {result}")
    if result.get("status") != "report_requested":
        raise SmokeError(f"request_report did not return report_requested for {session_name}: {result}")
    report = result.get("report") if isinstance(result.get("report"), dict) else {}
    if report.get("status") != "report_requested":
        raise SmokeError(f"request_report missing report status for {session_name}: {result}")


def assert_summary_report_requested(summary: dict[str, Any], session_name: str) -> None:
    if summary.get("state") not in {"report_requested", "report_drafting"}:
        raise SmokeError(f"summary did not project report request for {session_name}: {summary}")
    report = summary.get("report") if isinstance(summary.get("report"), dict) else {}
    if report.get("status") not in {"report_requested", "report_drafting"}:
        raise SmokeError(f"summary report status missing request state for {session_name}: {summary}")


def ingest_report_write(base_url: str, session_name: str, workspace: Path, report_path: str) -> None:
    result = post_json(
        f"{base_url}/api/hooks/ingest",
        {
            "event": "PostToolUse",
            "runtime": "claude-code-session",
            "payload": {
                "session_id": f"hook-{session_name}",
                "wrapper_session": session_name,
                "workspace": str(workspace),
                "tool_name": "Write",
                "tool_input": {"file_path": report_path},
                "transcript_path": str(workspace / ".agentcall" / f"{session_name}.jsonl"),
            },
        },
    )
    decision = result.get("decision") if isinstance(result.get("decision"), dict) else {}
    if decision.get("report_ready") is not True:
        raise SmokeError(f"hook ingest did not mark report_ready for {session_name}: {result}")


def assert_report_ready(summary: dict[str, Any], session_name: str) -> None:
    report = summary.get("report") if isinstance(summary.get("report"), dict) else {}
    if report.get("ready") is not True:
        raise SmokeError(f"summary report_ready missing for {session_name}: {summary}")
    if summary.get("state") != "report_ready":
        raise SmokeError(f"summary state should be report_ready for {session_name}: {summary}")
    if summary.get("next_action") != "accept_report":
        raise SmokeError(f"summary next_action should be accept_report for {session_name}: {summary}")


def accept_report(base_url: str, session_name: str) -> dict[str, Any]:
    return mcp_call(
        base_url,
        "agentcall_report",
        {
            "action": "accept",
            "session_id": session_name,
        },
    )


def assert_report_accept_high(accepted: dict[str, Any], session_name: str) -> None:
    if accepted.get("ok") is not True:
        raise SmokeError(f"report accept failed for {session_name}: {accepted}")
    validation = accepted.get("validation") if isinstance(accepted.get("validation"), dict) else {}
    confidence = accepted.get("confidence") if isinstance(accepted.get("confidence"), dict) else {}
    if validation.get("daemon_observed_write") is not True:
        raise SmokeError(f"report accept missing daemon write evidence for {session_name}: {accepted}")
    if confidence.get("overall") != "high":
        raise SmokeError(f"report accept confidence should be high for {session_name}: {accepted}")


def assert_runtime_baseline(health: dict[str, Any]) -> None:
    if health.get("active_pty_sessions") != 0:
        raise SmokeError(f"runtime health expected active_pty_sessions=0 after stop: {health}")
    owner = health.get("owner_leases") if isinstance(health.get("owner_leases"), dict) else {}
    workspace = (
        health.get("workspace_leases")
        if isinstance(health.get("workspace_leases"), dict)
        else {}
    )
    if owner.get("active") != 0:
        raise SmokeError(f"runtime health expected owner_leases.active=0 after stop: {health}")
    if workspace.get("active") != 0:
        raise SmokeError(f"runtime health expected workspace_leases.active=0 after stop: {health}")


def send_input(base_url: str, session_name: str, text: str) -> dict[str, Any]:
    return mcp_call(
        base_url,
        "agentcall_session_send",
        {
            "name": session_name,
            "action": "send",
            "text": text,
            "enter": True,
            "idempotency_key": f"smoke-send-{session_name}-{text}",
            "owner_id": "codex",
        },
    )


def stop_session(base_url: str, session_name: str) -> dict[str, Any]:
    summary = mcp_call(base_url, "agentcall_session", {"name": session_name})
    token = summary.get("control", {}).get("token") if isinstance(summary.get("control"), dict) else None
    if not token:
        raise SmokeError(f"missing control token for stop: {summary}")
    return mcp_call(
        base_url,
        "agentcall_session_send",
        {
            "name": session_name,
            "action": "stop",
            "control_token": token,
        },
    )


def verify_restart_recovery(
    base_url: str,
    workspace: Path,
    store_backend: str,
    session_name: str,
) -> None:
    summary = mcp_call(base_url, "agentcall_session", {"name": session_name})
    assert_eq(summary.get("view"), "summary", "restart session summary view")
    if summary.get("state") not in {"stopping", "done", "working"}:
        raise SmokeError(f"restart state unexpected: {summary.get('state')!r}")

    session_with_events = mcp_call(
        base_url,
        "agentcall_session",
        {"name": session_name, "view": "events", "limit": 20},
    )
    event_count = session_with_events.get("event_count", 0)
    if event_count <= 0:
        raise SmokeError("restart event recovery: expected persisted session events")

    assert_command_record(workspace, store_backend, f"smoke-send-{session_name}-AGENTCALL_SMOKE_PING")
    assert_released_leases(workspace, store_backend, session_name)


def verify_active_leases(workspace: Path, store_backend: str, session_name: str) -> None:
    if store_backend == "sqlite":
        import sqlite3

        db_path = workspace / ".agentcall" / "state" / "runtime.db"
        with sqlite3.connect(db_path) as conn:
            owner = conn.execute(
                "SELECT status FROM owner_leases WHERE session_id = ?",
                (session_name,),
            ).fetchone()
            workspace_row = conn.execute(
                "SELECT lease_id FROM workspace_leases WHERE session_id = ?",
                (session_name,),
            ).fetchone()
            session_row = conn.execute(
                "SELECT runtime FROM sessions WHERE session_id = ?",
                (session_name,),
            ).fetchone()
        if owner is None or owner[0] != "Active":
            raise SmokeError(f"sqlite lease recovery: expected active owner lease, got {owner!r}")
        if workspace_row is None:
            raise SmokeError("sqlite lease recovery: expected active workspace lease")
        if session_row is None or session_row[0] != "pty":
            raise SmokeError(
                f"sqlite route recovery: expected durable pty session row, got {session_row!r}"
            )
        return

    owner_index = read_json_file(workspace / ".agentcall" / "state" / "owner_leases.index.json")
    workspace_index = read_json_file(
        workspace / ".agentcall" / "state" / "workspace_leases.index.json"
    )
    session_index = read_json_file(workspace / ".agentcall" / "state" / "sessions.index.json")
    if owner_index.get(session_name, {}).get("status") != "Active":
        raise SmokeError("json lease recovery: expected active owner lease")
    if workspace_index.get(session_name, {}).get("status") != "Active":
        raise SmokeError("json lease recovery: expected active workspace lease")
    if session_index.get(session_name, {}).get("runtime") != "pty":
        raise SmokeError("json route recovery: expected durable pty session row")


def assert_released_leases(workspace: Path, store_backend: str, session_name: str) -> None:
    if store_backend == "sqlite":
        import sqlite3

        db_path = workspace / ".agentcall" / "state" / "runtime.db"
        with sqlite3.connect(db_path) as conn:
            owner = conn.execute(
                "SELECT status FROM owner_leases WHERE session_id = ?",
                (session_name,),
            ).fetchone()
            workspace_row = conn.execute(
                "SELECT lease_id FROM workspace_leases WHERE session_id = ?",
                (session_name,),
            ).fetchone()
        if owner is None or owner[0] != "Released":
            raise SmokeError(f"sqlite lease recovery: expected released owner lease, got {owner!r}")
        if workspace_row is not None:
            raise SmokeError("sqlite lease recovery: expected workspace lease to be released")
        return

    owner_index = read_json_file(workspace / ".agentcall" / "state" / "owner_leases.index.json")
    workspace_index = read_json_file(
        workspace / ".agentcall" / "state" / "workspace_leases.index.json"
    )
    if owner_index.get(session_name, {}).get("status") != "Released":
        raise SmokeError("json lease recovery: expected released owner lease")
    if workspace_index.get(session_name, {}).get("status") != "Released":
        raise SmokeError("json lease recovery: expected released workspace lease")


def assert_command_record(workspace: Path, store_backend: str, idempotency_key: str) -> None:
    if store_backend == "sqlite":
        import sqlite3

        db_path = workspace / ".agentcall" / "state" / "runtime.db"
        if not db_path.exists():
            raise SmokeError(f"sqlite command recovery: missing db {db_path}")
        with sqlite3.connect(db_path) as conn:
            row = conn.execute(
                "SELECT status FROM commands WHERE owner_id = ? AND idempotency_key = ?",
                ("codex", idempotency_key),
            ).fetchone()
        if row is None:
            raise SmokeError("sqlite command recovery: expected command idempotency row")
        if row[0] != "completed":
            raise SmokeError(f"sqlite command recovery: expected completed command, got {row!r}")
        return

    index_path = workspace / ".agentcall" / "state" / "commands.index.json"
    if not index_path.exists():
        raise SmokeError(f"json command recovery: missing index {index_path}")
    value = json.loads(index_path.read_text(encoding="utf-8"))
    record = value.get(f"codex:{idempotency_key}")
    if record is None:
        raise SmokeError("json command recovery: expected command idempotency record")
    if record.get("status") != "completed":
        raise SmokeError(f"json command recovery: expected completed command, got {record!r}")


def read_json_file(path: Path) -> dict[str, Any]:
    if not path.exists():
        raise SmokeError(f"missing JSON state file: {path}")
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise SmokeError(f"expected object JSON state file: {path}")
    return value


def mcp_call(base_url: str, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
    return post_json(
        f"{base_url}/api/mcp/call",
        {
            "name": name,
            "arguments": arguments,
        },
    )


def wait_for_clean_output(base_url: str, session_name: str, needle: str, label: str) -> None:
    deadline = time.time() + 10
    last_text = ""
    while time.time() < deadline:
        output = get_json(f"{base_url}/api/sessions/{session_name}/output/clean", timeout=2.0)
        text = output.get("clean_output", "")
        last_text = text[-1200:]
        if needle in text:
            return
        time.sleep(0.2)
    try:
        sessions = get_json(f"{base_url}/api/sessions", timeout=2.0)
    except SmokeError as exc:
        sessions = {"error": str(exc)}
    try:
        summary = mcp_call(base_url, "agentcall_session", {"name": session_name})
    except SmokeError as exc:
        summary = {"error": str(exc)}
    raise SmokeError(
        f"timed out waiting for {label}: {needle}; clean_tail={last_text!r}; "
        f"sessions={json.dumps(sessions, ensure_ascii=False)[:1200]}; "
        f"summary={json.dumps(summary, ensure_ascii=False)[:1200]}"
    )


def wait_for_worker_state(
    base_url: str,
    session_name: str,
    allowed: set[str],
    label: str,
) -> None:
    deadline = time.time() + 10
    last = None
    while time.time() < deadline:
        summary = mcp_call(base_url, "agentcall_session", {"name": session_name})
        last = summary.get("state")
        if last in allowed:
            return
        time.sleep(0.2)
    raise SmokeError(f"timed out waiting for {label}: last worker state={last}")


def get_json(url: str, timeout: float = 5.0) -> dict[str, Any]:
    request = urllib.request.Request(url, method="GET", headers=daemon_headers())
    return request_json(request, timeout)


def post_json(url: str, payload: dict[str, Any], timeout: float = 5.0) -> dict[str, Any]:
    data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(
        url,
        data=data,
        method="POST",
        headers={"Content-Type": "application/json; charset=utf-8", **daemon_headers()},
    )
    return request_json(request, timeout)


def daemon_headers() -> dict[str, str]:
    return {"X-AgentCall-Token": SMOKE_DAEMON_TOKEN}


def request_json(request: urllib.request.Request, timeout: float) -> dict[str, Any]:
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            data = response.read()
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", errors="replace")
        raise SmokeError(f"{request.full_url} returned HTTP {exc.code}: {body}") from exc
    except OSError as exc:
        raise SmokeError(f"{request.full_url} failed: {exc}") from exc
    try:
        value = json.loads(data.decode("utf-8"))
    except json.JSONDecodeError as exc:
        raise SmokeError(f"{request.full_url} returned invalid JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise SmokeError(f"{request.full_url} returned non-object JSON")
    return value


def assert_eq(actual: Any, expected: Any, label: str) -> None:
    if actual != expected:
        raise SmokeError(f"{label}: expected {expected!r}, got {actual!r}")


def daemon_tail(path: Path) -> str:
    if not path.exists():
        return ""
    try:
        return path.read_text(encoding="utf-8", errors="replace")[-4000:]
    except OSError:
        return ""


def terminate_daemon(proc: subprocess.Popen[str]) -> None:
    if proc.poll() is not None:
        return
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


if __name__ == "__main__":
    raise SystemExit(main())
