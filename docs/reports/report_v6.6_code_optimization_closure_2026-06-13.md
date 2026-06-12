# AgentCall v6.6 代码优化闭口报告

> 日期：2026-06-13  
> 基准报告：`docs/reports/report_code_reasonability_2026-06-13.md`  
> 本轮目标：在不改写冻结计划 `docs/v6.2-code-plan.md` 的前提下，推进 v6.6 代码级闭口。

## Summary

v6.6 关闭了四类直接影响真实调用体验和控制面可维护性的事项：

- 安全锁错误码从字符串表升级为 Rust `ErrorCode` 枚举入口。
- RuntimeStore writer 支持 SQLite/WAL 下最多 6 路并行写入；JSON 后端保持单 writer 安全模式。
- `prompt_pending_ack` 不再默认要求 Codex 手动 `submit_pending_prompt`，daemon 会在短 grace 后自动提交 commit signal。
- `projection` / `worker_state` 中最关键的大分支逻辑已拆出 reducer 和 decision helpers。

## Closure Table

| 审计项 | 原报告优先级 | v6.6 状态 | 代码落点 | 说明 |
|---|---:|---|---|---|
| 错误类型退化为字符串 | P0/P1 | **部分关闭** | `crates/agentcall-daemon/src/errors.rs` | 新增 `ErrorCode` enum，`structured_error` 改为枚举入口；HTTP status、metadata、message classification 统一走 enum。仍保留部分 `Result<T, String>` 作为函数签名，后续可继续替换为 typed error。 |
| 常见安全锁 400 不透明 | P0 | **关闭** | `errors.rs`, `ownership.rs`, `scheduler.rs`, `http.rs` | `workspace_busy`、`owner_lease_exists`、`owner_conflict`、`capacity_exceeded` 等已用 enum-backed structured error。 |
| 单线程 store writer 瓶颈 | P0 | **部分关闭** | `crates/agentcall-daemon/src/store.rs`, `store_sqlite.rs`, `state.rs` | `StoreWriterRuntimeStore` 支持按 shard fan-out；SQLite 后端可用 `store_writer_threads=6`，JSON 后端自动降为 1，避免并发 read-modify-write 丢状态。 |
| Runtime health 缺 writer 信息 | P1 | **关闭** | `crates/agentcall-daemon/src/summary.rs` | `runtime_health` 增加 `store_writer_threads`、`configured_store_writer_threads`、`store_parallel_writes`。 |
| `prompt_pending` / prompt commit 二次确认 | P0 | **关闭** | `crates/agentcall-daemon/src/prompt_gate.rs`, `mcp.rs` tests | `prompt_pending_ack` 超过 2 秒无 `UserPromptSubmit` 时 daemon 自动发送 commit signal；手动 `submit_pending_prompt` 仍保留为 debug/recovery。 |
| `apply_event_to_projection` 巨型 reducer | P0 | **部分关闭** | `crates/agentcall-daemon/src/projection.rs` | 拆出 `reduce_session_start_event`、`reduce_command_progress_event`、`reduce_runtime_failure_event` 等小 reducer；完整 enum 状态机仍留后续。 |
| `worker_state_for_session` 巨型状态推导 | P0 | **部分关闭** | `crates/agentcall-daemon/src/worker_state.rs` | 拆出 `decide_worker_state` / `decide_prompt_gate_state`，将输入收集与状态判断分离；完整转换表仍留后续。 |
| `SessionProjectionV1` 字符串状态字段 | P0 | **未关闭** | `projection.rs` | 本轮没有改公共 projection JSON 字段类型；建议后续内部 enum + serde string 输出，避免 MCP 契约漂移。 |
| HTTP/WebSocket 自研 parser | P0/P1 | **未关闭** | `http.rs` | 本轮不迁移到 `hyper`/`tungstenite`；当前优先级低于真实调用中的 prompt/lease/error/store 热点。 |
| JSON/NDJSON advisory lock | P1 | **未关闭，改为规避** | `store.rs` | 不把 JSON 多写作为 v6.6 路线；JSON 保持单 writer，SQLite 才允许多 writer。 |

## Validation

- `cargo-1.95.0-msvc.cmd test --workspace --target-dir .agentcall_build\target-v660`：通过，171 daemon tests + 2 hook tests + 10 MCP tests。
- `python -m pytest -q`：通过，17 tests。
- `python agentcall.py release-check`：通过。
- `python agentcall.py runtime-release --version 6.6.0 ...`：通过，live daemon version `6.6.0`，`active_pty_sessions=0`。
- `python agentcall.py daemon-health`：通过，live health 显示 `store_backend=sqlite`、`store_writer_threads=6`、`store_parallel_writes=true`。
- 新增覆盖：
  - JSON 后端配置 6 writer 时仍实际使用 1 writer。
  - SQLite 后端配置 6 writer 时实际使用 6 writer，并发 idempotency 写入成功。
  - `prompt_pending_ack` 在短 grace 后自动进入 `commit_signal_sent`。

## Remaining Work

- 把更多 `Result<T, String>` 签名逐步替换为 typed error，而不是只在响应层包装。
- 将 projection 的状态字段内部 enum 化，同时保持外部 JSON 字符串稳定。
- 如果真实日志继续显示 store 仍是瓶颈，再把默认 live config 切到 SQLite，并对 JSON 后端做迁移/只读兼容策略；本轮只更新模板和 runtime health。
- `apply_event_to_projection` 和 `worker_state` 已拆第一刀，但完整状态转换表仍是后续架构任务。
