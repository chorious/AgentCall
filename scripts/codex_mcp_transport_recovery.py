from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any


CODEX_HOME = Path(os.environ.get("CODEX_HOME", Path.home() / ".codex"))
DEFAULT_CONFIG = CODEX_HOME / "config.toml"
DEFAULT_DAEMON_URL = "http://127.0.0.1:3293"


@dataclass
class ProcessInfo:
    pid: int
    ppid: int
    name: str
    command_line: str


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Diagnose or force-reset Codex AgentCall MCP transport out-of-band."
    )
    parser.add_argument("--config", default=str(DEFAULT_CONFIG))
    parser.add_argument("--daemon-url", default=DEFAULT_DAEMON_URL)
    parser.add_argument("--kill-runtime", type=int, help="Codex runtime PID to terminate.")
    parser.add_argument("--yes", action="store_true", help="Required with --kill-runtime.")
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit machine-readable diagnostics.",
    )
    args = parser.parse_args(argv)

    if args.kill_runtime is not None:
        if not args.yes:
            print("--kill-runtime requires --yes", file=sys.stderr)
            return 2
        terminate_process(args.kill_runtime)
        print(f"terminated Codex runtime pid={args.kill_runtime}")
        return 0

    state = diagnose(Path(args.config), args.daemon_url)
    if args.json:
        print(json.dumps(state, ensure_ascii=False, indent=2))
    else:
        print_human(state)
    return 0


def diagnose(config: Path, daemon_url: str) -> dict[str, Any]:
    processes = list_processes()
    agentcall_mcp = [
        proc
        for proc in processes
        if "agentcall-mcp" in proc.command_line.lower()
        and proc.pid != os.getpid()
    ]
    codex_runtimes = [
        proc
        for proc in processes
        if proc.name.lower() == "codex.exe" or proc.name.lower() == "codex"
    ]
    ancestor_chain = current_ancestor_chain(processes)
    current_runtime = next(
        (
            proc
            for proc in ancestor_chain
            if proc.name.lower() in {"codex.exe", "codex"}
        ),
        None,
    )
    return {
        "config": str(config),
        "config_exists": config.exists(),
        "agentcall_config": read_agentcall_config(config),
        "daemon": probe_daemon(daemon_url),
        "agentcall_mcp_processes": [proc.__dict__ for proc in agentcall_mcp],
        "codex_runtimes": [proc.__dict__ for proc in codex_runtimes],
        "current_process": os.getpid(),
        "current_ancestor_chain": [proc.__dict__ for proc in ancestor_chain],
        "current_codex_runtime_pid": current_runtime.pid if current_runtime else None,
        "recovery_policy": {
            "in_band_restart_possible": False,
            "why": "When MCP transport is closed, AgentCall MCP tools cannot be called to restart themselves.",
            "safe_default": "Use a new Codex thread/session or restart the Codex app to create a fresh MCP stdio transport.",
            "force_option": "Run this script out-of-band with --kill-runtime <pid> --yes to terminate a stuck Codex runtime.",
        },
    }


def print_human(state: dict[str, Any]) -> None:
    print("AgentCall MCP transport recovery")
    print()
    print(f"config: {state['config']} exists={state['config_exists']}")
    print(f"daemon: {state['daemon']}")
    print(f"agentcall_mcp_processes: {len(state['agentcall_mcp_processes'])}")
    for proc in state["agentcall_mcp_processes"]:
        print(f"  - pid={proc['pid']} ppid={proc['ppid']} {proc['command_line']}")
    print(f"current_codex_runtime_pid: {state['current_codex_runtime_pid']}")
    print()
    print("Recovery:")
    print("- If AgentCall daemon is healthy but MCP tool calls say Transport closed, the broken layer is Codex stdio transport.")
    print("- There is no in-band AgentCall tool that can fix a closed transport.")
    print("- Preferred: create/reopen a Codex thread or restart Codex app.")
    if state["current_codex_runtime_pid"]:
        pid = state["current_codex_runtime_pid"]
        print(f"- Force current runtime reset, out-of-band only:")
        print(f"  python scripts\\codex_mcp_transport_recovery.py --kill-runtime {pid} --yes")


def read_agentcall_config(config: Path) -> dict[str, Any]:
    if not config.exists():
        return {"found": False}
    text = config.read_text(encoding="utf-8", errors="replace")
    marker = "[mcp_servers.agentcall]"
    if marker not in text:
        return {"found": False}
    section = text.split(marker, 1)[1].split("\n[", 1)[0]
    return {"found": True, "section": marker + section.rstrip()}


def probe_daemon(daemon_url: str) -> dict[str, Any]:
    try:
        host, port = parse_daemon_url(daemon_url)
        with socket.create_connection((host, port), timeout=1.0) as sock:
            request = (
                "GET /api/runtime/health HTTP/1.1\r\n"
                f"Host: {host}:{port}\r\n"
                "Connection: close\r\n\r\n"
            )
            sock.sendall(request.encode("ascii"))
            chunks = []
            while True:
                data = sock.recv(65536)
                if not data:
                    break
                chunks.append(data)
        data = b"".join(chunks)
        text = data.decode("utf-8", errors="replace")
        body = text.split("\r\n\r\n", 1)[1] if "\r\n\r\n" in text else "{}"
        return {"status": "reachable", "health": json.loads(body)}
    except Exception as exc:
        return {"status": "unreachable", "error": str(exc)}


def parse_daemon_url(url: str) -> tuple[str, int]:
    rest = url.removeprefix("http://").rstrip("/")
    host, port = rest.rsplit(":", 1)
    return host, int(port)


def list_processes() -> list[ProcessInfo]:
    if os.name == "nt":
        script = (
            "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new(); "
            "Get-CimInstance Win32_Process | "
            "Where-Object { $_.Name -match 'codex|agentcall|powershell|python|node' } | "
            "Select-Object ProcessId,ParentProcessId,Name,CommandLine | "
            "ConvertTo-Json -Depth 3"
        )
        try:
            output = subprocess.check_output(
                ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", script],
                text=True,
                encoding="utf-8",
                errors="replace",
                stderr=subprocess.DEVNULL,
            )
            raw = json.loads(output)
            if isinstance(raw, dict):
                raw = [raw]
            return [
                ProcessInfo(
                    pid=int(item.get("ProcessId") or 0),
                    ppid=int(item.get("ParentProcessId") or 0),
                    name=str(item.get("Name") or ""),
                    command_line=str(item.get("CommandLine") or ""),
                )
                for item in raw
            ]
        except Exception:
            fallback = (
                "Get-Process | "
                "Select-Object Id,ProcessName,Path | "
                "ConvertTo-Json -Depth 3"
            )
            output = subprocess.check_output(
                ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", fallback],
                text=True,
                encoding="utf-8",
                errors="replace",
                stderr=subprocess.DEVNULL,
            )
            raw = json.loads(output)
            if isinstance(raw, dict):
                raw = [raw]
            return [
                ProcessInfo(
                    pid=int(item.get("Id") or 0),
                    ppid=0,
                    name=str(item.get("ProcessName") or ""),
                    command_line=str(item.get("Path") or item.get("ProcessName") or ""),
                )
                for item in raw
            ]
    output = subprocess.check_output(
        ["ps", "-eo", "pid=,ppid=,comm=,args="],
        text=True,
        encoding="utf-8",
        errors="replace",
    )
    processes = []
    for line in output.splitlines():
        parts = line.strip().split(None, 3)
        if len(parts) >= 3:
            processes.append(
                ProcessInfo(
                    pid=int(parts[0]),
                    ppid=int(parts[1]),
                    name=parts[2],
                    command_line=parts[3] if len(parts) > 3 else parts[2],
                )
            )
    return processes


def current_ancestor_chain(processes: list[ProcessInfo]) -> list[ProcessInfo]:
    by_pid = {proc.pid: proc for proc in processes}
    chain: list[ProcessInfo] = []
    pid = os.getpid()
    seen = set()
    while pid and pid not in seen:
        seen.add(pid)
        proc = by_pid.get(pid)
        if proc is None:
            break
        chain.append(proc)
        pid = proc.ppid
    return chain


def terminate_process(pid: int) -> None:
    if os.name == "nt":
        subprocess.check_call(
            ["powershell.exe", "-NoProfile", "-NonInteractive", "-Command", f"Stop-Process -Id {pid} -Force"]
        )
    else:
        os.kill(pid, 9)
    time.sleep(0.2)


if __name__ == "__main__":
    raise SystemExit(main())
