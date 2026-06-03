from __future__ import annotations

import argparse
import json
import threading
import time
from pathlib import Path

import winpty

from .models import utc_now
from .store import Store


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True)
    parser.add_argument("--cwd", default=None)
    parser.add_argument("--name", required=True)
    parser.add_argument("--command-json", required=True)
    parser.add_argument("--cols", type=int, default=100)
    parser.add_argument("--rows", type=int, default=40)
    args = parser.parse_args()

    root = Path(args.root).resolve()
    cwd = Path(args.cwd).resolve() if args.cwd else root
    store = Store(root)
    session_dir = root / ".agentcall" / "sessions" / args.name
    state_path = session_dir / "state.json"
    input_path = session_dir / "input.ndjson"
    output_path = session_dir / "output.log"
    command = [str(part) for part in json.loads(args.command_json)]

    write_state(state_path, args.name, command, "starting", child_pid=None, cwd=str(cwd))
    child = winpty.PtyProcess.spawn(
        command,
        cwd=str(cwd),
        dimensions=(args.rows, args.cols),
    )
    child_pid = getattr(child, "pid", None)
    write_state(state_path, args.name, command, "running", child_pid=child_pid, cwd=str(cwd))
    store.append_event(
        "pty.worker_started",
        message=f"PTY worker spawned child for session {args.name}.",
        data={"name": args.name, "child_pid": child_pid, "command": command, "cwd": str(cwd)},
    )

    stop = threading.Event()

    def reader() -> None:
        with output_path.open("a", encoding="utf-8", errors="replace") as output:
            output.write(f"\n\n--- session {args.name} started {utc_now()} cwd={str(cwd)!r} command={command!r} ---\n")
            output.flush()
            while not stop.is_set():
                try:
                    chunk = child.read(4096)
                except Exception as exc:
                    output.write(f"\n--- read error: {exc} ---\n")
                    output.flush()
                    store.append_event(
                        "pty.read_error",
                        message=f"PTY read error in session {args.name}.",
                        data={"name": args.name, "error": str(exc)},
                    )
                    break
                if not chunk:
                    time.sleep(0.05)
                    continue
                output.write(chunk)
                output.flush()

    thread = threading.Thread(target=reader, daemon=True)
    thread.start()

    offset = 0
    try:
        while child.isalive() and not stop.is_set():
            if input_path.exists():
                with input_path.open("r", encoding="utf-8") as handle:
                    handle.seek(offset)
                    while True:
                        line = handle.readline()
                        if not line:
                            break
                        offset = handle.tell()
                        if not line.strip():
                            continue
                        event = json.loads(line)
                        if event.get("type") == "stop":
                            store.append_event(
                                "pty.stop_received",
                                message=f"PTY stop received for session {args.name}.",
                                data={"name": args.name},
                            )
                            stop.set()
                            break
                        if event.get("type") == "input":
                            delay_ms = int(event.get("delay_ms") or 0)
                            if delay_ms > 0:
                                time.sleep(delay_ms / 1000)
                            child.write(str(event.get("text", "")))
            time.sleep(0.1)
    finally:
        stop.set()
        try:
            if child.isalive():
                child.terminate(force=True)
        except Exception:
            pass
        write_state(state_path, args.name, command, "stopped", child_pid=getattr(child, "pid", None), cwd=str(cwd))
        store.append_event(
            "pty.worker_stopped",
            message=f"PTY worker stopped session {args.name}.",
            data={"name": args.name, "child_pid": getattr(child, "pid", None), "cwd": str(cwd)},
        )
    return 0


def write_state(path: Path, name: str, command: list[str], status: str, child_pid: int | None, cwd: str | None) -> None:
    current = {}
    if path.exists():
        try:
            current = json.loads(path.read_text(encoding="utf-8"))
        except Exception:
            current = {}
    data = {
        "name": name,
        "command": command,
        "cwd": cwd,
        "worker_pid": current.get("worker_pid"),
        "child_pid": child_pid,
        "status": status,
        "created_at": current.get("created_at", utc_now()),
        "updated_at": utc_now(),
    }
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")


if __name__ == "__main__":
    raise SystemExit(main())
