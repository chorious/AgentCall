# AgentCall v5.x/v5.2 功能实现审查报告

**审查日期:** 2026-06-09
**审查范围:** AgentCall 当前 HEAD（工作树 clean）
**审查方法:** 只读源码审查 + 文档-代码对照 + 测试存在性验证
**参考文档:** `docs/v5.0-code-plan.md`, `docs/v5.1-code-plan.md`, `docs/v5.2-code-plan.md`, `docs/reports/v5-implementation-alignment.md`, `docs/reports/v5-implementation-deep-audit.md`

---

## 1. 总体结论

当前 AgentCall 实现**不是 v5.2 正式版本**，而是一个 **v4.3.0 功能 + v5.0/v5.1/v5.2 部分模块的集成里程碑**。README 和插件 manifest 均声明版本为 `v4.3.0`，Rust crate 版本号为 `3.0.0`，代码中没有任何地方声明 v5.x 版本。

v5 计划文档中列出的**核心模块、数据契约、happy-path 都已真实落地**，107 个 Rust 测试通过。但 v5 计划反复强调的**三条最硬安全闸门未真正闭合**（ precondition seq 校验、失败态投影、控制/输出通道分离）。此外存在**版本号不一致、schema 漂移、primitive write 暴露**等问题。

当前应被视为 **integration milestone**，而非 v5 sign-off。

---

## 2. 已兑现功能（按模块）

### 2.1 MCP Canonical 工具面 ✅

| 工具 | daemon 端 | MCP bridge 端 | 状态 |
|---|---|---|---|
| `agentcall_daemon` | N/A (bridge 本地处理) | `bootstrap.rs` | ✅ |
| `agentcall_board` | `mcp.rs:162` | `tools.rs:36` | ✅ |
| `agentcall_route` | `mcp.rs:176` | `tools.rs:55` | ✅ |
| `agentcall_session` | `mcp.rs:182` | `tools.rs:91` | ✅ |
| `agentcall_session_send` | `mcp.rs:264` | `tools.rs:111` | ✅ |
| `agentcall_report` | `mcp.rs:519` | `tools.rs:135` | ✅ |

- 工具名、参数结构、默认值在 daemon 和 MCP bridge 两侧**基本一致**。
- `agentcall_session` 默认走 `session_projection_summary`（projection-only），仅在显式 `include` 时读取 live session / clean_tail / plan / events。
- `agentcall_board(view=compact, filter=attention)` 走 `board_attention_projection` 热路径，不扫描完整状态文件。
- **Schema 漂移**: daemon `mcp.rs:51` 中 `runtime` enum 为 `["auto", "pty", "sdk"]`，但 MCP bridge `tools.rs:65` 中仅为 `["auto", "pty"]`。SDK runtime 在 bridge 侧不可见但 daemon 侧可接受。证据：`crates/agentcall-daemon/src/mcp.rs:51` vs `crates/agentcall-mcp/src/tools.rs:65`。

### 2.2 Daemon / Board / Session / Report 主流程 ✅

| 流程 | 实现位置 | 状态 |
|---|---|---|
| Daemon HTTP 服务 | `http.rs:36` | ✅ |
| Board compact+attention 热路径 | `summary.rs:42`, `projection.rs:231` | ✅ |
| Session projection 默认摘要 | `projection.rs:139` | ✅ |
| Route PTY 启动 | `routes.rs:444`, `runtime_pty.rs:35` | ✅ |
| Report + ConfidenceLedger | `confidence.rs:30`, `mcp.rs:519` | ✅ |
| Event cursor 查询 | `store.rs:12`, `mcp.rs:221` | ✅ |
| Idle session cleanup | `summary.rs:390` | ✅ |

- Board 默认 compact+attention 路径**确实在 cold read 之前返回**，符合 v5.0 P0-1 要求。证据：`summary.rs:42-44` 中 `if view == Some("compact") && filter == Some("attention")` 在函数体最前面。
- Session 默认 summary 确实只读 projection，不读 transcript。证据：`mcp.rs:182-217` 中默认路径返回 `session_projection_summary`，`clean_tail`/`plan`/`events` 需要显式 include。

### 2.3 Hooks / cwd / UTF-8 / Log Rotation / Idle Cleanup / Permission Interaction

| 功能 | 实现位置 | 状态 | 证据 |
|---|---|---|---|
| Claude hooks 接入 | `hooks.rs:176` | ✅ | `hooks.rs:176 ingest_hook` |
| Hook context injection | `hooks.rs:295` | ✅ | `hooks.rs:295 context_injection` |
| Supervisor instruction queue | `hooks.rs:150` | ✅ | `hooks.rs:150 queue_supervisor_instruction` |
| Policy deny 聚合 + block | `hooks.rs:1009` | ✅ | `hooks.rs:1009 record_policy_denial_locked` |
| PTY path enforcement | `hooks.rs:616` | ✅ | `hooks.rs:616 pty_path_policy_for_wrapper` |
| Plan phase gate | `hooks.rs:828` | ✅ | `hooks.rs:828 pty_plan_policy_decision` |
| CWD 强制为 claude_workspace | `session.rs:316` | ✅ | `session.rs:316 resolve_session_cwd` |
| UTF-8 环境变量 | `session.rs:107` | ✅ | `session.rs:107-110 PYTHONUTF8/LANG/LC_ALL` |
| Log rotation (recent + archive) | `state.rs:493` | ✅ | `state.rs:493 append_rotating_ndjson` |
| Hook 分类日志 | `state.rs:480` | ✅ | `state.rs:480 append_hook_index` |
| Artifact 外置 (大 payload) | `state.rs:320` | ✅ | `state.rs:320 sanitize_tool_response` |
| Idle/stale session cleanup | `summary.rs:390` | ✅ | `summary.rs:390 cleanup_stale_runtime_state` |
| Pending instruction cleanup | `summary.rs:443` | ✅ | `summary.rs:443 pending supervisor cleanup` |
| Permission menu select_option | `mcp.rs:295` | ✅ | `mcp.rs:295 select_option handling` |
| Needs permission 检测 | `summary.rs:488` | ✅ | `summary.rs:488 session_summary attention_status` |
| 60s patience contract | `summary.rs:855` | ✅ | `summary.rs:855 patience_contract` |
| Runtime lock (单实例) | `runtime_lock.rs` | ✅ | `main.rs:56 acquire_runtime_lock` |
| HTTP body 1MiB 上限 | `http.rs:33` | ✅ | `http.rs:33 MAX_HTTP_BODY_BYTES` |
| WebSocket frame 64KiB 上限 | `http.rs:34` | ✅ | `http.rs:34 MAX_WS_FRAME_BYTES` |

### 2.4 v5.2 Durable Store / Scheduler / Runtime Trait ✅

| 功能 | 实现位置 | 状态 |
|---|---|---|
| RuntimeStore trait | `store.rs:89` | ✅ |
| StoreWriter actor 串行化 | `store.rs:170` | ✅ |
| JsonRuntimeStore | `store_json.rs` | ✅ |
| SqliteRuntimeStore + 迁移 | `store_sqlite.rs:16` | ✅ |
| SQLite 真事务回滚 | `store_sqlite.rs:266` | ✅ |
| Event + projection 同事务 | `store_sqlite.rs:266` | ✅ |
| Command + event + projection 同事务 | `store_sqlite.rs:346` | ✅ |
| Route + lease 同事务 | `store_sqlite.rs:375` | ✅ |
| WorkerScheduler (capacity reject) | `scheduler.rs:18` | ✅ |
| AgentRuntime trait | `runtime.rs:40` | ✅ |
| ClaudeCodePtyRuntime | `runtime_pty.rs:11` | ✅ |
| ClaudeCodeSdkRuntime (gated) | `runtime_sdk.rs:9` | ✅ |
| ConfidenceLedger (deterministic) | `confidence.rs:16` | ✅ |
| Owner/workspace lease | `ownership.rs` | ✅ |
| Windows Job Object kill tree | `process.rs` | ✅ |
| Idempotency key 去重 | `commands.rs:67` | ✅ |
| SessionActor + command inbox | `actor.rs:54` | ✅ |
| Architecture audit 脚本 | `scripts/agentcall_arch_audit.py` | ✅ |

---

## 3. 未兑现或风险项

### 🔴 P0 — 安全闸门未闭合

#### P0-1: precondition seq 一致校验【完全未实现】

- **Plan 要求**: `precondition.projection_last_session_seq` 不匹配 → 返回 `rejected_precondition`，防止 stale send。
- **实际**: `CommandEnvelopeV1` 中的 `precondition` 字段被解析、写入 store，但**全代码库无任何一处把它和当前 projection seq 做比较**。`rejected_precondition` 字符串在源码中 0 次出现。
- **证据**:
  - `commands.rs:42-48` `CommandPrecondition` 定义包含 `projection_last_session_seq`
  - `commands.rs:67-108` `prepare_session_send_command` 只检查 precondition 对象是否存在（`requires_precondition`），不校验其值
  - `actor.rs:135-251` `execute_command` 完全不读取 precondition
- **缺失测试**: `session_send_precondition_seq_mismatch_rejected`、`projection_seq_precondition_prevents_stale_send`（plan 列出，均不存在）
- **影响**: plan 主打的"防陈旧写入 / 乐观并发"安全闸门是空的。**自评报告未提及此缺口。**

#### P0-2: actor panic / writer-closed / orphaned 失败态投影【未实现】

- **Plan 要求** (v5.1 Gate #7 + Hard Correction #11): actor panic、writer closed、process orphaned 必须投影为 `failed_or_orphaned`，不得继续显示 healthy running。
- **实际**: `actor.rs:121` `session_actor_loop` 在 `execute_command` panic 时**线程直接结束，无任何 failed/orphaned 投影或事件**；writer 写失败只把 `Err` 返回给调用方，session 状态不变。
- **证据**:
  - `actor.rs:115-133` `session_actor_loop` 是裸 `for command in receiver` loop，无 panic catch
  - `actor.rs:123-125` `execute_command` 结果只通过 `reply.send()` 返回，panic 时整个 loop 终止
- **缺失测试**: `actor_panic_marks_session_failed_or_orphaned`、`writer_closed_marks_session_failed`、`orphaned_projection_not_reported_alive`
- **自评报告 Next Todo 已自认此项未做**（"Add actor failure / writer-closed projection tests"）。

#### P0-3: control / output 通道分离 + stop 优先级【类型层违反，行为层部分缓解】

- **Plan 要求** (v5.1 Hard Correction #8): `ActorControlCommand` 只能承载小型控制消息，**严禁把 `PtyOutput(Vec<u8>)` 放入 control inbox**；stop/interrupt/kill 必须优先于 output。
- **实际**:
  - `actor.rs:49` `ActorControlCommand::RawWrite(Vec<u8>)` 直接把字节塞进 control inbox——**字面违反**。
  - `actor.rs:59` actor 只有**单条 unbounded `mpsc::channel`**，无 `output_rx`、无 `PtyOutputSignal`、无 coalescing、无优先级；排在前面的 `Submit` 会让后到的 stop 排队等待。
  - v5.1 plan 设计的 `SessionActor{ control_rx, output_rx, projection, seq, owner_lease... }` 结构体**根本不存在**，只剩一个 loop 函数 + writer。
- **行为缓解**: 真正的高频 PTY 输出走 reader → replay，不进 actor；`RawWrite` 只用于光标位置自动回复（`session.rs:203` 小字节），所以"输出洪水阻塞 stop"的现实风险较低。
- **证据**:
  - `actor.rs:47-52` enum 定义
  - `actor.rs:54-77` `spawn_session_actor` 只创建一个 channel
  - `session.rs:199-203` `RawWrite` 仅用于 `\x1b[1;1R` 光标回复
- **缺失测试**: `control_inbox_and_output_channel_are_separate`、`high_volume_pty_output_does_not_delay_interrupt`、`stop_command_has_priority_over_output_chunks`

### 🟠 P1 — 实现漂移/文档不一致

#### P1-1: 版本号不一致

- **README**: `v4.3.0`
- **插件 manifest** (`plugins/agentcall/.codex-plugin/plugin.json`): `v4.3.0`
- **Rust crates** (`crates/*/Cargo.toml`): `v3.0.0`
- **v5 计划文档**: 存在 v5.0/v5.1/v5.2 code plan，但代码中**没有任何地方声明 v5.x 版本**
- **影响**: 用户/开发者无法从代码本身判断当前实现对应哪个版本计划。CHANGELOG 只到 v4.3.0，v5 工作完全在 arch plan 文档中，未进入 CHANGELOG。

#### P1-2: Session 类型仍持有可写 PTY 句柄

- **Plan 要求** (v5.1 Gate #1): Session 类型不得持有可写 PTY stdin handle；理想 Session 只含 replay/metadata/decode/clients。
- **实际**: `session.rs:19` `Session` 仍持有 `master: Mutex<Box<dyn MasterPty>>`、`child`、`killer`。writer 本体已移交 actor（单写者行为成立），但 `master` 可再 `take_writer()`。
- **证据**: `session.rs:19-35` struct Session 定义
- **当前靠**: "writer 已被 take + arch audit grep" 兜住，非类型保证。

#### P1-3: stop 与 kill 实现相同，无 graceful 升级

- **Plan 要求**: stop = graceful → interrupt → kill 升级；kill = 最终强杀。
- **实际**: `actor.rs:156` StopSession 与 KillSession 都调用同一个 `stop_session()`（`session.rs:382` 立即 `kill_tree()` + `killer.kill()`）。
- **证据**: `actor.rs:155-163` match arm 对 StopSession/KillSession 处理相同；`session.rs:382-432` `stop_session` 直接 kill

#### P1-4: RuntimeStore trait 公开暴露 primitive write

- **Plan 要求** (v5.2 行 184-187): `append_event` / `update_projection` / `register_command` / `acquire_workspace_lease` 等 primitive write **不得进入 public trait**，只能 private 或 `#[cfg(test)]`。
- **实际**: `store.rs:89` trait 公开 `upsert_owner_lease` / `release_owner_lease` / `record_file_read` / `record_file_write` / `save_report_index` / `save_artifact_index` / `renew_owner_lease`。文件顶部 `#![allow(dead_code)]`。
- **证据**: `store.rs:89-134` trait 定义
- **已在** `docs/reports/v5.2-live-write-audit.md` 记为 "side-index debt"，属有意识延后。

#### P1-5: commands.rs 的 append-only registry 测试是 `#[cfg(test)]` 重实现

- `check_or_record_idempotency` / `commands_index_path` / `append_command_registry_line` / `rebuild_commands_index_from_log` 全部 `#[cfg(test)]`（`commands.rs:204+`）。
- 即 `commands_index_rebuilds_from_append_only_log` 测的是**测试专用副本**，不是生产路径。
- **证据**: `commands.rs:204-326` 全部标有 `#[cfg(test)]`
- 生产 idempotency/重建走 `state.store`（真实，由 `json_store_rebuilds_corrupt_command_index_from_logs` 覆盖）。

### 🟡 P2 — 其他不一致/风险

#### P2-1: pytest 结果不一致

- **自评报告声称**: `python -m pytest -q` → 17 passed
- **本次实测**: 3 passed, 14 errors（Windows 文件锁 teardown 问题）
- **影响**: "干净 17 passed" 在本机无法复现。errors 属环境/teardown 问题，非测试逻辑失败，但会影响 CI 可信度。

#### P2-2: plan 测试名与实际测试名不匹配

- 自评报告多处称 "Covered by X test"，但抽查 plan 列出的 20 个测试名**逐字 0 命中**。
- 大多等价改名覆盖（如 `interrupt_sent_does_not_mark_command_completed` 存在），但 G1/G2/G3 那 8 个安全相关测试**既无原名也无等价**。

#### P2-3: SDK runtime schema 不一致

- daemon `mcp.rs:51` route tool schema: `runtime` enum 含 `"sdk"`
- MCP bridge `tools.rs:65` route tool schema: `runtime` enum 仅 `["auto", "pty"]`
- 导致 Codex 通过 MCP 无法显式请求 sdk runtime，但 daemon HTTP API 可以。

#### P2-4: WebSocket resize 绕过 actor

- `http.rs:419` WebSocket resize 消息直接调用 `session.master.lock().unwrap().resize()`，**不通过 actor command path**。
- 这是只读控制操作，不写入 stdin，但 plan 的"所有控制走 actor"精神未完全贯彻。

#### P2-5: `file_claims.json` 等仍走 direct JSON write

- 以下状态仍由 `hooks.rs` / `routes.rs` 直接写 JSON，不走 `RuntimeStore`：
  - `file_claims.json` (`hooks.rs:651`)
  - `active_sessions.json` (`hooks.rs:493`)
  - `runtime_binding.json` (`hooks.rs:507`)
  - `pending_supervisor_instructions.json` (`hooks.rs:396`)
  - `policy_denials.json` (`hooks.rs:1018`)
- 已在 `v5.2-live-write-audit.md` 明确记为 side-index debt，不是 durable session truth。

---

## 4. 建议优先级

### P0（必须修复才能称 v5.2 完成）

1. **闭合 G1 — precondition seq 校验**: 在 `actor.rs::execute_command` 派发前或 `commands.rs::prepare_*` 中读取当前 projection 的 `projection_last_session_seq` 与 `precondition` 比较，不匹配返回 `rejected_precondition`。补 `session_send_precondition_seq_mismatch_rejected` 测试。
2. **闭合 G2 — 失败态投影**: `session_actor_loop` 用 panic catch/守卫，actor 退出 / writer 写失败时写 `failed_or_orphaned` 投影 + 事件；daemon 启动对无法确认 ownership 的 running session 标 orphaned。补对应 3 个测试。
3. **闭合 G3 — 控制/输出分离**: 至少把 `RawWrite` 移出 `ActorControlCommand`（独立小通道），并加 stop/interrupt 相对 queued submit 的优先处理。补 `stop_command_has_priority_over_output_chunks` 测试。

### P1（重要，应在 v5.2 后续 hardening 中处理）

4. **统一版本号**: 决定当前版本是 v4.3.0、v5.0 还是其他，统一 README、Cargo.toml、plugin.json、CHANGELOG。
5. **P1-2/P1-3/P1-4**: Session 类型剥离 writer、stop/kill 分级、RuntimeStore primitive write 私有化——在自评报告中显式标红，不要计入 "core gates complete"。
6. **修正自评报告**: 增列 G1（完全遗漏）、把 pytest 结果改为环境相关说明。

### P2（改进项）

7. **统一 SDK runtime schema**: 让 MCP bridge 和 daemon 的 `runtime` enum 一致（都含或都不含 `sdk`）。
8. **WebSocket resize 走 actor**: 即使只读控制，也应统一走 command path。
9. **解决 pytest Windows teardown 问题**: 使用 `tmp_path_factory` 或更健壮的临时目录清理。
10. **将 side-index JSON writers 逐步迁入 RuntimeStore**: 按 `v5.2-live-write-audit.md` 的 follow-up 计划执行。

---

## 5. 证据索引

| 问题 | 文件路径 | 行号/函数 |
|---|---|---|
| precondition 未校验 | `crates/agentcall-daemon/src/commands.rs` | 42-48, 67-108 |
| actor 无 panic catch | `crates/agentcall-daemon/src/actor.rs` | 115-133 |
| RawWrite 在 control inbox | `crates/agentcall-daemon/src/actor.rs` | 47-52 |
| Session 仍持 master | `crates/agentcall-daemon/src/session.rs` | 19-35 |
| stop == kill | `crates/agentcall-daemon/src/actor.rs` | 155-163; `session.rs` 382-432 |
| primitive write 在 public trait | `crates/agentcall-daemon/src/store.rs` | 89-134 |
| test-only idempotency | `crates/agentcall-daemon/src/commands.rs` | 204-326 |
| SDK schema 不一致 | `crates/agentcall-daemon/src/mcp.rs` vs `crates/agentcall-mcp/src/tools.rs` | 51 vs 65 |
| WebSocket resize 绕过 actor | `crates/agentcall-daemon/src/http.rs` | 419 |
| version 不一致 | `README.md`, `Cargo.toml`, `plugins/agentcall/.codex-plugin/plugin.json` | 3, 3, 3 |
| SQLite 事务回滚 | `crates/agentcall-daemon/src/store_sqlite.rs` | 266-403 |
| StoreWriter 串行化 | `crates/agentcall-daemon/src/store.rs` | 170-446 |
| projection fast path | `crates/agentcall-daemon/src/projection.rs` | 139, 231 |
| board attention fast path | `crates/agentcall-daemon/src/summary.rs` | 42-44 |
| hook policy block | `crates/agentcall-daemon/src/hooks.rs` | 1009-1100 |
| confidence ledger | `crates/agentcall-daemon/src/confidence.rs` | 16-120 |
| scheduler reject | `crates/agentcall-daemon/src/scheduler.rs` | 18-89 |
| Windows Job Object | `crates/agentcall-daemon/src/process.rs` | 31-85 |
| log rotation | `crates/agentcall-daemon/src/state.rs` | 493-520 |
| UTF-8 env | `crates/agentcall-daemon/src/session.rs` | 107-110 |
| architecture audit | `scripts/agentcall_arch_audit.py` | 全文件 |

---

## 6. 测试覆盖摘要

| 测试集 | 结果 | 备注 |
|---|---|---|
| `cargo test -p agentcall-daemon` | 100 passed | 含 SQLite 事务回滚、projection reducer、actor command、lease、scheduler |
| `cargo test -p agentcall-mcp` | 7 passed | 含 MCP timeout 分类、tool text cap |
| `python scripts/agentcall_arch_audit.py` | passed | 5 项架构审计全部通过 |
| `python -m pytest -q` | 3 passed, 14 errors | Windows 文件锁 teardown 问题 |
| `python scripts/agentcall_dev.py smoke real-worker --store-backend json` | passed | 自评声称 |
| `python scripts/agentcall_dev.py smoke real-worker --store-backend sqlite` | passed | 自评声称 |

---

*报告结束。本报告为只读审查产物，未修改任何生产源码。*
