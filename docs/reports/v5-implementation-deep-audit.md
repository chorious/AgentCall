# AgentCall v5.0 / v5.1 / v5.2 深度实现审计

Date: 2026-06-09
Auditor: 独立代码审计（不采信 `v5-implementation-alignment.md` 的自评，逐条核对 plan 的 Implementation Gate 与实际代码）
HEAD: `4449a4c Complete v5.2 core gates`（工作树 clean）

## 审计方法

不以「测试通过」或「模块文件存在」为结论，而是把三份 plan 里被明确标注为
「开工条件，不是实现建议 / 不允许靠措辞糊过去」的 Hard Corrections + Implementation
Gate 逐条对到源码行。同时核对 plan 列出的测试名是否真实存在。

实跑验证：

```text
cargo test -p agentcall-daemon -p agentcall-mcp
  agentcall-daemon: 100 passed; 0 failed   ← 与自评一致
  agentcall-mcp:      7 passed; 0 failed    ← 与自评一致

python scripts\agentcall_arch_audit.py
  [OK] AgentCall architecture audit passed  ← 通过

python -m pytest -q
  3 passed, 14 errors                        ← 与自评「17 passed」不一致
  errors = PermissionError 删除 .agentcall_pytest 临时目录失败（Windows 文件锁），
  属环境/teardown 问题，非测试逻辑失败；但「干净 17 passed」在本机无法复现。
```

## 总体结论

三份 plan 的**核心模块、数据契约、happy-path 都真实落地**，107 个 Rust 测试为绿。
但**每一版都各有若干被 plan 反复强调的 Hard Gate 没有真正闭合**，且这些恰好是
plan 自己说「不允许糊过去」的安全不变量。自评报告对“已建什么”基本准确，但对
“闭合程度”偏乐观：它把状态标为 first-pass complete，而它自己的 Next Todo 又承认
失败态投影未做，并且**完全没有提到 precondition seq 这条缺口**。

一句话：**不是骨架糊弄，是真实现；但 plan 里最硬的三条安全闸门没关上。**

---

## 确认扎实落地的部分（抽查通过）

### v5.0
- MCP 超时分类：`agentcall-mcp/daemon_client.rs`（`timeout_errors_are_classified_for_mcp_callers`）。
- compact JSON + 128KB cap + 截断预览：`protocol.rs:111 tool_text`，stdout **只写 JSON-RPC**
  （`protocol.rs:32`），timing 写文件、错误走 stderr（`append_timing_log` / `eprintln`）。stdout 规律达标（虽无 `mcp_stdout_*` 测试，但构造上成立）。
- **idempotency 缺失是真·daemon 级拒绝**，不是 warning：`commands.rs:79-88`，且所有 action
  `requires_idempotency=true`（含 `_ =>` 默认分支）。destructive 还要求 precondition 对象存在。
- `EventEnvelopeV1` 拆 `global_seq`/`session_seq`：`state.rs` `event_envelope_v1_tracks_global_and_session_sequence`。
- projection 只经 reducer：`projection.rs` + arch audit 强制 MCP 默认走 `session_projection_summary`。

### v5.1
- 命令统一过 actor inbox：`mcp.rs` / `http.rs` / `routes.rs` / `runtime_pty.rs` 全部调用
  `submit_session_command`，无一处直接 `write_input`。PTY 输出经 reader 直写 replay，不进 actor。
- writer move 进 actor：`session.rs:124 take_writer()` → `spawn_session_actor`，`PtyWriter` 定义在 `actor.rs`。
- owner/workspace lease + **stale lease generation 拒绝**：`ownership.rs:93,101`（`stale_lease_generation_is_rejected`）。
- 同 workspace 不同写法路径冲突：`same_workspace_different_path_spelling_conflicts`（真实存在）。
- Windows Job Object kill tree：`process.rs:212`（`..._when_assignable`，宿主拒绝 job 分配时跳过硬断言，未伪装通过）。
- interrupt `sent` ≠ `completed`：`actor.rs:253 command_terminal_event` 返回 `awaiting_observation`。

### v5.2
- `RuntimeStore` trait + 事务方法 + `StoreWriter` 串行化写：`store.rs`（`store_writer_serializes_concurrent_command_writes`）。
- SQLite **真事务回滚**：`sqlite_command_completion_rolls_back_when_event_insert_fails`、
  `sqlite_route_session_and_leases_roll_back_when_workspace_lease_fails`、owner/idempotency NOT NULL 去重。
- route + owner lease + workspace lease **单事务**：`routes.rs:480 acquire_route_leases_and_create_session`。
- scheduler 无隐藏队列：`scheduler.rs` 仅 reject（global/per-owner capacity），不返回 queued。
- SDK runtime gated：`runtime_sdk.rs` `sdk_runtime_stub_rejects_start_and_submit_without_bypass`、`runtime_health_hides_sdk_runtime_until_enabled`。
- deterministic ConfidenceLedger：`confidence.rs`（无 LLM 解析，含矛盾检测）。

---

## 确认的缺口 / 偏离（按严重度）

### 🔴 G1 — precondition 的 seq 一致校验【完全未实现】
- plan 要求（v5.0 PR5 / v5.1 PR3）：`precondition.projection_last_session_seq` 不匹配
  → 返回 `rejected_precondition`，防止 stale send。
- 实际：`projection_last_session_seq` 在 `CommandEnvelopeV1` 被解析、写入 store，但
  **全代码库无任何一处把它和当前 projection seq 做比较**；`rejected_precondition`
  字符串在源码中 0 次出现。`commands.rs:84` 只校验 destructive 命令“precondition 对象是否存在”，不校验其值。
- 缺失测试：`session_send_precondition_seq_mismatch_rejected`、`projection_seq_precondition_prevents_stale_send`（plan 列出，均不存在）。
- 影响：plan 主打的“防陈旧写入 / 乐观并发”这条安全闸门事实上是空的。**自评报告未提及此缺口。**

### 🔴 G2 — actor panic / writer-closed / orphaned 的失败态投影【未实现】
- plan 要求（v5.1 Gate #7 + Hard Correction #11）：actor panic、writer closed、process
  orphaned 必须投影为 `failed_or_orphaned`，不得继续显示 healthy running。
- 实际：`actor.rs:121 session_actor_loop` 在 `execute_command` panic 时线程直接结束，
  **无任何 failed/orphaned 投影或事件**；writer 写失败只把 `Err` 返回给调用方，session 状态不变。
- 缺失测试：`actor_panic_marks_session_failed_or_orphaned`、`writer_closed_marks_session_failed`、
  `orphaned_projection_not_reported_alive`（均不存在）。
- 自评报告的 Next Todo 已自认此项未做（“Add actor failure / writer-closed projection tests”），属已知坑。

### 🟠 G3 — control / output 通道分离 + stop 优先级【类型层违反，行为层部分缓解】
- plan Hard Correction #8 / Gate #3：`ActorControlCommand` 只能承载小型控制消息，
  **严禁把 `PtyOutput(Vec<u8>)` 放入 control inbox**；stop/interrupt/kill 必须优先于 output。
- 实际：
  - `ActorControlCommand::RawWrite(Vec<u8>)`（`actor.rs:49`）直接把字节塞进 control inbox——字面违反。
  - actor 只有**单条 unbounded `mpsc::channel`**（`actor.rs:59`），无 `output_rx`、无 `PtyOutputSignal`、无 coalescing、无优先级；排在前面的 `Submit` 会让后到的 stop 排队等待。
  - plan 设计的 `SessionActor{ control_rx, output_rx, projection, seq, owner_lease... }` 结构体**根本不存在**，只剩一个 loop 函数 + writer。
  - 行为缓解：真正的高频 PTY 输出走 reader → replay，不进 actor；`RawWrite` 只用于光标位置自动回复（`session.rs:203` 小字节），所以“输出洪水阻塞 stop”的现实风险较低。
- 缺失测试：`control_inbox_and_output_channel_are_separate`、`high_volume_pty_output_does_not_delay_interrupt`、
  `stop_command_has_priority_over_output_chunks`、`output_signal_coalescing_preserves_latest_brief`（均不存在）。

### 🟠 G4 — Session 类型仍持有可写 PTY 句柄【类型层未达标，行为/审计层达标】
- plan Gate #1：Session 类型不得持有可写 PTY stdin handle；理想 Session 只含 replay/metadata/decode/clients。
- 实际：`session.rs:19 Session` 仍持有 `master: Mutex<Box<dyn MasterPty>>`、`child`、`killer`。
  writer 本体已移交 actor（单写者行为成立），但 `master` 可再 `take_writer()`（仅一次），类型层并未真正剥离。
- 当前靠“writer 已被 take + arch audit grep”兜住，非类型保证。缺失测试 `raw_writer_is_not_reachable_from_session_type`。

### 🟡 G5 — stop 与 kill 实现相同，无 graceful 升级
- plan：stop = graceful → interrupt → kill 升级；kill = 最终强杀。两者语义不同。
- 实际：`actor.rs:156` StopSession 与 KillSession 都调用同一个 `stop_session()`
  （`session.rs:382` 立即 `kill_tree()` + `killer.kill()`）。无 graceful 阶段。

### 🟡 G6 — RuntimeStore trait 公开暴露 primitive write
- plan v5.2 硬规则（行 184-187）：`append_event` / `update_projection` / `register_command` /
  `acquire_workspace_lease` 等 primitive write **不得进入 public trait**，只能 private 或 `#[cfg(test)]`。
- 实际：`store.rs:89` trait 公开 `upsert_owner_lease` / `release_owner_lease` / `record_file_read` /
  `record_file_write` / `save_report_index` / `save_artifact_index` / `renew_owner_lease`——这些都是 primitive write。文件顶部 `#![allow(dead_code)]`。
- 已在 `docs/reports/v5.2-live-write-audit.md` 记为“side-index debt”，属有意识延后，但仍是 gate 偏离。

### 🟡 G7 — commands.rs 的 append-only registry 测试是 `#[cfg(test)]` 重实现
- `check_or_record_idempotency` / `commands_index_path` / `append_command_registry_line` /
  `rebuild_commands_index_from_log` 全部 `#[cfg(test)]`（`commands.rs:204+`）。
- 即 `commands_index_rebuilds_from_append_only_log` 测的是**测试专用副本**，不是生产路径。
  生产的 idempotency/重建走 `state.store`（真实，由 `json_store_rebuilds_corrupt_command_index_from_logs` 覆盖）。
  → gate 实质被 store_json 覆盖，但 commands.rs 里这段是带测试的死代码，易误读为“生产已覆盖”。

---

## 验证层面的不一致

| 项目 | 自评报告声称 | 本次实测 | 判定 |
|---|---|---|---|
| cargo daemon/mcp | 100 + 7 passed | 100 + 7 passed | ✅ 一致 |
| arch audit | passed | passed | ✅ 一致 |
| pytest | 17 passed | 3 passed / 14 errors（Windows 文件锁 teardown） | ⚠️ 干净结果未复现 |
| plan 列出的测试名 | 多处称 “Covered” | 抽查 20 个 plan 测试名**逐字 0 命中** | ⚠️ 改名覆盖，部分无等价 |

plan 测试名“改名覆盖”大多没问题（如 `interrupt_sent_does_not_mark_command_completed`、
`same_workspace_different_path_spelling_conflicts` 等确有等价实现）；但 G1/G2/G3 那
8 个安全相关测试**既无原名也无任何等价**，对应功能确实缺失。

---

## 建议（按优先级，均属 plan 内既定要求，非新增范围）

1. **G1 闭合**：在 `actor.rs::execute_command` 派发前（或 `commands.rs::prepare_*` 中）
   读取当前 projection 的 `projection_last_session_seq` 与 `precondition` 比较，不匹配返回
   `rejected_precondition`；补 `session_send_precondition_seq_mismatch_rejected`。
2. **G2 闭合**：`session_actor_loop` 用 catch/守卫，actor 退出 / writer 写失败时写
   `failed_or_orphaned` 投影 + 事件；daemon 启动对无法确认 ownership 的 running session 标 orphaned；补对应 3 个测试。
3. **G3**：至少把 `RawWrite` 移出 `ActorControlCommand`（独立小通道），并加 stop/interrupt
   相对 queued submit 的优先处理；补 `stop_command_has_priority_over_output_chunks`。
4. G5/G6/G7 可作为 v5.2 后续 hardening；但应在自评报告中显式标红，不要计入“core gates complete”。
5. 修正自评报告：增列 G1（完全遗漏）、把 pytest 结果改为环境相关说明。

## 一句话

实现是认真的、可跑的，但 plan 三版各自最硬的安全闸门（防陈旧写入、失败态可见性、
控制/输出隔离）尚未真正关上——这正是 plan 反复警告“不允许靠措辞糊过去”的部分。
当前应被视为 **integration milestone**，而非 v5 sign-off。
