# AgentCall v6.7.2 Owner-Scoped Concurrency 代码审查报告

**审查目标**：AgentCall main 分支 v6.7.2 owner-scoped concurrency 修复、MCP bridge owner 传递、scheduler 阻断维度、版本对齐及健壮性/安全问题。  
**审查方式**：静态代码与文档审查，未修改源码。  
**报告日期**：2026-06-14  
**报告人**：Claude Code report worker

---

## 1. 结论

v6.7.2 在 **scheduler 容量判定** 和 **MCP bridge owner_id 派生/透传** 两条主链路上实现了按 owner 隔离的设计意图：

- `crates/agentcall-mcp/src/config.rs` 正确从 `AGENTCALL_OWNER_ID` 或 `CODEX_THREAD_ID` 派生 `owner_id`，并随每个 `/api/mcp/call` 请求透传给 daemon。
- `crates/agentcall-daemon/src/scheduler.rs` 的 `enforce_start_capacity` / `scheduler_decision` 仅以 `active_owner_sessions >= per_owner_max_sessions` 为硬闸门，`max_sessions` 不再作为全局硬上限。
- owner lease 在 `routes.rs` 的 `start_pty_route` 中按传入的 `owner_id` 创建，容量和租约的“入口”是对的。

**但是**，在 **PTY 启动后的首次 prompt 提交**、**prompt gate 自动自愈**、**compact board 过滤** 和 **scheduler health 报告** 这几条关键路径上，代码仍然把 owner 当成 `"codex"` 硬编码，导致非默认 owner 的实际 worker 启动失败或监控失真。这是 v6.7.2 当前最显著的残余风险。

版本对齐（Cargo.toml / pyproject.toml / plugin.json / SERVER_VERSION / README / CHANGELOG / Cargo.lock）均保持 `6.7.2`，无版本漂移。

---

## 2. 按严重度排序的问题

### P0 — 非默认 owner 的 PTY worker 启动会因 owner lease 冲突失败

**问题描述**  
`routes.rs` 在创建 owner lease 时使用了正确的 `owner_id`，但在把首条 prompt 命令交给 session actor 时，把 `owner_id` 硬编码为 `"codex"`。`prepare_session_send_command` 会调用 `attach_or_validate_owner_lease`，该函数发现 session 已存在属于其他 owner 的 active lease 时会直接返回 `OwnerConflict` 错误。因此，只要 MCP bridge 派生出的 `owner_id` 不是 `"codex"`，PTY 路由就无法真正启动。

**证据**  
- `crates/agentcall-daemon/src/routes.rs:1079`
  ```rust
  let args = json!({
      "text": prompt,
      "enter": true,
      "idempotency_key": idempotency_key.clone(),
      "owner_id": "codex"
  });
  ```
- `crates/agentcall-daemon/src/routes.rs:1186`
  ```rust
  let args = json!({
      "text": prompt,
      "enter": true,
      "idempotency_key": idempotency_key,
      "owner_id": "codex"
  });
  ```
- `crates/agentcall-daemon/src/commands.rs:100`：`prepare_session_send_command` → `attach_or_validate_owner_lease`
- `crates/agentcall-daemon/src/ownership.rs:206-224`：`ensure_owner_lease` 在 `existing.owner_id != owner_id` 时返回 `OwnerConflict`。

**影响**  
- 只要 `AGENTCALL_OWNER_ID` / `CODEX_THREAD_ID` 派生出的 owner 不是 `"codex"`（例如 `codex-thread-xxx`），`agentcall_route(mode=start)` 会创建 route/lease，但在提交 prompt 时失败。
- v6.7.2 按 owner 隔离额度的目标在真实多线程场景下不可用。

**建议修复**  
把 `submit_pty_prompt_with_ack` / `submit_pty_prompt_without_hook_ack` 增加 `owner_id: &str` 参数，调用处传入 `start_pty_route` 收到的 `owner_id`，替换 `"codex"` 硬编码。

---

### P1 — prompt gate 自动自愈对非默认 owner 失效

**问题描述**  
daemon 的 prompt gate 自动提交（`prompt_missing` / `prompt_pending_ack` 超时后自动发空格）同样把 `owner_id` 硬编码为 `"codex"`。若 session 属于其他 owner，自动提交会触发 owner lease 冲突，导致 daemon 无法自愈 stale prompt。

**证据**  
- `crates/agentcall-daemon/src/prompt_gate.rs:191-194`
  ```rust
  let args = json!({
      "idempotency_key": attempt_id,
      "owner_id": "codex"
  });
  ```
- 该函数由 `refresh_prompt_gate_timeouts_for_session` 调用，用于 daemon 自动提交 stale prompt。

**影响**  
- 非默认 owner 的 worker 卡在 `prompt_missing` / `prompt_pending_ack` 时，daemon 自动恢复机制失效，Codex 需要手动 `submit_pending_prompt`。

**建议修复**  
在 `prompt_gate.rs` 的自动提交路径中，从 session/route 读取实际 `owner_id` 并传入命令。可在 `OwnerLease` 中通过 `session_id` 反查。

---

### P1 — compact board 和 attention 视图未按 owner 过滤

**问题描述**  
`agentcall_board(view=compact)` 接受 `scope` / `owner_id` 参数，但 `v6_compact_board_state` 把 owner_id 参数命名为 `_owner_id` 且从未使用。因此无论调用方 owner 是谁，compact board 始终返回所有 worker。

**证据**  
- `crates/agentcall-daemon/src/summary.rs:185-189`
  ```rust
  fn v6_compact_board_state(
      state: &AppState,
      filter: Option<&str>,
      _owner_id: Option<&str>,
      workspace_filter: Option<&str>,
  ) -> serde_json::Value {
  ```
- `crates/agentcall-daemon/src/summary.rs:30-38`：`board_owner_filter` 对 `scope=mine` 返回 `"codex"`，且 compact 视图完全未消费该结果。

**影响**  
- 多 owner 场景下，Codex 看到的 compact board 会混入其他 owner 的 worker，干扰监督决策。
- `scope=mine` 语义失效。

**建议修复**  
在 `v6_compact_board_state` 中按 `owner_id` 过滤 worker；worker 状态对象需包含 owner 字段（route/lease 中已有），或从 `owner_leases` 反查。

---

### P1 — runtime health 的 scheduler 字段永远报告 codex 的 owner 会话数

**问题描述**  
`runtime_health` 调用 `scheduler_health(state)` 时未传入 owner，而 `scheduler_health` 内部硬编码用 `"codex"` 调用 `scheduler_decision`。因此 `/api/runtime/health` 中的 `codex_active_sessions` 对任何 caller 都是 codex 的数据， misleading。

**证据**  
- `crates/agentcall-daemon/src/scheduler.rs:97-108`
  ```rust
  pub(crate) fn scheduler_health(state: &AppState) -> Value {
      let decision = scheduler_decision(state, "codex");
      ...
      "codex_active_sessions": decision.active_owner_sessions,
  }
  ```
- `crates/agentcall-daemon/src/summary.rs:342`：`"scheduler": scheduler_health(state)`。

**影响**  
- 多 owner 监控时，health 报告不能反映当前 caller 的真实容量使用情况。

**建议修复**  
`scheduler_health` 增加 `owner_id: &str` 参数；`runtime_health` 如能从请求上下文获得 owner 则传入，否则保留 `"codex"` 作为默认并显式标注为 default owner。

---

### P2 — owner_id 归一化可能产生碰撞

**问题描述**  
`normalize_owner_id` 把非安全字符替换为 `-`，不同原始 ID 可能归一化后相同。例如 `owner@a` 和 `owner#a` 都变成 `owner-a`，理论上可导致不同 caller 共享同一 quota。

**证据**  
- `crates/agentcall-daemon/src/mcp.rs:214-227`
- `crates/agentcall-mcp/src/config.rs:86-99`

**影响**  
- 低：owner 源为宿主环境变量，受信任；但归一化碰撞削弱了隔离保证的严谨性。

**建议修复**  
可选：在 owner_id 归一化时保留原始值的 SHA-256 摘要作为后缀，或在环境变量缺失时生成更稳定的唯一标识；至少应在日志中记录原始 owner_id。

---

### P2 — `/api/sessions` 原始启动路径始终使用 codex owner

**问题描述**  
直接调用 daemon HTTP `/api/sessions` 启动 session 时，`session.rs:154` 固定使用 `"codex"` 创建 owner lease。该路径不是 Codex 推荐路径，但与 owner-scoped 模型不一致。

**证据**  
- `crates/agentcall-daemon/src/session.rs:154`
  ```rust
  let owner_lease = ensure_owner_lease(state, &session.name, "codex")?;
  ```

**影响**  
- 仅影响绕过 MCP/route 的原始 HTTP 调用者；正常使用无影响。

**建议修复**  
如希望保留该 API，可在 `StartRequest` 中可选接受 `owner_id`，默认 `"codex"`。

---

## 3. 版本对齐检查结果

| 文件 | 版本 | 状态 |
|---|---|---|
| `crates/agentcall-daemon/Cargo.toml` | 6.7.2 | 一致 |
| `crates/agentcall-mcp/Cargo.toml` | 6.7.2 | 一致 |
| `crates/agentcall-hook/Cargo.toml` | 6.7.2 | 一致 |
| `Cargo.lock` | 6.7.2 | 一致 |
| `crates/agentcall-mcp/src/protocol.rs` (`SERVER_VERSION`) | 6.7.2 | 一致 |
| `pyproject.toml` | 6.7.2 | 一致 |
| `plugins/agentcall/.codex-plugin/plugin.json` | 6.7.2 | 一致 |
| `README.md` | 6.7.2 | 一致 |
| `CHANGELOG.md` | 6.7.2 | 一致 |
| `AGENTS.md` | 6.7.2 | 一致 |
| `docs/README.md` | 6.7.2 | 一致 |

**结论**：源码与文档版本已对齐，无漂移。

---

## 4. 安全与健壮性评估

- **容量隔离**：scheduler 本身正确按 owner 计数，无全局六并发硬阻塞问题；但 P0/P1 硬编码使该隔离在首条 prompt 和自愈路径上失效。
- **敏感信息**：MCP 参数 redaction（`redact_mcp_arguments`）覆盖 objective/text/prompt/content/tool_input/command/control_token 及超长字符串，保持 v6.7 加固水平。
- **错误码**：`capacity_exceeded` / `owner_lease_exists` / `workspace_busy` 均通过 `ErrorCode` 枚举输出结构化错误，无新增裸字符串错误码。
- **租约过期/孤儿清理**：`prune_expired_leases` 和 `release_orphaned_runtime_leases` 逻辑完整，能正确释放过期/孤儿 owner lease 和 workspace lease。
- **共享 report lease**：`route_uses_shared_workspace_lease` 对 report worker 使用 `SharedReport` 模式，允许多个 report worker 共存同一 workspace，实现正确。

---

## 5. 建议修复优先级与验证建议

### 立即修复（P0）

1. **`routes.rs` 首条 prompt 使用 route owner_id**
   - 修改 `submit_pty_prompt_with_ack` / `submit_pty_prompt_without_hook_ack` 签名，接收 `owner_id: &str`。
   - 在 `start_pty_route` 调用处传入 `owner_id`。
   - 替换 `"owner_id": "codex"` 为实际值。

### 尽快修复（P1）

2. **`prompt_gate.rs` 自动提交使用 session owner_id**
   - 在 `auto_submit_prompt` 中从 `owner_leases` 或 route record 读取实际 owner。
3. **`summary.rs` compact board 按 owner 过滤**
   - 在 `v6_compact_board_state` 中消费 `_owner_id`，过滤 `all_workers`。
   - 确保 worker board 对象包含 `owner` 字段。
4. **`scheduler.rs` health 支持 per-owner 报告**
   - `scheduler_health(state, owner_id)`，`runtime_health` 尽量传入 caller owner，否则标注 default。

### 可选加固（P2）

5. 评估 owner_id 归一化碰撞风险，必要时加入原始值摘要或长度限制校验。
6. 为 `/api/sessions` 原始启动路径增加可选 `owner_id` 字段。

### 验证建议

- **单元测试**
  - 在 `scheduler.rs` 已有测试基础上，增加多 owner 容量隔离测试：owner A 满额时不影响 owner B 启动。
  - 增加 `mcp_route` 非 codex owner 的 PTY 启动测试，断言 route record 的 `owner_id` 与 prompt 命令的 `owner_id` 一致，且 worker 成功进入 `prompt_pending_ack`。
- **集成测试**
  - 设置 `CODEX_THREAD_ID=thread-a` 和 `CODEX_THREAD_ID=thread-b` 分别启动两个 MCP bridge，验证双方各能启动最多 6 个 worker，且互不影响。
  - 验证 `agentcall_board(view=compact, scope=mine)` 只返回当前 owner 的 worker。
  - 验证 `agentcall_daemon(action=status)` 的 `scheduler.codex_active_sessions` 在多 owner 下能反映对应 owner（如无法传入 owner，至少验证不再恒为 codex 数据）。
- **回归测试**
  - `cargo test --workspace`
  - `python -m pytest -q`
  - `python agentcall.py runtime-release --version 6.7.2 --dry-run` 校验版本对齐

---

## 6. 残余风险总结

- **真实多 Codex 线程并发场景下，v6.7.2 目前无法真正按 owner 启动 worker**（P0）。
- prompt gate 自动自愈在 multi-owner 场景下失效，Codex 可能频繁遇到 stale prompt（P1）。
- compact board 会暴露其他 owner 的 worker，增加监督噪音（P1）。
- 版本对齐完整，无新增安全漏洞，但 owner 隔离的“最后一公里”未打通。
