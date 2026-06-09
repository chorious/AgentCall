from __future__ import annotations

import subprocess
import sys
from pathlib import Path


def main() -> int:
    root = Path(__file__).resolve().parents[1]
    failures: list[str] = []

    failures.extend(check_no_tracked_build_outputs(root))
    failures.extend(check_actor_writer_boundary(root))
    failures.extend(check_mcp_default_session_fast_path(root))
    failures.extend(check_board_attention_fast_path(root))
    failures.extend(check_runtime_store_transaction_boundary(root))

    if failures:
        print("[FAIL] AgentCall architecture audit failed:")
        for failure in failures:
            print(f"- {failure}")
        return 1
    print("[OK] AgentCall architecture audit passed")
    return 0


def check_no_tracked_build_outputs(root: Path) -> list[str]:
    failures: list[str] = []
    tracked = git_ls_files(root)
    forbidden_prefixes = (
        "target/",
        "target-v061-hook/",
        ".agentcall/",
        ".agentcall_build/",
        ".agentcall_pytest/",
    )
    for path in tracked:
        normalized = path.replace("\\", "/")
        if normalized.startswith(forbidden_prefixes):
            failures.append(f"tracked generated/runtime file should be removed from git: {path}")
    return failures


def check_actor_writer_boundary(root: Path) -> list[str]:
    failures: list[str] = []
    src = root / "crates" / "agentcall-daemon" / "src"
    allowed_raw_write = {
        src / "actor.rs",
        src / "session.rs",
    }
    allowed_take_writer = {
        src / "session.rs",
    }
    for path in src.glob("*.rs"):
        text = path.read_text(encoding="utf-8")
        if "submit_raw_write" in text and path not in allowed_raw_write:
            failures.append(
                f"{rel(root, path)} references submit_raw_write; PTY input must go through SessionActor command submission"
            )
        if "take_writer(" in text and path not in allowed_take_writer:
            failures.append(
                f"{rel(root, path)} calls take_writer; PtyWriter ownership must be established only in session startup"
            )
    session_text = (src / "session.rs").read_text(encoding="utf-8")
    session_struct = extract_rust_item_body(session_text, "pub(crate) struct Session")
    if "writer" in session_struct.lower():
        failures.append("Session struct appears to expose a PTY writer field")
    return failures


def check_mcp_default_session_fast_path(root: Path) -> list[str]:
    path = root / "crates" / "agentcall-daemon" / "src" / "mcp.rs"
    text = path.read_text(encoding="utf-8")
    body = extract_rust_item_body(text, "fn mcp_session")
    failures: list[str] = []
    if "session_projection_summary(state, name)" not in body:
        failures.append("mcp_session must start from session_projection_summary for default fast path")
    if "session_summary(" in body:
        failures.append("mcp_session default path must not call full session_summary")
    if "Ok(summary)" not in body:
        failures.append("mcp_session must return projection summary when no explicit include is requested")
    return failures


def check_board_attention_fast_path(root: Path) -> list[str]:
    path = root / "crates" / "agentcall-daemon" / "src" / "summary.rs"
    text = path.read_text(encoding="utf-8")
    body = extract_rust_item_body(text, "pub(crate) fn board_state")
    failures: list[str] = []
    marker = 'if view == Some("compact") && filter == Some("attention")'
    cold_marker = "let agent_dir = state.workspace.join"
    marker_index = body.find(marker)
    cold_index = body.find(cold_marker)
    if marker_index < 0:
        failures.append("board_state must special-case compact+attention")
    elif cold_index >= 0 and marker_index > cold_index:
        failures.append("board_state compact+attention fast path must run before cold state reads")
    if "return board_attention_projection(state," not in body:
        failures.append("board_state compact+attention must return board_attention_projection")
    return failures


def check_runtime_store_transaction_boundary(root: Path) -> list[str]:
    failures: list[str] = []
    src = root / "crates" / "agentcall-daemon" / "src"
    allowed_transaction_callers = {
        src / "state.rs",
        src / "store.rs",
        src / "store_json.rs",
        src / "store_sqlite.rs",
    }
    transaction_calls = (
        ".append_event_and_update_projection(",
        ".complete_command_with_event(",
    )
    for path in src.glob("*.rs"):
        text = path.read_text(encoding="utf-8")
        if path in allowed_transaction_callers:
            continue
        for call in transaction_calls:
            if call in text:
                failures.append(
                    f"{rel(root, path)} calls RuntimeStore {call}; live event/projection writes must go through state.rs transaction helpers"
                )
    return failures


def git_ls_files(root: Path) -> list[str]:
    try:
        completed = subprocess.run(
            ["git", "-C", str(root), "ls-files"],
            text=True,
            encoding="utf-8",
            errors="replace",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=True,
        )
    except (FileNotFoundError, subprocess.CalledProcessError) as exc:
        raise SystemExit(f"git ls-files failed: {exc}") from exc
    return [line.strip() for line in completed.stdout.splitlines() if line.strip()]


def extract_rust_item_body(text: str, marker: str) -> str:
    start = text.find(marker)
    if start < 0:
        return ""
    open_brace = text.find("{", start)
    if open_brace < 0:
        return ""
    depth = 0
    for index in range(open_brace, len(text)):
        char = text[index]
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return text[open_brace + 1 : index]
    return text[open_brace + 1 :]


def rel(root: Path, path: Path) -> str:
    return path.relative_to(root).as_posix()


if __name__ == "__main__":
    raise SystemExit(main())
