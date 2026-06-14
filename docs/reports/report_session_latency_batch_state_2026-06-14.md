# AgentCall Session Latency / Batch State 报告

日期：2026-06-14  
背景：Codex UI 中连续 `agentcall_session` 调用体感约 15 秒；用户希望确认是否能一次性请求全部状态，并将体感压到 5 秒以内。

## 结论

可以做，而且应该做。

当前慢的主要问题不是 daemon 单次 Rust 处理慢，而是 Codex UI 连续发起多次 MCP tool call，每次都走完整链路：

```text
Codex UI
  -> MCP stdio transport
  -> agentcall-mcp.exe
  -> daemon HTTP /api/mcp/call
  -> session summary / worker state
  -> MCP response
  -> Codex UI render
```

如果 UI 里连续出现 3 次 `Agentcall session`，就是 3 次完整往返。即使 daemon 单次只有几百毫秒，Codex host / MCP transport / tool rendering 的固定开销也会被乘以 3。

因此优化方向不是“让 Codex 并发调用 3 个 session”，而是让 Codex 调一次：

```text
agentcall_board(view=compact, filter=all, section=sessions)
```

或等价的 daemon batch projection，一次返回所有 live worker 的 compact state。

## 实测证据

在本机直接绕过 Codex UI，调用 daemon HTTP：

```powershell
POST http://127.0.0.1:3293/api/mcp/call
name=agentcall_board
arguments={ view=compact, filter=all, scope=all }
```

耗时约：

```text
45ms
```

之前直接调用单个 `agentcall_session(summary)` 约：

```text
218ms
```

进一步做三层对照实验：

| 层级 | 调用 | 观测耗时 | 说明 |
| --- | --- | ---: | --- |
| daemon HTTP 直连 | `agentcall_board(compact/all)` | 第一次 `46.88ms`，后续 `2.35-3.80ms` | 直接打 `/api/mcp/call`，绕过 MCP stdio 和 Codex UI |
| standalone MCP stdio | `agentcall_board(compact/all)` | 平均 `2.48ms`，最大 `3.64ms` | 手动启动 `target/debug/agentcall-mcp.exe`，用 JSON-RPC `tools/call` 走 stdio |
| Codex 内置 MCP tool | `agentcall_board(compact/all)` | `6.1ms` | 当前 Codex tool 调用，响应很小 |
| Codex 内置 MCP tool | `agentcall_session(route-pty-1803903)` | `12.99s` | 用户体感慢的同类调用 |
| agentcall-mcp timing log | 同一次 `agentcall_session(route-pty-1803903)` | `call_ms=171ms`, `render_ms=0`, `original_bytes=2507` | MCP server 已快速完成，慢发生在 server handler 外层 |

最后一条是关键证据：同一次 Codex tool call，外层 wall time 接近 13 秒，但 `agentcall-mcp` 在收到请求后的实际处理只有 171ms。因此这不是 Rust daemon 或 agentcall-mcp handler 慢，而是 Codex host 管理 MCP tool call 的调度/transport/UI 层慢，或者请求在进入 MCP server 前排队。

这说明 daemon 底层处理不是 15 秒级。15 秒体感更像：

- 多次 MCP tool call 串行；
- Codex UI 等待/渲染每个 tool call；
- Codex host 在请求进入 MCP server 前有排队/调度延迟；
- `agentcall_session` 默认路径职责过重；
- response 里字段偏多，导致 UI 展开和 token 消耗变重。

## 对“是不是 MCP 慢”的判断

如果把“MCP 慢”定义为 `agentcall-mcp.exe` 处理慢，答案是否定的：standalone stdio 和 timing log 都显示毫秒到百毫秒级。

如果把“MCP 慢”定义为 Codex App 中一次 MCP tool call 的端到端体验慢，答案是肯定的：同一次 `agentcall_session` 外层 wall time 可以到 13 秒，而 server 内部只花 171ms。

所以精确表述应为：

```text
慢点在 Codex-hosted MCP tool-call envelope，而不是 AgentCall daemon / agentcall-mcp server handler。
```

这也是为什么 batch state 有价值：我们无法完全控制 Codex host 的 per-tool-call 固定开销，但可以把 N 次 tool call 减成 1 次。

## 当前代码路径问题

### 1. `agentcall_session(summary)` 不是纯读

证据：

- `crates/agentcall-daemon/src/mcp.rs`
  - `mcp_session`
  - `session_summary_view`
- `crates/agentcall-daemon/src/worker_state.rs`
  - `worker_state_for_session`
- `crates/agentcall-daemon/src/prompt_gate.rs`
  - `refresh_prompt_gate_timeouts_for_session`

当前 summary 会调用：

```rust
worker_state_for_session(state, name)
```

而 worker state 内部会调用：

```rust
refresh_prompt_gate_timeouts_for_session(state, session_name)
```

这意味着一次普通 summary 查询可能会：

- 读 projection；
- 查 route；
- 刷 prompt gate；
- 可能 patch route；
- 生成 control token；
- 计算 patience / report / workspace；
- 检查 prompt 自动提交状态。

这不是“读状态”，而是“读状态 + 推进状态机”。

### 2. `board(compact)` 已经像 batch，但内部仍逐 session 重算

证据：

- `crates/agentcall-daemon/src/summary.rs`
  - `v6_compact_board_state`

当前逻辑：

```rust
let all_workers = runtime_sessions
    .iter()
    .filter(|session| session.status == "running")
    .map(|session| worker_state_for_session(state, &session.name).to_board_worker())
    .collect();
```

这已经是“一次请求返回多个 worker”，但每个 worker 仍会调用重型 `worker_state_for_session`，包括 prompt gate refresh。

## 设计建议

### P0：新增纯读 `worker_snapshot_for_session`

拆分：

```text
worker_state_for_session          = 可推进状态机，供控制动作/显式刷新使用
worker_snapshot_for_session       = 纯读 projection，供 board/session 默认查询使用
```

默认 MCP 查询必须走纯读：

```text
agentcall_board(view=compact, filter=all)
agentcall_session(view=summary)
```

只有这些动作允许推进状态机：

```text
agentcall_session_send(...)
agentcall_report(...)
agentcall_daemon health maintenance
显式 include/debug refresh
```

### P0：把 Codex 默认路径改成 batch board

Codex 不应该连续调用：

```text
agentcall_session(a)
agentcall_session(b)
agentcall_session(c)
```

而应该调用一次：

```text
agentcall_board(view=compact, filter=all, scope=mine)
```

返回字段控制在小集合：

```json
{
  "workers": [
    {
      "name": "...",
      "state": "...",
      "why": "...",
      "can_wait": true,
      "primary_action": {"kind": "wait"},
      "report": {"ready": false, "status": "..."},
      "attention": null
    }
  ],
  "counts": {...}
}
```

不要默认返回：

- control token；
- full prompt gate；
- route result 大字段；
- clean tail；
- raw terminal；
- events；
- tool output。

### P1：控制 token 懒生成

当前 `session_summary_view` 默认会生成 control token：

```rust
let control = slim_control_summary(control_summary_for_session(state, name, None));
```

建议默认只返回：

```json
"control": {
  "available": true,
  "required_for": ["stop", "interrupt", "kill"]
}
```

只有当调用方明确需要 phase-changing/destructive action 时，再请求带 token 的 control view。

### P1：session summary 增加 batch mode 或由 board 承担

不建议新增一堆 MCP 工具。可选方案：

#### 方案 A：扩展 `agentcall_board`

保持工具面不变：

```json
{
  "view": "compact",
  "filter": "all",
  "section": "sessions",
  "scope": "mine"
}
```

让这个返回所有 session compact summaries。

优点：

- 不新增工具；
- 符合 projection-first；
- Codex 容易学。

#### 方案 B：允许 `agentcall_session(name="*")`

不推荐。`session` 语义本来是单 worker，`*` 会污染接口直觉。

#### 方案 C：新增 `agentcall_sessions`

功能清晰，但违反“工具面收敛”的长期方向。

推荐方案 A。

## 5 秒目标可行性

如果当前 15 秒主要来自 3 次 sequential MCP calls，那么改成一次 batch call 后，理论上可直接降低到 1 次 MCP overhead。

目标：

```text
Codex 体感：<= 5s
daemon HTTP：<= 300ms for <= 6 live workers
MCP response size：<= 8KB default
```

为了达到这个目标，需要同时满足：

1. 默认查询只走 pure projection snapshot；
2. 一次 board 返回所有 worker；
3. response 不带 raw/events/control token；
4. board 不执行 prompt gate mutation；
5. Codex supervisor skill / AGENTS.md 明确：多 worker 状态检查优先 `board`，不要逐个 `session`。

## 风险

### 风险 1：纯读 snapshot 可能不触发 prompt gate 自愈

这是正确取舍。状态查询不应有副作用。prompt gate 自愈应该由：

- route start 后的后台维护；
- daemon periodic maintenance；
- 明确的 control action；
- health maintenance；

来完成，而不是靠用户每次查询 session 顺手推进。

### 风险 2：board 信息太少，Codex 还是会补查 session

需要把 board 的 compact worker 字段设计得够用：

- `state`
- `why`
- `can_wait`
- `primary_action`
- `report.ready/status/path`
- `pending_interaction.kind`
- `attention_status`
- `last_progress_age_seconds`

这样 Codex 只有在：

- `needs_permission`
- `blocked_by_policy`
- `report_ready`
- `control_needed`
- `debug_requested`

时才查单 session。

### 风险 3：batch state 可能放大 session 所有权串线

这是 batch 方案最危险的地方。

如果 batch board 一次返回所有 worker，而没有严格按 caller owner 过滤，会出现两类串线风险：

#### 读串线

当前 Codex session 看到其他 Codex session / 其他项目 worker 的：

- session name；
- target workspace；
- report path；
- state / why；
- pending interaction；
- permission/blocker 信息。

这不一定能直接控制对方 worker，但会污染主管判断。Codex 可能误以为“这些 worker 都是我的”，然后尝试 stop / accept / request_report。

现有代码中已经有迹象：

- `board(scope=mine)` 的 owner 语义目前不够可靠；
- `v6_compact_board_state(..., _owner_id, ...)` 参数被命名为 `_owner_id`，说明 compact board 可能没有真正消费 owner filter；
- 这和 v6.7.2 owner-scoped capacity 是同一个问题域。

#### 写 / 控制串线

这比读串线更严重。危险操作包括：

- `session_send(stop|interrupt|kill|approve_plan|start_auto)`；
- `session_send(send|continue|request_report)`；
- `agentcall_report(accept)`；
- prompt gate auto-submit；
- report acceptance / route patch。

理论上当前 daemon 有几层保护：

- owner lease；
- control token；
- session id；
- lease generation；
- route/report match；
- daemon-observed write evidence。

但当前审查报告已经指出 v6.7.2 仍有硬编码风险：

- `routes.rs` 首条 prompt 提交仍可能硬编码 `"owner_id": "codex"`；
- `prompt_gate.rs` auto-submit 也可能硬编码 `"owner_id": "codex"`；
- `control_summary_for_session(state, name, None)` 默认 owner 也是 `"codex"`。

这些问题在单 owner 场景里不明显，在多 Codex session / batch board 场景里会变成串线或 owner mismatch。

### 所有权安全原则

batch state 只能做成 **owner-scoped read batch**，不能做成“全局 session 列表 + 控制入口”。

建议原则：

1. MCP bridge 必须给每个请求附带 caller owner：
   - `AGENTCALL_OWNER_ID` 优先；
   - `CODEX_THREAD_ID` 次之；
   - 无 owner 时不应默默降级为可控制的 `codex`，至少要标记 `owner_unbound`。
2. `agentcall_board(scope=mine)` 默认只返回当前 owner 的 worker。
3. `agentcall_board(scope=all)` 只能返回只读、无 control token、无 destructive action 的 debug projection。
4. batch 返回中不得包含可复用 control token。
5. control token 必须按 session + owner + lease_generation mint，且只在单 session control view 里按需返回。
6. `session_send` / `report accept` 必须继续验证 owner lease，不得因为 batch 列表里出现 session name 就允许控制。
7. prompt gate auto-submit 必须使用 session 实际 owner，不得硬编码 `codex`。
8. report accept 必须校验 route owner / report path / daemon-observed write，避免 A owner 接受 B owner 的报告。
9. session name 不能作为所有权凭证；它只是索引，不是 authority。
10. batch projection 应带 `owner_visible: true/false` 或 `scope: mine/all` 元数据，方便 Codex 判断它能不能操作这些 worker。

### 推荐安全形态

默认路径：

```text
agentcall_board(view=compact, filter=all, scope=mine)
```

返回当前 owner 的所有 worker，但不返回 control token。

控制路径：

```text
agentcall_session(name=..., include=["summary", "control"])
```

只对单 session 返回短期 control token。

debug 路径：

```text
agentcall_board(view=compact, filter=all, scope=all)
```

只用于人类/调试，返回其他 owner worker 时必须降级为只读字段，不能带 control action/token。

### 判定

batch 性能优化可以做，但必须先补所有权边界：

| 项目 | 没补前风险 | 是否阻塞 batch |
| --- | --- | --- |
| compact board owner filter | 当前 Codex 看到其他 worker | 阻塞默认 batch |
| route prompt owner 硬编码 | 非默认 owner 启动/提交冲突 | 阻塞多 owner 正常使用 |
| prompt gate owner 硬编码 | auto-submit 串线或失败 | 阻塞自动恢复 |
| control token 懒生成 | token 被 batch 暴露 | 阻塞 batch control 字段 |
| `scope=all` 降级只读 | 调试视图误操作 | 不阻塞 mine，但阻塞 all 默认化 |

## 建议实施版本

建议作为 v6.7.3 或 v6.8 的 P0 性能修复：

1. 先修 owner-scoped control 闭口：
   - route prompt 使用实际 owner；
   - prompt gate auto-submit 使用实际 owner；
   - compact board 正确过滤 owner；
   - scope=all 降级只读。
2. 拆 `worker_snapshot_for_session` 纯读路径。
3. `v6_compact_board_state` 改用 snapshot，不调用 `worker_state_for_session`。
4. `agentcall_session(summary)` 默认也走 snapshot；需要 control token 时显式 include/control。
5. AGENTS / supervisor skill 更新：多 worker 检查只用 board batch。
6. 增加性能和所有权测试：
   - 6 live worker compact board <= 300ms daemon-side；
   - response <= 8KB；
   - board 调用不 patch route，不新增事件；
   - session summary default 不 mint token。
   - owner A 的 board(scope=mine) 不返回 owner B worker；
   - owner A 无法 stop/send/accept owner B session；
   - scope=all 不返回 control token。

## 给 GPT Pro 的问题

1. 是否同意“状态查询必须无副作用”的拆分？
2. `control token` 是否应该从默认 summary 移除，改为按需获取？
3. batch state 应该完全放在 `board`，还是单独提供 `sessions` endpoint？
4. prompt gate 自愈应该由后台维护驱动，还是允许 summary query 顺手推进？
5. 5 秒 Codex UI 体感目标下，默认 MCP response size 应该限制在多少？8KB 是否合理？
6. batch projection 是否必须强制 `scope=mine`，把 `scope=all` 保留为 debug-only？
7. 无 `CODEX_THREAD_ID` / `AGENTCALL_OWNER_ID` 时，是否应拒绝控制动作而不是 fallback 到 `codex`？
