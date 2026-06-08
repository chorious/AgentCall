from __future__ import annotations

import json
import os
import shutil
import socket
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

import pytest


REPO_ROOT = Path(__file__).resolve().parents[1]


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def build_daemon() -> Path:
    target_dir = REPO_ROOT / "target-v061-hook"
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(target_dir)
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo" / "bin" / "cargo.exe")
    subprocess.run([cargo, "build", "-p", "agentcall-daemon"], cwd=REPO_ROOT, env=env, check=True)
    suffix = ".exe" if sys.platform.startswith("win") else ""
    binary = target_dir / "debug" / f"agentcall-daemon{suffix}"
    assert binary.exists(), f"missing daemon binary: {binary}"
    return binary


@pytest.fixture(scope="session")
def daemon_binary_path() -> Path:
    return build_daemon()


def wait_for_daemon(base_url: str) -> None:
    deadline = time.time() + 10
    last_error: Exception | None = None
    while time.time() < deadline:
        try:
            with urllib.request.urlopen(f"{base_url}/api/runtime/health", timeout=1) as response:
                if response.status == 200:
                    return
        except Exception as exc:  # pragma: no cover - diagnostic only
            last_error = exc
        time.sleep(0.1)
    raise AssertionError(f"daemon did not become ready: {last_error}")


def read_json_url(url: str) -> object:
    with urllib.request.urlopen(url, timeout=5) as response:
        return json.loads(response.read().decode("utf-8"))


def run_hook_process(root: Path, base_url: str, event: str, payload: dict) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env["AGENTCALL_DAEMON_URL"] = base_url
    env["PYTHONPATH"] = str(REPO_ROOT / "src") + os.pathsep + env.get("PYTHONPATH", "")
    return subprocess.run(
        [
            sys.executable,
            str(REPO_ROOT / "scripts" / "agentcall-claude-hook.py"),
            "--root",
            str(root),
            "--event",
            event,
        ],
        input=json.dumps(payload),
        text=True,
        capture_output=True,
        cwd=REPO_ROOT,
        env=env,
        timeout=20,
    )


def parse_hook_stdout(stdout: str) -> dict:
    if not stdout.strip():
        return {}
    return json.loads(stdout)


def test_hook_script_daemon_first_concurrent_same_file(tmp_path, daemon_binary_path):
    binary = daemon_binary_path
    port = free_port()
    base_url = f"http://127.0.0.1:{port}"
    daemon = subprocess.Popen(
        [str(binary), "--port", str(port), "--workspace", str(tmp_path)],
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        wait_for_daemon(base_url)
        processes = []
        for index in range(8):
            payload = {
                "session_id": f"sess-{index}",
                "tool_name": "Write",
                "tool_input": {"file_path": "src/app.py"},
                "cwd": str(tmp_path),
            }
            env = os.environ.copy()
            env["AGENTCALL_DAEMON_URL"] = base_url
            env["PYTHONPATH"] = str(REPO_ROOT / "src") + os.pathsep + env.get("PYTHONPATH", "")
            processes.append(
                subprocess.Popen(
                    [
                        sys.executable,
                        str(REPO_ROOT / "scripts" / "agentcall-claude-hook.py"),
                        "--root",
                        str(tmp_path),
                        "--event",
                        "PreToolUse",
                    ],
                    stdin=subprocess.PIPE,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    text=True,
                    cwd=REPO_ROOT,
                    env=env,
                )
            )
            processes[-1].stdin.write(json.dumps(payload))
            processes[-1].stdin.close()

        completed = []
        for process in processes:
            stdout = process.stdout.read()
            stderr = process.stderr.read()
            returncode = process.wait(timeout=20)
            completed.append((returncode, stdout, stderr))

        assert all(returncode == 0 for returncode, _, _ in completed)
        assert not any("AgentCall daemon ingest failed" in stderr for _, _, stderr in completed)

        denied = [
            parse_hook_stdout(stdout)
            for _, stdout, _ in completed
            if parse_hook_stdout(stdout)
        ]
        assert len(denied) == 7
        for item in denied:
            hook = item["hookSpecificOutput"]
            assert hook["hookEventName"] == "PreToolUse"
            assert hook["permissionDecision"] == "deny"
            assert "file claim conflict" in hook["permissionDecisionReason"]

        claims = read_json_url(f"{base_url}/api/file-claims")
        active_claims = [claim for claim in claims.values() if claim.get("status") == "active"]
        assert len(active_claims) == 1
        assert active_claims[0]["file"] == "src/app.py"

        events_path = tmp_path / ".agentcall" / "events" / "recent.ndjson"
        ids = []
        for line in events_path.read_text(encoding="utf-8").splitlines():
            event = json.loads(line)
            ids.append(event["id"])
        assert len(ids) == len(set(ids))
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=5)
        except subprocess.TimeoutExpired:
            daemon.kill()


def test_hook_script_daemon_first_read_does_not_claim(tmp_path, daemon_binary_path):
    binary = daemon_binary_path
    port = free_port()
    base_url = f"http://127.0.0.1:{port}"
    daemon = subprocess.Popen(
        [str(binary), "--port", str(port), "--workspace", str(tmp_path)],
        cwd=REPO_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        wait_for_daemon(base_url)
        payload = {
            "session_id": "reader",
            "tool_name": "Read",
            "tool_input": {"file_path": "src/app.py"},
            "cwd": str(tmp_path),
        }
        result = run_hook_process(tmp_path, base_url, "PostToolUse", payload)
        assert result.returncode == 0
        assert "AgentCall daemon ingest failed" not in result.stderr
        assert read_json_url(f"{base_url}/api/file-claims") == {}
    finally:
        daemon.terminate()
        try:
            daemon.wait(timeout=5)
        except subprocess.TimeoutExpired:
            daemon.kill()
