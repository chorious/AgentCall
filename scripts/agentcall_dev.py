from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Callable


HOOK_EVENTS = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolBatch",
    "Notification",
    "Stop",
    "SubagentStop",
    "PreCompact",
    "SessionEnd",
]


@dataclass
class Check:
    name: str
    status: str
    detail: str
    hint: str | None = None


class ToolError(RuntimeError):
    pass


def main() -> int:
    configure_stdio()
    parser = argparse.ArgumentParser(
        description="AgentCall development and release helper. Start here when state, hooks, tests, or release checks look suspicious."
    )
    parser.add_argument("--root", default=repo_root(), help="AgentCall repository root.")
    sub = parser.add_subparsers(dest="command", required=True)

    doctor = sub.add_parser("doctor", help="Check config, hooks, daemon health, plugin metadata, and local tool paths.")
    doctor.add_argument("--strict", action="store_true", help="Return non-zero on warnings as well as failures.")
    doctor.add_argument("--daemon-url", default=default_daemon_url(), help="Daemon base URL.")
    doctor.set_defaults(func=cmd_doctor)

    health = sub.add_parser("daemon-health", help="Query daemon /api/runtime/health with a short timeout.")
    health.add_argument("--daemon-url", default=default_daemon_url(), help="Daemon base URL.")
    health.add_argument("--timeout", type=float, default=5.0, help="HTTP timeout in seconds.")
    health.set_defaults(func=cmd_daemon_health)

    hooks = sub.add_parser("install-hooks", help="Install Claude/Codex hooks using the repo config conventions.")
    hooks.add_argument("--claude", action="store_true", help="Install Claude Code hooks.")
    hooks.add_argument("--codex", action="store_true", help="Install Codex hooks.")
    hooks.add_argument("--dry-run", action="store_true", help="Print intended hook config without writing when supported.")
    hooks.set_defaults(func=cmd_install_hooks)

    release = sub.add_parser("release-check", help="Run the checks normally needed before committing a release.")
    release.add_argument("--skip-cargo", action="store_true", help="Skip cargo tests.")
    release.add_argument("--skip-pytest", action="store_true", help="Skip pytest.")
    release.add_argument("--skip-plugin", action="store_true", help="Skip Codex plugin validation.")
    release.set_defaults(func=cmd_release_check)

    paths = sub.add_parser("paths", help="Print resolved important local paths.")
    paths.set_defaults(func=cmd_paths)

    args = parser.parse_args()
    root = Path(args.root).resolve()
    try:
        return int(args.func(root, args) or 0)
    except ToolError as exc:
        print(f"[FAIL] {exc}", file=sys.stderr)
        return 2


def cmd_doctor(root: Path, args: argparse.Namespace) -> int:
    checks: list[Check] = []
    checks.append(check_repo(root))
    config = read_local_config(root)
    checks.append(check_config(root, config))
    checks.append(check_cargo())
    checks.append(check_python())
    checks.append(check_node())
    checks.append(check_plugin(root))
    checks.extend(check_claude_hooks(root, config))
    checks.append(check_daemon(args.daemon_url, timeout=5.0))
    checks.append(check_git(root))
    print_checks(checks)
    failed = [item for item in checks if item.status == "FAIL"]
    warned = [item for item in checks if item.status == "WARN"]
    if failed or (args.strict and warned):
        print()
        if failed:
            print("Failures:")
            for item in failed:
                print(f"- {item.name}: {item.detail}")
                if item.hint:
                    print(f"  hint: {item.hint}")
        if args.strict and warned:
            print("Warnings:")
            for item in warned:
                print(f"- {item.name}: {item.detail}")
                if item.hint:
                    print(f"  hint: {item.hint}")
        return 1
    return 0


def cmd_daemon_health(root: Path, args: argparse.Namespace) -> int:
    _ = root
    url = args.daemon_url.rstrip("/") + "/api/runtime/health"
    try:
        payload = http_json(url, timeout=args.timeout)
    except ToolError as exc:
        raise ToolError(f"daemon health failed: {exc}") from exc
    print(json.dumps(payload, ensure_ascii=False, indent=2))
    return 0


def cmd_install_hooks(root: Path, args: argparse.Namespace) -> int:
    install_claude = args.claude or not args.codex
    install_codex = args.codex or not args.claude
    if install_claude:
        cmd = [sys.executable, str(root / "scripts" / "install_claude_hooks.py"), "--root", str(root)]
        if args.dry_run:
            cmd.append("--dry-run")
        run_checked(cmd, root, "install Claude hooks")
    if install_codex:
        cmd = [sys.executable, str(root / "scripts" / "install_codex_hooks.py"), "--root", str(root)]
        if args.dry_run:
            print("[WARN] install_codex_hooks.py does not use --dry-run in older versions; running real installer is skipped.")
        else:
            run_checked(cmd, root, "install Codex hooks")
    return 0


def cmd_release_check(root: Path, args: argparse.Namespace) -> int:
    env = os.environ.copy()
    cargo = find_cargo()
    if cargo:
        env["PATH"] = str(cargo.parent) + os.pathsep + env.get("PATH", "")

    run_checked([sys.executable, "-m", "compileall", "scripts", "src"], root, "python compileall", env=env, timeout=120)
    node = shutil.which("node")
    if node:
        run_checked([node, "--check", "web\\board.js"], root, "node syntax check", env=env, timeout=60)
    else:
        print("[WARN] node not found; skipping web/board.js syntax check")

    if not args.skip_plugin:
        validator = plugin_validator_path()
        if validator.exists():
            run_checked([sys.executable, str(validator), str(root / "plugins" / "agentcall")], root, "plugin validation", env=env, timeout=120)
        else:
            print(f"[WARN] plugin validator not found: {validator}")

    if not args.skip_cargo:
        if not cargo:
            raise ToolError("cargo not found. Install Rust or add C:\\Users\\<you>\\.cargo\\bin to PATH.")
        run_checked([str(cargo), "test", "--workspace", "--target-dir", ".agentcall_build\\target-check"], root, "cargo workspace tests", env=env, timeout=900)

    if not args.skip_pytest:
        run_checked([sys.executable, "-m", "pytest", "-q"], root, "pytest", env=env, timeout=300)

    run_checked(["git", "-C", str(root), "diff", "--check"], root, "git diff whitespace check", env=env, timeout=60)
    print("[OK] release-check completed")
    return 0


def cmd_paths(root: Path, args: argparse.Namespace) -> int:
    _ = args
    config = read_local_config(root)
    claude_workspace = config.get("claude_workspace")
    paths = {
        "repo": str(root),
        "config": str(root / "config" / "agentcall.local.json"),
        "claude_workspace": claude_workspace,
        "claude_settings": str(Path(claude_workspace) / ".claude" / "settings.local.json") if claude_workspace else None,
        "cargo": str(find_cargo()) if find_cargo() else None,
        "python": sys.executable,
        "node": shutil.which("node"),
        "plugin": str(root / "plugins" / "agentcall"),
        "plugin_validator": str(plugin_validator_path()),
    }
    print(json.dumps(paths, ensure_ascii=False, indent=2))
    return 0


def check_repo(root: Path) -> Check:
    required = ["Cargo.toml", "README.md", "scripts", "crates", "plugins/agentcall"]
    missing = [item for item in required if not (root / item).exists()]
    if missing:
        return Check("repo", "FAIL", f"missing {', '.join(missing)}", "Run from AgentCall repo root or pass --root.")
    return Check("repo", "OK", str(root))


def check_config(root: Path, config: dict) -> Check:
    config_path = root / "config" / "agentcall.local.json"
    claude_workspace = config.get("claude_workspace")
    if not config_path.exists():
        return Check(
            "config",
            "FAIL",
            "config/agentcall.local.json is missing",
            "Copy config/agentcall.example.json to config/agentcall.local.json and set claude_workspace.",
        )
    if not isinstance(claude_workspace, str) or not claude_workspace.strip():
        return Check("config", "FAIL", "claude_workspace is missing", "Set claude_workspace; this is Claude PTY cwd and hook settings root.")
    if not Path(claude_workspace).exists():
        return Check("config", "WARN", f"claude_workspace does not exist: {claude_workspace}", "Create it or fix config/agentcall.local.json.")
    return Check("config", "OK", f"claude_workspace={claude_workspace}")


def check_cargo() -> Check:
    cargo = find_cargo()
    if cargo:
        return Check("cargo", "OK", str(cargo))
    return Check("cargo", "FAIL", "cargo not found", "Add C:\\Users\\<you>\\.cargo\\bin to PATH or install Rust.")


def check_python() -> Check:
    return Check("python", "OK", sys.executable)


def check_node() -> Check:
    node = shutil.which("node")
    if node:
        return Check("node", "OK", node)
    return Check("node", "WARN", "node not found", "Only web syntax checks are skipped; daemon still works.")


def check_plugin(root: Path) -> Check:
    manifest = root / "plugins" / "agentcall" / ".codex-plugin" / "plugin.json"
    if not manifest.exists():
        return Check("plugin", "FAIL", "plugin manifest missing", "Expected plugins/agentcall/.codex-plugin/plugin.json.")
    try:
        data = json.loads(manifest.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return Check("plugin", "FAIL", f"invalid plugin JSON: {exc}")
    version = data.get("version", "-")
    mcp_servers = data.get("mcpServers")
    if not mcp_servers:
        return Check("plugin", "FAIL", f"version={version}; mcpServers missing")
    return Check("plugin", "OK", f"version={version}; mcpServers={mcp_servers}")


def check_claude_hooks(root: Path, config: dict) -> list[Check]:
    claude_workspace = config.get("claude_workspace")
    if not isinstance(claude_workspace, str) or not claude_workspace.strip():
        return [Check("claude hooks", "FAIL", "cannot locate hooks without claude_workspace")]
    settings = Path(claude_workspace) / ".claude" / "settings.local.json"
    if not settings.exists():
        return [
            Check(
                "claude hooks",
                "FAIL",
                f"settings missing: {settings}",
                "Run: python agentcall.py install-hooks --claude",
            )
        ]
    try:
        data = json.loads(settings.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        return [Check("claude hooks", "FAIL", f"invalid JSON: {settings}: {exc}")]
    missing = [event for event in HOOK_EVENTS if not event_has_agentcall_hook(data, event)]
    script = root / "scripts" / "agentcall-claude-hook.py"
    checks = []
    if missing:
        checks.append(
            Check(
                "claude hooks",
                "FAIL",
                f"missing AgentCall events: {', '.join(missing)}",
                "Run: python agentcall.py install-hooks --claude",
            )
        )
    else:
        checks.append(Check("claude hooks", "OK", f"settings={settings}"))
    if script.exists():
        checks.append(Check("claude hook script", "OK", str(script)))
    else:
        checks.append(Check("claude hook script", "FAIL", f"missing {script}"))
    return checks


def check_daemon(base_url: str, timeout: float) -> Check:
    try:
        payload = http_json(base_url.rstrip("/") + "/api/runtime/health", timeout=timeout)
    except ToolError as exc:
        return Check(
            "daemon",
            "FAIL",
            str(exc),
            "Start or restart the daemon, then retry: target\\debug\\agentcall-daemon.exe --workspace E:\\Project\\AgentCall",
        )
    status = payload.get("status", "unknown")
    detail = f"status={status}; active_pty_sessions={payload.get('active_pty_sessions', '-')}; claude_workspace={payload.get('claude_workspace', '-')}"
    if status == "ok":
        return Check("daemon", "OK", detail)
    return Check("daemon", "WARN", detail, "Inspect /api/runtime/health warnings.")


def check_git(root: Path) -> Check:
    try:
        proc = subprocess.run(
            ["git", "-C", str(root), "status", "--short", "--branch"],
            text=True,
            encoding="utf-8",
            errors="replace",
            capture_output=True,
            timeout=10,
        )
    except Exception as exc:
        return Check("git", "WARN", f"git status failed: {exc}")
    if proc.returncode != 0:
        return Check("git", "WARN", proc.stderr.strip() or proc.stdout.strip())
    lines = [line for line in proc.stdout.splitlines() if line.strip()]
    dirty = [line for line in lines[1:] if line.strip()]
    if dirty:
        return Check("git", "WARN", f"{len(dirty)} changed paths", "Commit, stash, or review changes before release.")
    return Check("git", "OK", lines[0] if lines else "clean")


def event_has_agentcall_hook(settings: dict, event: str) -> bool:
    entries = settings.get("hooks", {}).get(event)
    if not isinstance(entries, list):
        return False
    for entry in entries:
        hooks = entry.get("hooks") if isinstance(entry, dict) else None
        if not isinstance(hooks, list):
            continue
        for hook in hooks:
            if not isinstance(hook, dict):
                continue
            command = str(hook.get("command", ""))
            args = " ".join(str(arg) for arg in hook.get("args", []) if arg is not None)
            if "agentcall-claude-hook.py" in f"{command} {args}":
                return True
    return False


def run_checked(
    cmd: list[str],
    cwd: Path,
    label: str,
    env: dict[str, str] | None = None,
    timeout: float = 180,
) -> None:
    print(f"[RUN] {label}: {quote_cmd(cmd)}")
    try:
        proc = subprocess.run(
            cmd,
            cwd=str(cwd),
            env=env,
            text=True,
            encoding="utf-8",
            errors="replace",
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=timeout,
        )
    except FileNotFoundError as exc:
        raise ToolError(f"{label} failed: executable not found: {cmd[0]}") from exc
    except subprocess.TimeoutExpired as exc:
        raise ToolError(f"{label} timed out after {exc.timeout}s: {quote_cmd(cmd)}") from exc
    if proc.stdout.strip():
        print(tail(proc.stdout, 60))
    if proc.returncode != 0:
        raise ToolError(f"{label} failed with exit code {proc.returncode}: {quote_cmd(cmd)}")
    print(f"[OK] {label}")


def http_json(url: str, timeout: float) -> dict:
    try:
        with urllib.request.urlopen(url, timeout=timeout) as response:
            raw = response.read().decode("utf-8", errors="replace")
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", errors="replace")
        raise ToolError(f"HTTP {exc.code} from {url}: {body[:500]}") from exc
    except urllib.error.URLError as exc:
        raise ToolError(f"cannot reach {url}: {exc.reason}") from exc
    except TimeoutError as exc:
        raise ToolError(f"timeout after {timeout}s: {url}") from exc
    try:
        return json.loads(raw)
    except json.JSONDecodeError as exc:
        raise ToolError(f"invalid JSON from {url}: {exc}") from exc


def read_local_config(root: Path) -> dict:
    path = root / "config" / "agentcall.local.json"
    if not path.exists():
        return {}
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return {}


def print_checks(checks: list[Check]) -> None:
    widths = max(len(item.name) for item in checks) if checks else 4
    for item in checks:
        print(f"[{item.status:<4}] {item.name:<{widths}}  {item.detail}")
        if item.hint and item.status != "OK":
            print(f"       hint: {item.hint}")


def find_cargo() -> Path | None:
    cargo = shutil.which("cargo")
    if cargo:
        return Path(cargo)
    home = Path.home()
    candidates = [home / ".cargo" / "bin" / "cargo.exe", home / ".cargo" / "bin" / "cargo"]
    for candidate in candidates:
        if candidate.exists():
            return candidate
    return None


def plugin_validator_path() -> Path:
    return Path.home() / ".codex" / "skills" / ".system" / "plugin-creator" / "scripts" / "validate_plugin.py"


def default_daemon_url() -> str:
    return os.environ.get("AGENTCALL_DAEMON_URL", "http://127.0.0.1:3293")


def repo_root() -> str:
    return str(Path(__file__).resolve().parents[1])


def quote_cmd(cmd: list[str]) -> str:
    return " ".join(f'"{part}"' if " " in str(part) else str(part) for part in cmd)


def tail(text: str, lines: int) -> str:
    items = text.rstrip().splitlines()
    if len(items) <= lines:
        return "\n".join(items)
    return "\n".join(["..."] + items[-lines:])


def configure_stdio() -> None:
    for stream_name in ("stdout", "stderr"):
        stream = getattr(sys, stream_name, None)
        reconfigure: Callable[..., object] | None = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="replace")


if __name__ == "__main__":
    raise SystemExit(main())
