# AgentCall 协议与工具概览报告

**任务**: 在无上下文注入（Context Injection: none）条件下，阅读 `AGENTS.md`、`README.md` 与 `docs/agentcall-protocol.md`，总结 AgentCall 的规范 MCP 工具、worker 类型、核心流程与关键边界/禁令。

**结论**: 本任务为纯静态文档阅读与总结，无注入上下文并未造成明显延迟或理解障碍；三份文档已完整覆盖所需信息。

---

## 1. 规范 MCP 工具（Canonical MCP Tools）

仅默认使用以下 5 个 AgentCall MCP 工具（来源：`docs/agentcall-protocol.md` 第 9–19 行）：

| 工具 | 用途 |
|---|---|
| `agentcall_daemon` | 启动/检查 daemon 状态（`action=start|status`） |
| `agentcall_board` | 查看 compact board 投影，过滤 attention 状态 |
| `agentcall_route` | 创建任务路由，指定目标 workspace、write_paths、reference_paths |
| `agentcall_session` | 查看指定 session 的投影与 primary_action |
| `agentcall_session_send` | 向 worker 发送动作，如 `request_report`、`select_option` |
| `agentcall_report` | 请求/验收报告（`action=request|accept`） |

**注意**: 文档明确禁止调用已废弃的 delegate/workflow 工具（`docs/agentcall-protocol.md:21`）。`README.md:189–196` 与 `AGENTS.md:79–88` 也列出了同样的推荐流程。

---

## 2. 正常 Worker 类型（Normal Worker Kinds）

AgentCall 仅保留两类正常 worker（`AGENTS.md:92–96`、`README.md:200–205`）：

1. **`coding`**
   - 必须传入实现级 `write_paths`。
   - worker 获得**独占的 target workspace lease**。
   - 只能写入 `write_paths` 对应的实现路径，以及 scratch/report 空间。

2. **`report`**
   - 不传 `write_paths`，或仅将其限制在报告范围内。
   - worker 获得**共享的报告 workspace lease**。
   - 只能写入 scratch/report 产物，不得修改实现文件。

**重要变化**: `read_only` 已不再是合法路由参数。需要产出报告的任务属于 `report` worker，而非只读 worker。

---

## 3. 核心使用流程（Core Usage Flow）

推荐的标准流程如下（来源：`docs/agentcall-protocol.md:24–32`）：

```text
1. agentcall_daemon(action=start)
2. agentcall_board(view=compact, filter=attention)
3. agentcall_route(objective=..., workspace=..., write_paths=..., reference_paths=...)
4. agentcall_session(name=...)
5. 按 primary_action 执行正常路径
6. agentcall_session_send(action=request_report)  // 仅在需要结束 worker 时
7. agentcall_report(action=accept, session_id=...)
```

关键要点：
- `agentcall_route` 默认使用 daemon 拥有的 Claude Code PTY worker。
- `runtime`、`mode`、SDK/ACP、lease id、precondition、idempotency key 均为调试/兼容内部字段，不应出现在正常 Codex 循环中。
- `report_path` 可省略；省略时 daemon 会在目标 workspace 下生成唯一报告路径（`README.md:207–211`）。
- 通过 `agentcall_daemon(action=status)` 检查可用性，`tool_search agentcall` 可能不可靠（`AGENTS.md:99`、`README.md:223`）。

---

## 4. 关键边界与禁令（Key Boundaries / Prohibitions）

### 4.1 写入与路径边界
- 不得写入分配的 `write_paths`、scratch、`report_path` 之外的任何位置（`AGENTS.md:104`）。
- `write_paths` 是写权限边界；`reference_paths` 只是阅读建议，不由 daemon 强制只读（`README.md:215`）。
- 本地配置 `claude_workspace` 决定 Claude Code 进程 cwd，而 route 的 `workspace` 只是任务目标目录，二者不互相覆盖（`AGENTS.md:54–60`、`README.md:116–125`）。

### 4.2 状态源与行为禁令
- **禁止**将原始终端/转录输出作为默认状态源；应优先读取 compact projection（`docs/agentcall-protocol.md:21`、`AGENTS.md:105`）。
- **禁止**重复已被策略拒绝的动作；重复视为 `blocked_by_policy`（`AGENTS.md:108`）。
- **禁止**把 `submit_pending_prompt` 当作正常完成路径；它仅是 debug/recovery 信号（`docs/agentcall-protocol.md:43`、`README.md:217`、`AGENTS.md:157`）。
- **禁止**在 patience window 期间重复发送提示（`docs/agentcall-protocol.md:100`）。
- **禁止**将 Python 用作实时状态写入器（`AGENTS.md:116`、`docs/agentcall-protocol.md:102`）。
- **禁止**自动 kill 可见的 PTY worker（`docs/agentcall-protocol.md:103`）。

### 4.3 权限与菜单规则
- 权限菜单是结构化交互，必须使用 `agentcall_session_send(action=select_option, choice="1|2|3")`，不得发送自然语言（`docs/agentcall-protocol.md:68–70`）。

### 4.4 报告验收规则
- `overall=high`: 报告产物存在，且 daemon 观察到确定性写证据。
- `overall=medium`: 报告产物存在，但确定性证据不完整。
- `overall=low`: 仅自然语言报告、证据缺失、策略阻塞、权限拒绝等（`docs/agentcall-protocol.md:80–91`、`README.md:221`）。

---

## 5. 架构速览

AgentCall 的调用链为（`README.md:30–37`）：

```text
Codex
  -> AgentCall MCP bridge (stdio)
  -> Rust daemon HTTP API
  -> SessionActor / PTY runtime
  -> Claude Code worker in configured claude_workspace
  -> Claude/Codex hooks POST /api/hooks/ingest
  -> projections, board, claims, reports, runtime health
```

核心 crate：
- `crates/agentcall-daemon`: daemon、HTTP API、PTY runtime、hooks、routes、projections、ownership。
- `crates/agentcall-mcp`: MCP stdio bridge 与工具协议。
- `crates/agentcall-hook`: hook helper crate。

---

## 6. 已读文件（files_read）

- `E:\Project\AgentCall\AGENTS.md`
- `E:\Project\AgentCall\README.md`
- `E:\Project\AgentCall\docs\agentcall-protocol.md`

---

## 7. 不确定点（Uncertainties）

1. 当前 daemon 是否正在运行：文档描述了启动方式（`target\debug\agentcall-daemon.exe --workspace E:\Project\AgentCall`），但本任务为静态阅读，未实际调用 `agentcall_daemon(action=status)` 验证。
2. `agentcall_board`、`agentcall_route` 等工具的具体 JSON schema 未在三份文档中展开；文档仅描述语义与推荐用法，详细字段定义可能在 MCP bridge schema 或源码中。
3. `crates/agentcall-mcp` 中是否还有其他非默认/调试工具：本报告仅覆盖文档声明的 canonical tools。

---

## 8. 无注入上下文的影响评估

本任务要求总结的是 AgentCall 自身文档中已明确写出的协议、工具与边界。`AGENTS.md`、`README.md` 和 `docs/agentcall-protocol.md` 三份文件内容一致且互补，足以完成任务。**缺少额外上下文注入并未减慢或复杂化任务**，因为：
- 所需信息全部来自指定参考文件；
- 文档本身对 canonical tools、worker kinds、core flow、prohibitions 有重复且一致的描述；
- 唯一可能缺失的是实时 daemon 状态，但这不属于报告要求的内容。

---

**报告完成时间**: 2026-06-14  
**任务 ID**: ctx-metric-simple-a-raw
