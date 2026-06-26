from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
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

    verify_runtime = sub.add_parser(
        "verify-runtime-build",
        help="Fail if the running daemon cannot prove it is the current built binary.",
    )
    verify_runtime.add_argument("--daemon-url", default=default_daemon_url(), help="Daemon base URL.")
    verify_runtime.add_argument("--daemon-bin", default=None, help="Expected daemon binary path.")
    verify_runtime.add_argument("--timeout", type=float, default=5.0, help="HTTP timeout in seconds.")
    verify_runtime.set_defaults(func=cmd_verify_runtime_build)

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

    runtime_release = sub.add_parser(
        "runtime-release",
        help="Align product version, build, stop stale AgentCall processes, start daemon, and verify live version.",
    )
    runtime_release.add_argument("--version", required=True, help="Product version, for example 6.3.0.")
    runtime_release.add_argument("--release-label", default=None, help="README/docs version label.")
    runtime_release.add_argument("--daemon-url", default=default_daemon_url(), help="Daemon URL.")
    runtime_release.add_argument("--skip-tests", action="store_true", help="Skip cargo test and pytest.")
    runtime_release.add_argument("--skip-release-check", action="store_true", help="Skip release-check.")
    runtime_release.add_argument("--update-skills", action="store_true", help="Regenerate repository-managed AgentCall skills before release.")
    runtime_release.add_argument("--skip-skill-update", action="store_true", help="Keep generated AgentCall skills unchanged for this release.")
    runtime_release.add_argument("--no-stop-existing", action="store_true", help="Do not stop old daemon/MCP processes.")
    runtime_release.add_argument("--no-restart", action="store_true", help="Do not start daemon after build.")
    runtime_release.add_argument("--dry-run", action="store_true", help="Print intended actions without writing.")
    runtime_release.set_defaults(func=cmd_runtime_release)

    smoke = sub.add_parser("smoke", help="Run bounded integration smoke checks.")
    smoke_sub = smoke.add_subparsers(dest="smoke_command", required=True)
    real_worker = smoke_sub.add_parser(
        "real-worker",
        help="Start a temporary daemon and deterministic PTY worker to validate route/session/projection control.",
    )
    real_worker.add_argument("--daemon-bin", default=None, help="Path to agentcall-daemon executable.")
    real_worker.add_argument("--keep-workspace", action="store_true", help="Keep temporary smoke workspace.")
    real_worker.add_argument(
        "--store-backend",
        choices=["json", "sqlite"],
        default="json",
        help="Temporary daemon RuntimeStore backend.",
    )
    real_worker.add_argument(
        "--parallel-workers",
        type=int,
        default=1,
        help="Run N concurrent fake PTY workers with independent target workspaces.",
    )
    real_worker.add_argument(
        "--omit-report-path",
        action="store_true",
        help="Do not pass report_path to route; require daemon to mint a unique report path.",
    )
    real_worker.set_defaults(func=cmd_smoke_real_worker)

    paths = sub.add_parser("paths", help="Print resolved important local paths.")
    paths.set_defaults(func=cmd_paths)

    logs = sub.add_parser("logs", help="Inspect AgentCall log layout and size budgets.")
    logs_sub = logs.add_subparsers(dest="logs_command", required=True)
    logs_doctor = logs_sub.add_parser("doctor", help="Report recent/archive/artifact log sizes.")
    logs_doctor.set_defaults(func=cmd_logs_doctor)

    sessions = sub.add_parser("sessions", help="Inspect or clean stale session projections.")
    sessions_sub = sessions.add_subparsers(dest="sessions_command", required=True)
    sessions_cleanup = sessions_sub.add_parser("cleanup", help="Remove stale active session projections and orphan queued instructions.")
    sessions_cleanup.add_argument("--stale-after", default="5m", help="TTL such as 5m, 300s, or 1h.")
    sessions_cleanup.add_argument("--apply", action="store_true", help="Write cleanup changes. Without this, only prints a dry run.")
    sessions_cleanup.set_defaults(func=cmd_sessions_cleanup)

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
    checks.append(check_scratch_workspaces(root))
    checks.append(check_daemon(root, args.daemon_url, timeout=5.0))
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
    url = args.daemon_url.rstrip("/") + "/api/runtime/health"
    try:
        payload = http_json(url, timeout=args.timeout, headers=daemon_auth_headers(root))
    except ToolError as exc:
        raise ToolError(f"daemon health failed: {exc}") from exc
    print(json.dumps(payload, ensure_ascii=False, indent=2))
    return 0


def cmd_verify_runtime_build(root: Path, args: argparse.Namespace) -> int:
    url = args.daemon_url.rstrip("/") + "/api/runtime/health"
    try:
        payload = http_json(url, timeout=args.timeout, headers=daemon_auth_headers(root))
    except ToolError as exc:
        raise ToolError(f"daemon health failed: {exc}") from exc
    build = payload.get("build")
    if not isinstance(build, dict):
        raise ToolError("daemon health did not expose build identity")
    binary_path = Path(str(build.get("binary_path") or "")).resolve()
    expected_bin = expected_daemon_binary(root, args.daemon_bin, build)
    if not expected_bin.exists():
        raise ToolError(f"expected daemon binary missing: {expected_bin}")
    if not paths_same(binary_path, expected_bin):
        raise ToolError(f"daemon binary mismatch: running={binary_path}; expected={expected_bin}")
    process_started_at_ms = build.get("process_started_at_ms")
    if not isinstance(process_started_at_ms, int):
        raise ToolError("daemon build identity missing process_started_at_ms")
    binary_modified_ms = int(expected_bin.stat().st_mtime * 1000)
    if process_started_at_ms + 1000 < binary_modified_ms:
        raise ToolError(
            "daemon process started before the expected binary was modified; rebuild/restart required"
        )
    result = {
        "status": "ok",
        "daemon_url": args.daemon_url,
        "binary_path": str(binary_path),
        "process_started_at": build.get("process_started_at"),
        "binary_modified_at": build.get("binary_modified_at"),
        "version": build.get("version"),
        "git_sha": build.get("git_sha"),
        "build_profile": build.get("build_profile"),
    }
    print(json.dumps(result, ensure_ascii=False, indent=2))
    return 0


def expected_daemon_binary(root: Path, daemon_bin: str | None, build: dict) -> Path:
    if daemon_bin:
        return Path(daemon_bin).resolve()
    version = str(build.get("version") or "").strip()
    if version:
        runtime = root / "target" / "runtime" / version / executable_name("agentcall-daemon")
        if runtime.exists():
            return runtime.resolve()
    return default_daemon_binary(root)


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
    run_checked([sys.executable, "scripts/generate_agentcall_skill.py", "--check"], root, "agentcall skills check", env=env, timeout=60)
    run_checked([sys.executable, "scripts/agentcall_arch_audit.py"], root, "agentcall architecture audit", env=env, timeout=60)
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


def cmd_runtime_release(root: Path, args: argparse.Namespace) -> int:
    cmd = [
        sys.executable,
        str(root / "scripts" / "agentcall_runtime_release.py"),
        "--root",
        str(root),
        "--version",
        args.version,
        "--daemon-url",
        args.daemon_url,
    ]
    if args.release_label:
        cmd.extend(["--release-label", args.release_label])
    for flag in [
        "skip_tests",
        "skip_release_check",
        "update_skills",
        "skip_skill_update",
        "no_stop_existing",
        "no_restart",
        "dry_run",
    ]:
        if getattr(args, flag):
            cmd.append("--" + flag.replace("_", "-"))
    run_checked(cmd, root, "runtime release", timeout=1200)
    return 0


def cmd_smoke_real_worker(root: Path, args: argparse.Namespace) -> int:
    cmd = [sys.executable, str(root / "scripts" / "agentcall_real_worker_smoke.py"), "--root", str(root)]
    if args.daemon_bin:
        cmd.extend(["--daemon-bin", args.daemon_bin])
    if args.keep_workspace:
        cmd.append("--keep-workspace")
    cmd.extend(["--store-backend", args.store_backend])
    if args.parallel_workers != 1:
        cmd.extend(["--parallel-workers", str(args.parallel_workers)])
    if args.omit_report_path:
        cmd.append("--omit-report-path")
    run_checked(cmd, root, "real worker PTY smoke", timeout=max(90, 20 * args.parallel_workers))
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


def cmd_logs_doctor(root: Path, args: argparse.Namespace) -> int:
    _ = args
    agent_dir = root / ".agentcall"
    checks = [
        size_check("legacy events", agent_dir / "events.ndjson", warn_bytes=8 * 1024 * 1024),
        size_check("recent events", agent_dir / "events" / "recent.ndjson", warn_bytes=2 * 1024 * 1024),
        size_check("routes state", agent_dir / "state" / "routes.json", warn_bytes=4 * 1024 * 1024),
        dir_size_check("hook logs", agent_dir / "logs" / "hooks", warn_bytes=32 * 1024 * 1024),
        dir_size_check("artifacts", agent_dir / "artifacts", warn_bytes=128 * 1024 * 1024),
    ]
    print_checks(checks)
    recent = agent_dir / "events" / "recent.ndjson"
    if recent.exists():
        print(f"[INFO] board should read recent-first events: {recent}")
    else:
        print("[WARN] recent events file is missing; daemon has not written v4.3 events yet")
    return 0 if all(check.status != "FAIL" for check in checks) else 1


def cmd_sessions_cleanup(root: Path, args: argparse.Namespace) -> int:
    ttl_seconds = parse_duration_seconds(args.stale_after)
    state_dir = root / ".agentcall" / "state"
    active_path = state_dir / "active_sessions.json"
    pending_path = state_dir / "pending_supervisor_instructions.json"
    active = read_json_object(active_path)
    pending = read_json_object(pending_path)
    now = time.time()

    stale_active = []
    for key, value in active.items():
        updated_at = value.get("updated_at") if isinstance(value, dict) else None
        age = timestamp_age_seconds(updated_at, now) if isinstance(updated_at, str) else None
        binding_source = value.get("binding_source", "unbound") if isinstance(value, dict) else "unbound"
        runtime = value.get("runtime", "") if isinstance(value, dict) else ""
        wrapper = value.get("wrapper_session") if isinstance(value, dict) else None
        if age is not None and age >= ttl_seconds and (binding_source == "unbound" or runtime == "codex" or not wrapper):
            stale_active.append(key)

    stale_pending = []
    for wrapper, items in pending.items():
        if not isinstance(items, list):
            stale_pending.append(wrapper)
            continue
        if all(timestamp_age_seconds(item.get("created_at"), now) >= ttl_seconds for item in items if isinstance(item, dict)):
            stale_pending.append(wrapper)

    print(f"[INFO] stale-after={ttl_seconds}s")
    print(f"[INFO] active_sessions stale candidates={len(stale_active)}")
    for key in stale_active[:30]:
        print(f"  active: {key}")
    if len(stale_active) > 30:
        print(f"  ... {len(stale_active) - 30} more")
    print(f"[INFO] pending instruction stale candidates={len(stale_pending)}")
    for wrapper in stale_pending[:30]:
        print(f"  pending: {wrapper}")
    if len(stale_pending) > 30:
        print(f"  ... {len(stale_pending) - 30} more")

    if not args.apply:
        print("[DRY-RUN] no files changed; pass --apply to write cleanup")
        return 0

    for key in stale_active:
        active.pop(key, None)
    for wrapper in stale_pending:
        pending.pop(wrapper, None)
    write_json_object(active_path, active)
    write_json_object(pending_path, pending)
    print("[OK] stale session cleanup applied")
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


def check_scratch_workspaces(root: Path) -> Check:
    scratch_root = root / ".agentcall" / "workspaces"
    if not scratch_root.exists():
        return Check("scratch", "OK", "no session scratch directories")
    items = [path for path in scratch_root.iterdir() if path.is_dir()]
    stale = []
    now = time.time()
    for path in items:
        try:
            age_seconds = now - path.stat().st_mtime
        except OSError:
            continue
        if age_seconds > 24 * 60 * 60:
            stale.append(path.name)
    if stale:
        return Check(
            "scratch",
            "WARN",
            f"{len(items)} scratch dirs, {len(stale)} older than 24h",
            "Review .agentcall/workspaces before manual cleanup; v4.2 does not auto-delete scratch.",
        )
    return Check("scratch", "OK", f"{len(items)} scratch dirs")


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


def check_daemon(root: Path, base_url: str, timeout: float) -> Check:
    try:
        payload = http_json(
            base_url.rstrip("/") + "/api/runtime/health",
            timeout=timeout,
            headers=daemon_auth_headers(root),
        )
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


def size_check(name: str, path: Path, warn_bytes: int) -> Check:
    if not path.exists():
        return Check(name, "WARN", f"missing: {path}")
    size = path.stat().st_size
    status = "WARN" if size > warn_bytes else "OK"
    hint = "Large log should be rotated or queried through recent-first paths." if status == "WARN" else None
    return Check(name, status, f"{format_bytes(size)} at {path}", hint)


def dir_size_check(name: str, path: Path, warn_bytes: int) -> Check:
    if not path.exists():
        return Check(name, "OK", f"missing/empty: {path}")
    size = sum(item.stat().st_size for item in path.rglob("*") if item.is_file())
    status = "WARN" if size > warn_bytes else "OK"
    hint = "Large hook/artifact directory is expected over time; board should not scan it by default." if status == "WARN" else None
    return Check(name, status, f"{format_bytes(size)} at {path}", hint)


def format_bytes(size: int) -> str:
    value = float(size)
    for unit in ["B", "KB", "MB", "GB"]:
        if value < 1024 or unit == "GB":
            return f"{value:.1f}{unit}"
        value /= 1024
    return f"{size}B"


def parse_duration_seconds(text: str) -> int:
    text = text.strip().lower()
    if text.endswith("ms"):
        return max(1, int(float(text[:-2]) / 1000))
    if text.endswith("s"):
        return int(float(text[:-1]))
    if text.endswith("m"):
        return int(float(text[:-1]) * 60)
    if text.endswith("h"):
        return int(float(text[:-1]) * 60 * 60)
    return int(float(text))


def timestamp_age_seconds(timestamp: object, now: float) -> float:
    if not isinstance(timestamp, str):
        return float("inf")
    normalized = timestamp.replace("Z", "+00:00")
    try:
        from datetime import datetime

        parsed = datetime.fromisoformat(normalized)
        return max(0.0, now - parsed.timestamp())
    except ValueError:
        return float("inf")


def read_json_object(path: Path) -> dict:
    if not path.exists():
        return {}
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return {}
    return value if isinstance(value, dict) else {}


def write_json_object(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    tmp.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    tmp.replace(path)


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


def http_json(url: str, timeout: float, headers: dict[str, str] | None = None) -> dict:
    request = urllib.request.Request(url, headers=headers or {})
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
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


def daemon_auth_headers(root: Path) -> dict[str, str]:
    token = os.environ.get("AGENTCALL_DAEMON_TOKEN", "").strip()
    if not token:
        token = str(read_local_config(root).get("daemon_token") or "").strip()
    return {"X-AgentCall-Token": token} if token else {}


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


def default_daemon_binary(root: Path) -> Path:
    return (root / "target" / "debug" / executable_name("agentcall-daemon")).resolve()


def executable_name(name: str) -> str:
    return f"{name}.exe" if os.name == "nt" else name


def paths_same(left: Path, right: Path) -> bool:
    try:
        return left.samefile(right)
    except OSError:
        return os.path.normcase(str(left)) == os.path.normcase(str(right))


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
