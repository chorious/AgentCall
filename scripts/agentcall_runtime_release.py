from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Iterable


AGENTCALL_PACKAGES = ["agentcall-daemon", "agentcall-hook", "agentcall-mcp"]
DEFAULT_DAEMON_URL = "http://127.0.0.1:3293"


class ReleaseError(RuntimeError):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code


def main() -> int:
    configure_stdio()
    parser = argparse.ArgumentParser(
        description=(
            "Align AgentCall versions, build, restart daemon/MCP binaries, and verify the live daemon version."
        )
    )
    parser.add_argument("--root", default=repo_root(), help="AgentCall repository root.")
    parser.add_argument(
        "--version",
        required=True,
        help="Product version to align everywhere, for example 6.3.0.",
    )
    parser.add_argument(
        "--release-label",
        default=None,
        help="README/docs label. Defaults to v<VERSION>.",
    )
    parser.add_argument(
        "--daemon-url",
        default=DEFAULT_DAEMON_URL,
        help="Daemon URL used for health verification.",
    )
    parser.add_argument(
        "--skip-tests",
        action="store_true",
        help="Skip cargo test and pytest before building.",
    )
    parser.add_argument(
        "--skip-release-check",
        action="store_true",
        help="Skip python agentcall.py release-check after build.",
    )
    parser.add_argument(
        "--no-stop-existing",
        action="store_true",
        help="Do not stop existing agentcall-daemon.exe / agentcall-mcp.exe processes.",
    )
    parser.add_argument(
        "--no-restart",
        action="store_true",
        help="Do not start daemon after build; only align/build/verify files.",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print intended file changes and commands without writing, building, or restarting.",
    )
    args = parser.parse_args()
    root = Path(args.root).resolve()
    label = args.release_label or current_release_label(root, args.version)
    try:
        return run_release(root, args.version, label, args)
    except ReleaseError as exc:
        print(
            json.dumps(
                {"status": "failed", "code": exc.code, "message": str(exc)},
                ensure_ascii=False,
                indent=2,
            ),
            file=sys.stderr,
        )
        return 2


def run_release(root: Path, version: str, label: str, args: argparse.Namespace) -> int:
    require_repo(root)
    validate_version(version)
    print_step("version", f"aligning AgentCall version to {version}")
    changed = align_versions(root, version, label, dry_run=args.dry_run)
    for path in changed:
        print(f"[CHANGED] {path}")
    if args.dry_run:
        print("[DRY-RUN] build/restart steps skipped")
        return 0

    print_step("fmt", "cargo fmt")
    run_checked([cargo_bin(), "fmt"], root, "cargo fmt")

    if not args.skip_tests:
        print_step("test", "cargo test --workspace and pytest")
        run_checked(
            [
                cargo_bin(),
                "test",
                "--workspace",
                "--target-dir",
                ".agentcall_build\\target-runtime-release",
            ],
            root,
            "cargo test --workspace",
            timeout=900,
        )
        run_checked([sys.executable, "-m", "pytest", "-q"], root, "pytest", timeout=300)

    if not args.no_stop_existing:
        print_step("pre-build stop", "stopping AgentCall daemon/MCP processes before overwriting binaries")
        stop_existing_processes(root)

    build_target_dir = ".agentcall_build\\target-runtime-release"
    print_step("build", f"cargo build --workspace --target-dir {build_target_dir}")
    run_checked(
        [cargo_bin(), "build", "--workspace", "--target-dir", build_target_dir],
        root,
        "cargo build --workspace",
        timeout=300,
    )

    if not args.skip_release_check:
        print_step("release-check", "python agentcall.py release-check")
        run_checked([sys.executable, "agentcall.py", "release-check"], root, "release-check", timeout=900)

    runtime_dir = runtime_binary_dir(root, version)
    if not args.no_stop_existing:
        print_step("stop", "stopping stale AgentCall daemon/MCP processes")
        stop_existing_processes(root)
        print_step("sync", f"copying freshly built binaries into target/runtime/{version}")
        runtime_dir = sync_runtime_binaries(root, Path(build_target_dir), version)

    if not args.no_restart:
        print_step("start", "starting daemon as Windows breakaway process")
        existing = daemon_health_if_running(root, args.daemon_url)
        if existing.get("build", {}).get("version") == version:
            print("[OK] daemon already running at expected version")
        else:
            pid = start_daemon_breakaway(root, runtime_dir)
            print(f"[OK] daemon process started pid={pid}")
        print_step("verify", "checking live daemon version")
        health = wait_for_daemon(root, args.daemon_url, version)
        print(
            json.dumps(
                {
                    "status": "ok",
                    "version": health.get("build", {}).get("version"),
                    "daemon_url": args.daemon_url,
                    "binary": health.get("build", {}).get("binary_path"),
                    "started": health.get("build", {}).get("process_started_at"),
                    "active_pty_sessions": health.get("active_pty_sessions"),
                },
                ensure_ascii=False,
                indent=2,
            )
        )
        print("[NOTE] Codex MCP stdio transports may need a new Codex session/plugin reload to bind to the versioned runtime agentcall-mcp.exe.")
    return 0


def align_versions(root: Path, version: str, label: str, dry_run: bool) -> list[str]:
    changed: list[str] = []
    replacements = {
        root / "crates" / "agentcall-daemon" / "Cargo.toml": [
            (r'(?m)^version = "[^"]+"', f'version = "{version}"')
        ],
        root / "crates" / "agentcall-hook" / "Cargo.toml": [
            (r'(?m)^version = "[^"]+"', f'version = "{version}"')
        ],
        root / "crates" / "agentcall-mcp" / "Cargo.toml": [
            (r'(?m)^version = "[^"]+"', f'version = "{version}"')
        ],
        root / "pyproject.toml": [(r'(?m)^version = "[^"]+"', f'version = "{version}"')],
        root / "crates" / "agentcall-mcp" / "src" / "protocol.rs": [
            (
                r'const SERVER_VERSION: &str = "[^"]+";',
                f'const SERVER_VERSION: &str = "{version}";',
            )
        ],
        root / "AGENTS.md": [
            (
                r"(?m)^- Current product version: `[^`]+`\.",
                f"- Current product version: `{version}`.",
            )
        ],
        root / "README.md": [
            (
                r"(?m)^当前版本 / Current version: `[^`]+`",
                f"当前版本 / Current version: `{label}`",
            )
        ],
        root / "docs" / "README.md": [
            (
                r"(?m)^这是 AgentCall 的文档索引。当前主线是 `[^`]+`",
                f"这是 AgentCall 的文档索引。当前主线是 `{label}`",
            )
        ],
    }
    for path, pairs in replacements.items():
        if replace_text(path, pairs, dry_run=dry_run):
            changed.append(str(path.relative_to(root)))

    plugin_path = root / "plugins" / "agentcall" / ".codex-plugin" / "plugin.json"
    if update_plugin_version(plugin_path, version, dry_run=dry_run):
        changed.append(str(plugin_path.relative_to(root)))
    plugin_mcp_path = root / "plugins" / "agentcall" / ".mcp.json"
    if update_plugin_mcp_command(plugin_mcp_path, root, version, dry_run=dry_run):
        changed.append(str(plugin_mcp_path.relative_to(root)))

    lock_path = root / "Cargo.lock"
    if update_cargo_lock(lock_path, version, dry_run=dry_run):
        changed.append(str(lock_path.relative_to(root)))
    return changed


def current_release_label(root: Path, version: str) -> str:
    readme = root / "README.md"
    if readme.exists():
        match = re.search(r"(?m)^当前版本 / Current version: `([^`]+)`", read_text(readme))
        if match and re.search(rf"\bv?{re.escape(version)}\b", match.group(1)):
            return match.group(1)
    return f"v{version}"


def replace_text(path: Path, pairs: Iterable[tuple[str, str]], dry_run: bool) -> bool:
    text = read_text(path)
    new = text
    for pattern, replacement in pairs:
        new_next, count = re.subn(pattern, replacement, new)
        if count == 0:
            raise ReleaseError("version_pattern_missing", f"pattern not found in {path}: {pattern}")
        new = new_next
    if new == text:
        return False
    if not dry_run:
        path.write_text(new, encoding="utf-8", newline="")
    return True


def update_plugin_version(path: Path, version: str, dry_run: bool) -> bool:
    data = json.loads(read_text(path))
    if data.get("version") == version:
        return False
    data["version"] = version
    if not dry_run:
        path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return True


def update_plugin_mcp_command(path: Path, root: Path, version: str, dry_run: bool) -> bool:
    data = json.loads(read_text(path))
    expected = str(root / "target" / "runtime" / version / executable_name("agentcall-mcp"))
    server = data.setdefault("mcpServers", {}).setdefault("agentcall", {})
    if server.get("command") == expected:
        return False
    server["command"] = expected
    if not dry_run:
        path.write_text(json.dumps(data, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return True


def update_cargo_lock(path: Path, version: str, dry_run: bool) -> bool:
    text = read_text(path)
    lines = text.splitlines()
    out: list[str] = []
    current_package: str | None = None
    changed = False
    for line in lines:
        if line == "[[package]]":
            current_package = None
        elif line.startswith("name = "):
            match = re.match(r'name = "([^"]+)"', line)
            current_package = match.group(1) if match else None
        elif line.startswith("version = ") and current_package in AGENTCALL_PACKAGES:
            new_line = f'version = "{version}"'
            if line != new_line:
                line = new_line
                changed = True
        out.append(line)
    if changed and not dry_run:
        path.write_text("\n".join(out) + "\n", encoding="utf-8", newline="")
    return changed


def stop_existing_processes(root: Path) -> None:
    if os.name != "nt":
        return
    root_text = str(root.resolve())
    script = rf"""
$root = {json.dumps(root_text)}
$rootLower = $root.ToLowerInvariant()
$names = @('agentcall-mcp.exe', 'agentcall-daemon.exe')
$matches = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
  Where-Object {{ ($names -contains $_.Name) -and $_.ExecutablePath -and $_.ExecutablePath.ToLowerInvariant().StartsWith($rootLower) }}
if (-not $matches) {{
  Write-Output 'No stale AgentCall processes found under repo root.'
  exit 0
}}
foreach ($proc in $matches) {{
  Write-Output ("Stopping {{0}} pid={{1}} path={{2}}" -f $proc.Name, $proc.ProcessId, $proc.ExecutablePath)
  Stop-Process -Id $proc.ProcessId -Force -ErrorAction Stop
}}
"""
    result = subprocess.run(
        ["powershell.exe", "-NoProfile", "-Command", script],
        cwd=root,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )
    output = result.stdout.strip()
    if output:
        print(output)
    if result.returncode != 0:
        raise ReleaseError("stop_process_failed", f"repo-scoped process stop failed: {output}")


def runtime_binary_dir(root: Path, version: str) -> Path:
    return root / "target" / "runtime" / version


def sync_runtime_binaries(root: Path, build_target_dir: Path, version: str) -> Path:
    src_dir = root / build_target_dir / "debug"
    dst_dir = runtime_binary_dir(root, version)
    dst_dir.mkdir(parents=True, exist_ok=True)
    binaries = [executable_name(package) for package in AGENTCALL_PACKAGES]
    missing = [name for name in binaries if not (src_dir / name).exists()]
    if missing:
        raise ReleaseError(
            "built_binary_missing",
            f"built binaries missing under {src_dir}: {', '.join(missing)}",
        )
    last_error = ""
    for attempt in range(1, 6):
        try:
            if attempt > 1:
                stop_existing_processes(root)
                time.sleep(0.2)
            for name in binaries:
                shutil.copy2(src_dir / name, dst_dir / name)
            return dst_dir
        except PermissionError as exc:
            last_error = str(exc)
            time.sleep(0.25 * attempt)
    raise ReleaseError(
        "binary_sync_locked",
        f"failed to sync runtime binaries into {dst_dir}; locked by live process: {last_error}",
    )

def start_daemon_breakaway(root: Path, runtime_dir: Path) -> int:
    daemon = runtime_dir / executable_name("agentcall-daemon")
    if not daemon.exists():
        raise ReleaseError("daemon_binary_missing", f"daemon binary missing: {daemon}")
    logs = root / ".agentcall" / "logs"
    logs.mkdir(parents=True, exist_ok=True)
    log = open(logs / "daemon-runtime-release.log", "ab", buffering=0)
    flags = 0
    if os.name == "nt":
        flags = 0x00000008 | 0x00000200 | 0x01000000
    try:
        proc = subprocess.Popen(
            [str(daemon), "--workspace", str(root)],
            cwd=root,
            stdin=subprocess.DEVNULL,
            stdout=log,
            stderr=log,
            creationflags=flags,
            close_fds=True,
        )
    except PermissionError as exc:
        raise ReleaseError(
            "daemon_breakaway_denied",
            "Windows denied CREATE_BREAKAWAY_FROM_JOB; run this script from an elevated shell or Codex approval.",
        ) from exc
    time.sleep(1.0)
    if proc.poll() is not None:
        raise ReleaseError("daemon_start_failed", f"daemon exited immediately with code {proc.returncode}")
    return proc.pid


def daemon_health_if_running(root: Path, daemon_url: str) -> dict:
    try:
        return http_json(
            daemon_url.rstrip("/") + "/api/runtime/health",
            headers=daemon_auth_headers(root),
            timeout=1.0,
        )
    except Exception:
        return {}


def wait_for_daemon(root: Path, daemon_url: str, expected_version: str) -> dict:
    deadline = time.time() + 15.0
    last_error = ""
    while time.time() < deadline:
        try:
            health = http_json(
                daemon_url.rstrip("/") + "/api/runtime/health",
                headers=daemon_auth_headers(root),
                timeout=3.0,
            )
            version = str(health.get("build", {}).get("version") or "")
            if version != expected_version:
                raise ReleaseError(
                    "live_version_mismatch",
                    f"daemon live version {version!r} != expected {expected_version!r}",
                )
            return health
        except ReleaseError:
            raise
        except Exception as exc:  # noqa: BLE001
            last_error = str(exc)
            time.sleep(0.5)
    raise ReleaseError("health_timeout", f"daemon did not become healthy: {last_error}")


def http_json(url: str, headers: dict[str, str], timeout: float) -> dict:
    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", errors="replace")
        raise ReleaseError("daemon_http_error", f"HTTP {exc.code}: {body}") from exc
    except urllib.error.URLError as exc:
        raise ReleaseError("daemon_connect_failed", str(exc.reason)) from exc


def daemon_auth_headers(root: Path) -> dict[str, str]:
    config = root / "config" / "agentcall.local.json"
    if not config.exists():
        return {}
    data = json.loads(read_text(config))
    token = str(data.get("daemon_token") or os.environ.get("AGENTCALL_DAEMON_TOKEN") or "").strip()
    return {"X-AgentCall-Token": token} if token else {}


def run_checked(cmd: list[str], cwd: Path, label: str, timeout: int = 120) -> None:
    print("[RUN]", label, ":", " ".join(cmd))
    try:
        subprocess.run(cmd, cwd=cwd, check=True, timeout=timeout)
    except subprocess.TimeoutExpired as exc:
        raise ReleaseError("command_timeout", f"{label} timed out after {timeout}s") from exc
    except subprocess.CalledProcessError as exc:
        raise ReleaseError("command_failed", f"{label} failed with exit code {exc.returncode}") from exc


def cargo_bin() -> str:
    for name in ["cargo-1.95.0-msvc.cmd", "cargo.cmd", "cargo"]:
        found = shutil.which(name)
        if found:
            return found
    candidate = Path.home() / ".codex" / "skills" / "compile-tools" / "scripts" / "wrappers" / "cargo-1.95.0-msvc.cmd"
    if candidate.exists():
        return str(candidate)
    raise ReleaseError("cargo_missing", "cargo wrapper not found; install or refresh compile-tools skill.")


def executable_name(name: str) -> str:
    return f"{name}.exe" if os.name == "nt" else name


def validate_version(version: str) -> None:
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise ReleaseError("invalid_version", "--version must look like 6.3.0")


def require_repo(root: Path) -> None:
    if not (root / "Cargo.toml").exists() or not (root / "scripts").is_dir():
        raise ReleaseError("not_repo_root", f"not an AgentCall repo root: {root}")


def read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError as exc:
        raise ReleaseError("file_missing", f"required file missing: {path}") from exc


def print_step(name: str, detail: str) -> None:
    print(f"\n== {name}: {detail}")


def repo_root() -> str:
    return str(Path(__file__).resolve().parents[1])


def configure_stdio() -> None:
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    if hasattr(sys.stderr, "reconfigure"):
        sys.stderr.reconfigure(encoding="utf-8", errors="replace")


if __name__ == "__main__":
    raise SystemExit(main())
