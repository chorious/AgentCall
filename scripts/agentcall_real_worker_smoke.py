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
    args = parser.parse_args()
    root = Path(args.root).resolve()
    daemon_bin = Path(args.daemon_bin) if args.daemon_bin else root / "target" / "debug" / executable_name("agentcall-daemon")
    workspace = Path(tempfile.mkdtemp(prefix="agentcall-v5-smoke-"))
    proc: subprocess.Popen[str] | None = None
    daemon_log = workspace / "daemon.log"
    try:
        ensure_daemon_binary(daemon_bin)
        write_local_config(workspace, args.store_backend)
        port = args.port or free_port()
        proc = start_daemon(daemon_bin, workspace, port, daemon_log)
        base_url = f"http://127.0.0.1:{port}"
        wait_for_daemon(base_url, proc)
        session_name = f"v5-smoke-{int(time.time())}"
        route = start_route(base_url, root, workspace, session_name)
        assert_eq(route.get("status"), "started_prompt_dispatched_without_hook_ack", "route status")
        assert_eq(route.get("session_name"), session_name, "route session_name")
        wait_for_clean_output(base_url, session_name, "AGENTCALL_FAKE_INPUT", "initial route prompt")
        send_input(base_url, session_name, "AGENTCALL_SMOKE_PING")
        wait_for_clean_output(base_url, session_name, "AGENTCALL_SMOKE_PONG", "actor send input")
        summary = mcp_call(base_url, "agentcall_session", {"name": session_name})
        assert_eq(summary.get("projection_only"), True, "session projection_only")
        assert_eq(summary.get("projection_stale"), False, "session projection_stale")
        assert_eq(summary.get("liveness_status"), "working", "session liveness after send")
        stop = stop_session(base_url, session_name)
        assert_eq(stop.get("awaiting_observation"), True, "stop awaiting_observation")
        wait_for_summary_status(base_url, session_name, {"stopping", "completed"}, "stop projection")
        board = get_json(f"{base_url}/api/board?view=compact&filter=attention")
        assert_eq(board.get("projection_only"), True, "board projection_only")
        print(json.dumps({
            "status": "ok",
            "workspace": str(workspace),
            "base_url": base_url,
            "store_backend": args.store_backend,
            "session_name": session_name,
            "route_id": route.get("route_id"),
            "checks": [
                "MCP route started a real PTY runtime",
                "route initial prompt reached fake worker",
                "MCP session_send used actor command path",
                "MCP session default returned projection summary without raw terminal scan",
                "stop returned awaiting observation",
                "compact attention board returned projection-only payload",
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
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
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


def write_local_config(workspace: Path, store_backend: str) -> None:
    config_dir = workspace / "config"
    config_dir.mkdir(parents=True, exist_ok=True)
    (config_dir / "agentcall.local.json").write_text(
        json.dumps(
            {
                "claude_workspace": str(workspace),
                "store_backend": store_backend,
                "max_sessions": 3,
                "per_owner_max_sessions": 2,
                "experimental_sdk_runtime": False,
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


def wait_for_daemon(base_url: str, proc: subprocess.Popen[str]) -> None:
    deadline = time.time() + 10
    while time.time() < deadline:
        if proc.poll() is not None:
            raise SmokeError(f"daemon exited early with code {proc.returncode}\n{daemon_tail(proc)}")
        try:
            health = get_json(f"{base_url}/api/runtime/health", timeout=1.0)
            if health.get("status") in {"ok", "running"}:
                return
        except SmokeError:
            time.sleep(0.2)
    raise SmokeError(f"daemon did not become healthy at {base_url}")


def start_route(base_url: str, root: Path, workspace: Path, session_name: str) -> dict[str, Any]:
    payload = {
        "objective": "AgentCall v5 smoke. Echo route prompt, accept actor input, and wait for stop.",
        "workspace": str(workspace),
        "mode": "start",
        "runtime": "pty",
        "session_name": session_name,
        "command": [sys.executable, str(root / "scripts" / "fake_pty_worker.py")],
        "allowed_paths": [".agentcall/reports"],
        "report_path": ".agentcall/reports/v5-real-worker-smoke.md",
        "read_only": False,
    }
    return mcp_call(base_url, "agentcall_route", payload)


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
    return mcp_call(
        base_url,
        "agentcall_session_send",
        {
            "name": session_name,
            "action": "stop",
            "idempotency_key": f"smoke-stop-{session_name}",
            "owner_id": "codex",
            "precondition": {"turn_state": "working"},
        },
    )


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
    while time.time() < deadline:
        output = get_json(f"{base_url}/api/sessions/{session_name}/output/clean", timeout=2.0)
        text = output.get("clean_output", "")
        if needle in text:
            return
        time.sleep(0.2)
    raise SmokeError(f"timed out waiting for {label}: {needle}")


def wait_for_summary_status(
    base_url: str,
    session_name: str,
    allowed: set[str],
    label: str,
) -> None:
    deadline = time.time() + 10
    last = None
    while time.time() < deadline:
        summary = get_json(f"{base_url}/api/sessions/{session_name}/summary", timeout=2.0)
        last = summary.get("liveness_status")
        if last in allowed:
            return
        time.sleep(0.2)
    raise SmokeError(f"timed out waiting for {label}: last liveness_status={last}")


def get_json(url: str, timeout: float = 5.0) -> dict[str, Any]:
    request = urllib.request.Request(url, method="GET")
    return request_json(request, timeout)


def post_json(url: str, payload: dict[str, Any], timeout: float = 5.0) -> dict[str, Any]:
    data = json.dumps(payload, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(
        url,
        data=data,
        method="POST",
        headers={"Content-Type": "application/json; charset=utf-8"},
    )
    return request_json(request, timeout)


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


if __name__ == "__main__":
    raise SystemExit(main())
