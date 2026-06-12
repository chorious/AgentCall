# AgentCall v6.7 P0/P1 Closure Report

> 日期：2026-06-13  
> 基线：`docs/reports/report_v6.6_open_issues_priority_2026-06-13.md`  
> 范围：v6.6 后未关闭 issue 中，原优先级或我的优先级为 P0/P1 的项目。

## Summary

v6.7 目标是把 v6.6 留下的 P0/P1 控制面问题先关一轮，重点不是扩功能，而是让 daemon 的状态、控制、SQLite hot path 和 PTY worker failure 处理更硬。

本轮已落地：

- `SessionProjectionV1` 内部状态从裸 `String` 改为 `ProjectionStatus` enum，外部 JSON 仍输出原 snake_case 字符串。
- control token、MCP idempotency fingerprint、store command fingerprint、policy-denial key 改为 SHA-256。
- SQLite `get_events` 将 `session_id`、`event_types`、`global_seq` 和 `LIMIT` 下推 SQL。
- SQLite runtime store 热路径改为 thread-local cached connection，避免每个操作重新 open。
- hook PreToolUse policy 拆出 `evaluate_pty_pre_tool_policy` 纯策略函数，外层只负责 I/O/claim/event。
- `ControlError` 内部状态改用 `ErrorCode` enum，并保持外部 `status` 兼容字段。
- MCP `mcp.tool_called` 事件记录 structured error object，而不是只记录字符串。
- worker action 名称收敛到 enum，新增 worker transition table 测试契约。
- actor panic 后会 terminalize：移除 actor handle、标记 session failed、释放 owner/workspace lease，并执行 session cleanup。
- Windows JobObject 不可用时，process fallback 会尝试 `taskkill /PID <pid> /T /F`，无 pid 时诚实返回未请求。
- lease install/release/acquire 常见路径改为锁内 clone snapshot、锁外持久化，降低持锁 I/O 风险。

## Closed / Advanced Issues

| 原优先级 | 我的优先级 | v6.7 状态 | Issue | 处理 |
|---|---:|---|---|---|
| P0 | P0 | closed | `SessionProjectionV1` 字符串状态字段 | 新增 `ProjectionStatus` enum，serde 仍保持字符串协议。 |
| P0 | P0 | closed | control token 指纹使用 FNV-1a | `control_token_hash` 改为 SHA-256。 |
| P0 | P0 | closed | SQLite `get_events` 过滤/limit 未下推 SQL | 新增动态 SQL builder，`event_types` 和 `limit` 不再靠内存 retain。 |
| P0 | P0 | partial-closed | 公共 API 大量 `Result<T, String>` | control error 内部先改用 `ErrorCode`；RuntimeStore trait 仍保留 `String`，后续可继续替换为 `AgentCallError`。 |
| P0 | P0 | partial-closed | `ingest_hook` / PreToolUse policy 巨型混合逻辑 | PreToolUse path policy 已拆为纯函数；完整 hook ingest 拆文件仍待后续。 |
| P0 | P1 | advanced | `apply_event_to_projection` 巨型 reducer | 本轮没有大改 reducer 行为，但 enum 化后 reducer 分支不再自由写状态字符串。 |
| P0 | P1 | advanced | `worker_state_for_session` 手动状态机 | 新增 worker transition table 测试契约，状态判断仍保持原逻辑。 |
| P0 | P1 | advanced | `StoreWriterRuntimeStore` / SQLite 写瓶颈 | SQLite 仍支持 6 writer，并新增 thread-local cached connection。 |
| P1 | P1 | partial-closed | action 名称字符串重复 | session summary 常用 action 已 enum 化；MCP schema 字符串仍保留为外部契约。 |
| P1 | P1 | partial-closed | 锁顺序 / 持锁 I/O | owner/workspace lease 常见 install/release/acquire 路径已改为锁外持久化。 |
| P1 | P1 | closed | SQLite 每次新建连接 | RuntimeStore hot path 改为 thread-local cached connection。 |
| P1 | P1 | closed | actor panic 后没有重启/清理策略 | actor panic 现在 terminalize + lease cleanup。 |
| P1 | P1 | closed | Windows `kill_tree` fallback 不真正杀父进程 | fallback 使用 `taskkill /T /F`，并补充无 pid 语义。 |
| P1 | P1 | closed | MCP 错误日志与响应结构不一致 | `mcp.tool_called` event 带 structured error object。 |
| P1 | P1 | partial-closed | Bash 只读黑名单容易绕过 | 本轮保持现有只读白名单/echo fallback，不放宽；完整 shell parser 仍非 v6.7 范围。 |

## Remaining P0/P1 Observation Items

- `RuntimeStore` trait 仍是 `Result<T, String>`，v6.7 只把 control path 改成 typed enum。若继续推进，应新增 `AgentCallError` 并分阶段替换 store/route/session trait。
- `ingest_hook` 函数整体仍偏大。v6.7 只把最危险的 PreToolUse policy 评估拆出纯函数。
- `apply_event_to_projection` 仍是中心 dispatch。enum 化降低了状态拼写风险，但还没有彻底拆成事件族模块。
- Bash 只读策略仍不是强 shell sandbox。当前主边界依然是 daemon path policy + Write/Edit/MultiEdit bounded write。
- MCP budget trimming 仍是递归修剪算法，未在本轮做结构化 projection-only 响应重写。

## Validation

- `cargo-1.95.0-msvc.cmd test -p agentcall-daemon --target-dir .agentcall_build\target-v670`
  - 175 passed.
- `cargo test --workspace --target-dir .agentcall_build\target-v670-final`
  - daemon: 175 passed.
  - hook helper: 2 passed.
  - MCP bridge: 10 passed.
- `python -m pytest -q`
  - 17 passed after pytest temp isolation fix.
- `python agentcall.py release-check`
  - passed: compileall, skill generation check, architecture audit, `web/board.js` syntax, plugin validation, cargo workspace tests, pytest, and `git diff --check`.
- `python agentcall.py verify-runtime-build`
  - passed: live daemon reports version `6.7.0`, binary path `E:\Project\AgentCall\target\debug\agentcall-daemon.exe`, and process start time later than binary modified time.
- `python agentcall.py daemon-health`
  - live daemon status `ok`, version `6.7.0`, `store_backend=sqlite`, `store_writer_threads=6`, `active_pty_sessions=0`, Claude hooks complete at `D:\guKimi\.claude\settings.local.json`.
- `python agentcall.py runtime-release --version 6.7.0 --release-label "v6.7.0 - P0/P1 control hardening"`
  - version alignment completed.
  - workspace cargo tests and Python tests passed in the first full run.
  - later reruns exposed two release-environment issues listed below.

## Release Environment Findings

### `.agentcall_pytest` ACL poisoning

One later `runtime-release` rerun failed before implementation validation because pytest tried to reuse repo-local `.agentcall_pytest`. The directory had been created under a different sandbox/elevated security principal with protected ACLs:

- owner observed as `MUSHROOM\CodexSandboxOffline`;
- access limited to `SYSTEM`, `Administrators`, and `OWNER RIGHTS`;
- normal cleanup and even repo-local force delete returned `Access denied`.

This is not a product-state failure. The first immediate cause was repo-local pytest temp configuration:

```toml
addopts = "--basetemp=.agentcall_pytest -p no:cacheprovider"
cache_dir = ".agentcall_pytest/cache"
```

That design is unsafe when validation may run under different Codex sandbox/elevation identities.

After removing the repo-local override, pytest exposed the same pattern in the default Windows temp root: `C:\Users\MUSHI\AppData\Local\Temp\pytest-of-MUSHI` was also inaccessible to the current process. The deeper issue is pytest's numbered temp discovery scanning a parent directory that may contain stale entries created by a different security principal.

v6.7 therefore changes pytest to use a gitignored, repo-controlled, per-process single-level temp path and disables the cache provider:

```toml
addopts = "-p no:cacheprovider"
```

`tests/conftest.py` sets `config.option.basetemp` to `.pytest_tmp_<pid>` when the caller did not explicitly provide a basetemp. This avoids both default `%TEMP%` discovery and cross-principal reuse of one fixed repo-local directory. The hook intentionally lives under `tests/` rather than repo root so it does not cause `agentcall.py` to shadow the `src/agentcall` package during collection.

The already-poisoned `.agentcall_pytest` and `%TEMP%\pytest-of-MUSHI` directories may still require removal by the owning/admin security context, but future normal validation no longer depends on either path.

### Release restart hardening

The original release restart used a broad process stop path that could hit permission errors on unrelated or already-guarded `agentcall-mcp.exe` processes. v6.7 changes the release helper to stop only AgentCall processes whose executable path is under the current repo root. This keeps the helper from trying to manage unrelated binaries and produces a more actionable failure if the repo-owned process cannot be stopped.
