# CHANGELOG

## v0.8a - Route-First MCP 收敛

- MCP 默认工具面收敛为 `agentcall_board`、`agentcall_route`、`agentcall_session`、`agentcall_session_send`、`agentcall_report`。
- 新增 canonical `agentcall_route`：支持 `mode=recommend|start` 与 `runtime=auto|pty|acp`。
- `runtime=auto` 要求调用方提供估计字段，避免 Codex 盲目推断任务规模。
- `agentcall_delegate` / `agentcall_delegate_acp` 从默认工具面移除；隐藏兼容 handler 只返回 deprecated 提示，不再执行 Python workflow。
- daemon 新增 `/api/routes`、`/api/routes/{id}`，route 和 invocation 状态由 daemon 记录并投影到 board。
- daemon 新增 `/api/context`、`/api/transcripts/index`、`/api/sessions/{name}/checkpoint`，关闭 MCP 默认 checkpoint/context/transcript Python 写路。
- ACP v0.8a 采用 daemon-owned transitional invocation：daemon 记录状态、可选启动 adapter、负责 timeout/kill；完整 Rust 原生 ACP client 留给 v0.8b。
- PTY route 复用 daemon session，并要求后续 hook-aware binding 通过 `AGENTCALL_WRAPPER_SESSION` 建立可信绑定。

## v0.7.1 - Hook-Aware Summary Binding

- 新增 wrapper session 与 Claude hook session 的可靠绑定：PTY 启动时注入 `AGENTCALL_WRAPPER_SESSION`，hook payload 带回 daemon。
- 新增 daemon 单写的 `.agentcall/state/runtime_binding.json`。
- `session_summary` 拆分状态维度：`liveness_status`、`attention_status`、`report_ready`。
- 修正 hook 语义：`Stop -> idle`，`SubagentStop -> checkpoint_due`，permission notification 保留为 `needs_permission`。
- 修复 Windows headless ConPTY 启动卡住：daemon PTY 会回答 `ESC[6n` DSR 光标查询。
- 修复 PTY stop 死锁：使用 `portable-pty` 的 `clone_killer()`。
- 改善 clean output 可读性，保留关键换行/空格语义。
- 降低 compact attention 噪声：legacy Python PTY 不再默认进入 attention。
- 修复 Claude/Codex hook UTF-8：stdin 使用 `utf-8-sig`，stdout/stderr 强制 UTF-8。

## v0.7 - Readable Wrapper + Low-Friction Codex Control

- 修复 PTY 输出 UTF-8 chunk 边界解码，新增流式 decoder 与 `decode_health`。
- 输出拆成 `raw_output`、`clean_output`、`llm_summary` 三层。
- Legacy Python PTY 降级为 `legacy_detached`，不再出现在 live daemon session 列表。
- `runtime_health` 明确 `restart_required_after_update: true`。
- MCP 工具面开始收敛到 board/session/session_send/report。

## v0.6.1 - Close Daemon Single-Writer Gap

- Claude/Codex hook 客户端改为 daemon-first：POST `/api/hooks/ingest`。
- Python `agentcall hook ingest` 退出 live 主路径，仅保留 legacy/fallback 语义。
- 新增并发验收：独立 OS 进程同时调用 hook，验证同文件 claim 稳定冲突。
- daemon 拆模块：`state`、`hooks`、`session`、`summary`、`http`、`terminal`。

## v0.6 - Concurrent Trust Base

- 确立 daemon single-writer 模型：daemon 是 `events / file_claims / active_sessions / session_summary` 的 live 写者。
- event id 由 daemon 单调发号，新事件使用 ISO-8601 时间戳。
- File claim 区分写工具和读工具：`Write/Edit/MultiEdit/NotebookEdit` claim，`Read/Glob/Grep` observe。
- claim 增加 TTL 与 stale 惰性回收。
- 缺 session id 的 hook 进入 unmatched 或稳定 fallback id，不再坍缩为 `unknown-session`。

## v0.5.1 - Codex Hooks 与 MCP

- Codex 接入 AgentCall hooks/preflight。
- Rust MCP 暴露 board、route、claims、reports 等能力。
- 明确 Python 胶水与 Rust 后端边界：高频 hook、进程 I/O、MCP/PTY 服务边界优先 Rust。

## v0.5 - Claude Code Handoff 可观察性

- 增加 Claude Code hook 安装。
- 增加 file claim 冲突保护、transcript 索引、route 增强和可视化 board。
- 建立 Claude Code PTY handoff 的第一版可观察路径。

## v0.4 - 编排控制面

- 建立 AgentCall 控制面，区分 ACP agents-as-tools 与 Claude Code handoff。
- 引入 context packet、结构化报告、约束子任务。
- 初步支持 hook payload 写入 shared state。
