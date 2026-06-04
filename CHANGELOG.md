# CHANGELOG

## v0.8.1 - Stable MCP Bridge

- MCP stdio 进程改为稳定薄外壳：本地只保留 `agentcall_daemon` bootstrap。
- 业务工具列表由 daemon `GET /api/mcp/tools` 提供，业务调用由 daemon `POST /api/mcp/call` 执行。
- `agentcall-mcp` 不再本地实现 route/session/report/board 业务逻辑，避免每次工具面更新都重建并杀死 MCP transport。
- daemon 新增 MCP bridge 模块，canonical 工具仍保持：
  - `agentcall_board`
  - `agentcall_route`
  - `agentcall_session`
  - `agentcall_session_send`
  - `agentcall_report`
- 更新策略变更：频繁业务更新应改 daemon 并重启 daemon；只有 MCP 协议外壳变化才需要重启 Codex MCP transport。
- 已知事实：已经断开的 Codex MCP transport 不能由代码热修复，需要一次 MCP 重连；本版本的目标是避免后续业务更新继续触发同类断连。

## v0.8b - Rust Native ACP Client

- daemon 新增 Rust 原生 ACP stdio JSON-RPC client，不再通过 Python workflow 承担默认 ACP 控制逻辑。
- ACP route 使用 daemon-owned bounded invocation：initialize、session/new、session/set_mode、session/prompt、permission request、session/update 均由 Rust client 处理。
- request id 对齐 Python `AcpStdioClient` reference，从 `0` 开始。
- stdout 和 stderr 并发读取；timeout 到期由 daemon kill 自己派生的 ACP 子进程。
- `agentcall_route(runtime=acp)` 必须显式提供 `adapter_command` 或设置 `AGENTCALL_ACP_COMMAND`，避免隐式触发 `npx` 下载或真实模型调用。
- 新增同进同出 parity 验收：同一个 fake ACP server、同一个 prompt/工作目录/模式，Python reference 和 Rust daemon native ACP 的 JSON-RPC 客户端消息序列及输出必须一致。

## v0.8a - Route-First MCP 收敛

- MCP 默认工具面收敛为 `board -> route -> session/report`。
- 新增 `agentcall_daemon(action=status|start)`，daemon 未运行时可由 MCP 拉起。
- `agentcall_route` 成为唯一高层调度入口，支持 `mode=recommend|start` 和 `runtime=auto|pty|acp`。
- `agentcall_route` 支持 `task_id/call_id/phase/role/allowed_paths/acceptance_criteria/persist_context`，route 可直接生成 context packet。
- `agentcall_delegate` / `agentcall_delegate_acp` 从默认工具面移除，不再执行 Python workflow。
- daemon 新增 `/api/routes`、`/api/routes/{id}`，route 和 invocation 状态由 daemon 记录并投影到 board。
- daemon 新增 `/api/context`、`/api/transcripts/index`、`/api/sessions/{name}/checkpoint`，关闭默认 checkpoint/context/transcript Python 写路。
- ACP v0.8a 仍是 daemon-owned transitional adapter；完整 Rust 原生 ACP client 留给后续版本。

## v0.7.1 - Hook-Aware Summary Binding

- wrapper session 与 Claude hook session 通过 `AGENTCALL_WRAPPER_SESSION` 做可靠绑定。
- daemon 单写 `.agentcall/state/runtime_binding.json`。
- `session_summary` 拆分 `liveness_status`、`attention_status`、`report_ready`。
- 修正 hook 语义：`Stop -> idle`，`SubagentStop -> checkpoint_due`，permission notification 保留为 `needs_permission`。
- 改善 PTY stop 与 clean output 可读性。

## v0.7 - Readable Wrapper + Low-Friction Codex Control

- 修复 PTY 输出 UTF-8 chunk 边界解码，新增流式 decoder 与 `decode_health`。
- 输出分为 `raw_output`、`clean_output`、`llm_summary`。
- legacy Python PTY 降级为 `legacy_detached`，不再出现在 live daemon session 默认列表。
- `runtime_health` 明确 `restart_required_after_update: true`。
- MCP 工具面开始向 board/session/session_send/report 收敛。

## v0.6.1 - Close Daemon Single-Writer Gap

- Claude/Codex hook 客户端改为 daemon-first，POST `/api/hooks/ingest`。
- Python `agentcall hook ingest` 退出 live 主路径，只保留 legacy/fallback 语义。
- 增加真实 OS 进程并发测试，验证同文件 claim 冲突、event id 唯一、NDJSON 可解析。
- daemon 拆分为 `state`、`hooks`、`session`、`summary`、`http`、`terminal` 等模块。

## v0.6 - Concurrent Trust Base

- 建立 daemon single-writer 模型：daemon 是 `events / file_claims / active_sessions / session_summary` 的 live 写入权威。
- event id 由 daemon 单调发号，新事件使用 ISO-8601 时间戳。
- File claim 区分写工具和读工具：`Write/Edit/MultiEdit/NotebookEdit` claim，`Read/Glob/Grep` observe。
- claim 增加 TTL 与 stale 惰性回收。
- 缺 session id 的 hook 进入 unmatched 或稳定 fallback id，不再坍缩为 `unknown-session`。

## v0.5.1 - Codex Hooks 与 MCP

- Codex 接入 AgentCall hooks/preflight。
- Rust MCP 暴露 board、route、claims、reports 等能力。
- 明确 Python 胶水与 Rust 端边界：高频 hook、进程 I/O、MCP/PTY 服务边界优先 Rust。

## v0.5 - Claude Code Handoff 可视化

- 增加 Claude Code hook 安装。
- 增加 file claim 冲突保护、transcript 索引、route 增强和可视化 board。
- 建立 Claude Code PTY handoff 的第一版可视化路径。

## v0.4 - 控制面原型

- 建立 AgentCall 控制面，区分 ACP agents-as-tools 与 Claude Code handoff。
- 引入 context packet、结构化报告、约束子任务。
- 初步支持 hook payload 写入 shared state。
