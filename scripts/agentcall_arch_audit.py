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
    failures.extend(check_mcp_bridge_schema_alignment(root))
    failures.extend(check_mcp_runtime_manifest_guard(root))
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
        "target-",
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
    if '"summary" => Ok(session_summary_view(state, name, &include))' not in body:
        failures.append("mcp_session must route view=summary to session_summary_view")
    if "session_summary(" in body:
        failures.append("mcp_session default path must not call full session_summary")
    if 'unwrap_or_else(|| legacy_session_view(&include))' not in body:
        failures.append("mcp_session must translate legacy include without changing the summary default")
    summary_body = extract_rust_item_body(text, "fn session_summary_view")
    summary_impl_body = summary_body
    if "session_summary_view_for_owner" in summary_body:
        summary_impl_body = extract_rust_item_body(text, "fn session_summary_view_for_owner")
    if "session_projection_summary(state, name)" not in summary_impl_body:
        failures.append("session_summary_view must read projection summary for the default fast path")
    if '"view": "summary"' not in summary_impl_body:
        failures.append("session_summary_view must return the v5.4 summary view contract")
    if '"clean_tail"' in summary_impl_body:
        failures.append("session_summary_view must not include clean_tail")
    return failures


def check_mcp_bridge_schema_alignment(root: Path) -> list[str]:
    failures: list[str] = []
    daemon_text = (root / "crates" / "agentcall-daemon" / "src" / "mcp.rs").read_text(
        encoding="utf-8"
    )
    bridge_text = (root / "crates" / "agentcall-mcp" / "src" / "tools.rs").read_text(
        encoding="utf-8"
    )
    daemon_body = extract_rust_item_body(daemon_text, "pub(crate) fn mcp_tools")
    bridge_functions = {
        "agentcall_board": "board_tool",
        "agentcall_route": "route_tool",
        "agentcall_session": "session_tool",
        "agentcall_session_send": "session_send_tool",
        "agentcall_report": "report_tool",
    }
    for tool_name, bridge_fn in bridge_functions.items():
        daemon_fragment = extract_json_macro_object_containing(
            daemon_body, f'"name": "{tool_name}"'
        )
        bridge_fragment = extract_rust_item_body(bridge_text, f"fn {bridge_fn}")
        if not daemon_fragment:
            failures.append(f"daemon MCP schema missing {tool_name}")
            continue
        if not bridge_fragment:
            failures.append(f"MCP bridge fallback schema missing {tool_name}")
            continue
        daemon_props = schema_property_names(daemon_fragment)
        bridge_props = schema_property_names(bridge_fragment)
        if daemon_props != bridge_props:
            failures.append(
                f"{tool_name} daemon/bridge properties differ: daemon={sorted(daemon_props)} bridge={sorted(bridge_props)}"
            )
        daemon_required = schema_required_fields(daemon_fragment)
        bridge_required = schema_required_fields(bridge_fragment)
        if daemon_required != bridge_required:
            failures.append(
                f"{tool_name} daemon/bridge required fields differ: daemon={daemon_required} bridge={bridge_required}"
            )
        for prop in sorted(daemon_props | bridge_props):
            daemon_enum = schema_property_enum(daemon_fragment, prop)
            bridge_enum = schema_property_enum(bridge_fragment, prop)
            if daemon_enum != bridge_enum:
                failures.append(
                    f"{tool_name}.{prop} daemon/bridge enum differs: daemon={daemon_enum} bridge={bridge_enum}"
                )
    return failures


def check_board_attention_fast_path(root: Path) -> list[str]:
    path = root / "crates" / "agentcall-daemon" / "src" / "summary.rs"
    text = path.read_text(encoding="utf-8")
    body = extract_rust_item_body(text, "pub(crate) fn board_state")
    failures: list[str] = []
    marker = 'if view == Some("compact")'
    cold_marker = "let agent_dir = state.workspace.join"
    marker_index = body.find(marker)
    cold_index = body.find(cold_marker)
    if marker_index < 0:
        failures.append("board_state must special-case compact board")
    elif cold_index >= 0 and marker_index > cold_index:
        failures.append("board_state compact projection path must run before cold state reads")
    compact_branch = body[marker_index:cold_index] if marker_index >= 0 and cold_index >= 0 else body
    if "return board_attention_projection(state, owner_id, filter, workspace_filter);" not in body:
        failures.append("board_state compact board must return board_attention_projection")
    for forbidden in (
        "cleanup_stale_runtime_state",
        "list_sessions(",
        "worker_snapshot_for_session",
        "state_writer.lock",
    ):
        if forbidden in compact_branch:
            failures.append(f"board_state compact projection path must not call {forbidden}")
    for removed_item in (
        "fn v6_compact_board_state",
        "fn cleanup_stale_runtime_state",
    ):
        if removed_item in text:
            failures.append(f"summary.rs must not retain old read/write mixed helper {removed_item}")
    return failures


def check_mcp_runtime_manifest_guard(root: Path) -> list[str]:
    failures: list[str] = []
    bootstrap = (root / "crates" / "agentcall-mcp" / "src" / "bootstrap.rs").read_text(
        encoding="utf-8"
    )
    tools = (root / "crates" / "agentcall-mcp" / "src" / "tools.rs").read_text(
        encoding="utf-8"
    )
    release = (root / "scripts" / "agentcall_runtime_release.py").read_text(encoding="utf-8")
    required_bootstrap_markers = (
        'const RUNTIME_VERSION_FILE: &str = "agentcall-version.json";',
        "fn ensure_daemon_matches_spec(",
        '"daemon_version_drift"',
        '"mcp_runtime_manifest_drift"',
        "normalize_path_for_compare(spec.daemon_binary.as_path())",
    )
    for marker in required_bootstrap_markers:
        if marker not in bootstrap:
            failures.append(f"MCP bootstrap runtime manifest guard missing marker: {marker}")
    if "ensure_daemon_runtime(config)?" not in tools:
        failures.append("MCP proxy tools must validate daemon runtime identity before forwarding")
    if "write_runtime_version_manifest(root, runtime_dir, version)" not in release:
        failures.append("runtime-release must write the MCP hot-read runtime version manifest")
    if '"agentcall-version.json"' not in release:
        failures.append("runtime-release manifest filename must be agentcall-version.json")
    if "$matchedProcs = @(" not in release:
        failures.append("runtime-release Windows cleanup must materialize matched processes as an array")
    if "$matches =" in release:
        failures.append("runtime-release Windows cleanup must not assign to PowerShell $Matches")
    if "$root = {json.dumps(root_text)}" in release:
        failures.append("runtime-release must not pass JSON-escaped Windows paths to PowerShell")
    if "powershell_single_quote(root_text)" not in release:
        failures.append("runtime-release must quote repo roots with PowerShell string rules")
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


def extract_json_macro_object_containing(text: str, marker: str) -> str:
    marker_index = text.find(marker)
    if marker_index < 0:
        return ""
    macro_index = text.rfind("json!({", 0, marker_index)
    if macro_index < 0:
        return ""
    open_brace = text.find("{", macro_index)
    return extract_braced_text(text, open_brace)


def extract_braced_text(text: str, open_brace: int) -> str:
    if open_brace < 0 or open_brace >= len(text) or text[open_brace] != "{":
        return ""
    depth = 0
    in_string = False
    escape = False
    for index in range(open_brace, len(text)):
        char = text[index]
        if in_string:
            if escape:
                escape = False
            elif char == "\\":
                escape = True
            elif char == '"':
                in_string = False
            continue
        if char == '"':
            in_string = True
        elif char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0:
                return text[open_brace : index + 1]
    return text[open_brace:]


def schema_property_names(schema_fragment: str) -> set[str]:
    properties_object = named_json_object(schema_fragment, "properties")
    return top_level_json_keys(properties_object)


def schema_required_fields(schema_fragment: str) -> list[str]:
    required_array = named_json_array(schema_fragment, "required")
    return parse_string_array(required_array)


def schema_property_enum(schema_fragment: str, property_name: str) -> list[str]:
    properties_object = named_json_object(schema_fragment, "properties")
    if not properties_object:
        return []
    property_object = named_json_object(properties_object, property_name)
    enum_array = named_json_array(property_object, "enum")
    return parse_string_array(enum_array)


def named_json_object(text: str, name: str) -> str:
    key_index = text.find(f'"{name}"')
    while key_index >= 0:
        colon = text.find(":", key_index)
        open_brace = text.find("{", colon)
        next_comma = text.find(",", colon)
        if colon >= 0 and open_brace >= 0 and (next_comma < 0 or open_brace < next_comma):
            return extract_braced_text(text, open_brace)
        key_index = text.find(f'"{name}"', key_index + len(name) + 2)
    return ""


def named_json_array(text: str, name: str) -> str:
    key_index = text.find(f'"{name}"')
    while key_index >= 0:
        colon = text.find(":", key_index)
        open_bracket = text.find("[", colon)
        next_comma = text.find(",", colon)
        if colon >= 0 and open_bracket >= 0 and (next_comma < 0 or open_bracket < next_comma):
            return extract_bracketed_text(text, open_bracket)
        key_index = text.find(f'"{name}"', key_index + len(name) + 2)
    return ""


def extract_bracketed_text(text: str, open_bracket: int) -> str:
    if open_bracket < 0 or open_bracket >= len(text) or text[open_bracket] != "[":
        return ""
    depth = 0
    in_string = False
    escape = False
    for index in range(open_bracket, len(text)):
        char = text[index]
        if in_string:
            if escape:
                escape = False
            elif char == "\\":
                escape = True
            elif char == '"':
                in_string = False
            continue
        if char == '"':
            in_string = True
        elif char == "[":
            depth += 1
        elif char == "]":
            depth -= 1
            if depth == 0:
                return text[open_bracket : index + 1]
    return text[open_bracket:]


def top_level_json_keys(object_text: str) -> set[str]:
    keys: set[str] = set()
    if not object_text.startswith("{"):
        return keys
    depth = 0
    index = 0
    in_string = False
    escape = False
    while index < len(object_text):
        char = object_text[index]
        if in_string:
            if escape:
                escape = False
            elif char == "\\":
                escape = True
            elif char == '"':
                in_string = False
            index += 1
            continue
        if char == '"':
            literal, end = parse_string_literal(object_text, index)
            next_index = skip_ws(object_text, end)
            if depth == 1 and next_index < len(object_text) and object_text[next_index] == ":":
                keys.add(literal)
            index = end
            continue
        if char == "{":
            depth += 1
        elif char == "}":
            depth -= 1
        index += 1
    return keys


def parse_string_array(array_text: str) -> list[str]:
    if not array_text:
        return []
    values: list[str] = []
    index = 0
    while index < len(array_text):
        if array_text[index] == '"':
            literal, end = parse_string_literal(array_text, index)
            values.append(literal)
            index = end
        else:
            index += 1
    return values


def parse_string_literal(text: str, quote_index: int) -> tuple[str, int]:
    chars: list[str] = []
    index = quote_index + 1
    escape = False
    while index < len(text):
        char = text[index]
        if escape:
            chars.append(char)
            escape = False
        elif char == "\\":
            escape = True
        elif char == '"':
            return "".join(chars), index + 1
        else:
            chars.append(char)
        index += 1
    return "".join(chars), index


def skip_ws(text: str, index: int) -> int:
    while index < len(text) and text[index].isspace():
        index += 1
    return index


def rel(root: Path, path: Path) -> str:
    return path.relative_to(root).as_posix()


if __name__ == "__main__":
    raise SystemExit(main())
