# CHANGELOG

## v0.7.1 - Hook-Aware Summary Binding

- 新增 wrapper session 与 Claude hook session 的可靠绑定：PTY 启动时注入 `AGENTCALL_WRAPPER_SESSION`，hook payload 带回 daemon。
- 新增 daemon 单写的 `.agentcall/state/runtime_binding.json`。
- binding 来源限定为 `env`、`known_session`、`unbound`；无 env 且从未绑定过的 hook 不再用 cwd、PID、窗口标题或启动顺序猜归属。
- `session_summary` 拆分状态维度：`liveness_status`、`attention_status`、`report_ready`。
- `report_ready` 独立于 liveness，不再和 `working/idle` 抢唯一 `status`。
- 修正 hook 语义：`Stop -> idle`，`SubagentStop -> checkpoint_due`，permission notification 保留为 `needs_permission`。
- `agentcall_board(view=compact, filter=attention)` 不再展示普通 `idle/Stop` 噪声，只展示真正需要介入的会话。

## v0.7 - Readable Wrapper + Low-Friction Codex Control

- 修复 PTY 输出 UTF-8 chunk 边界解码，新增流式 decoder 与 `decode_health`。
- 输出分为 `raw_output`、`clean_output`、`llm_summary` 三层。
- Legacy Python PTY 降级为 `legacy_detached`，不再出现在 live daemon session 列表。
- `runtime_health` 明确 `restart_required_after_update: true`。
- MCP 工具面收敛，默认控制流转向 `agentcall_board`、`agentcall_session`、`agentcall_session_send`、`agentcall_report`。

## v0.6.1 - Close Daemon Single-Writer Gap

- Claude/Codex hook 客户端改为 daemon-first，POST `/api/hooks/ingest`。
- Python `agentcall hook ingest` 退出 live 主路径，仅保留 legacy/fallback 语义。
- 新增并发验收：独立 OS 进程同时调用 hook 脚本，验证同文件 claim 稳定冲突、不同文件并发稳定通过。
- daemon `main.rs` 拆模块：`state`、`hooks`、`session`、`summary`、`http`、`terminal`。

## v0.6 - Concurrent Trust Base

- 确立 daemon single-writer 模型：daemon 是 `events / file_claims / active_sessions / session_summary` 的 live 权威写者。
- event id 由 daemon 单调发号，新事件使用 ISO-8601 时间戳。
- File claim 区分写工具与读工具：`Write/Edit/MultiEdit/NotebookEdit` claim，`Read/Glob/Grep` observe。
- claim 增加 TTL，stale 惰性回收。
- 缺 session id 的 hook 进入 unmatched 或稳定 fallback id，不再坍缩成 `unknown-session`。

## v0.5.1 - Codex Hooks 与架构收敛

- Codex 接入 AgentCall hooks/preflight 体系。
- Rust MCP 提供 `agentcall_codex_preflight`，帮助主程检查 board、route、claims、reports。
- 明确 Python 胶水与 Rust 后端边界：高频 hook、进程 I/O、MCP/PTY 服务边界优先 Rust。

## v0.5 - Claude Code Handoff 可观察性

- 增加 Claude Code hook 安装脚本。
- 增加 file claim 冲突保护、transcript 索引、route 增强和可视化 board。
- 建立 Claude Code PTY handoff 的第一版可观察路径。

## v0.4 - 编排控制面

- 建立 AgentCall 控制面：MCP 暴露能力，route 区分 ACP agents-as-tools 与 Claude Code handoff。
- 引入 context packet，用结构化输入约束子任务。
- 初步支持 hook payload 进入 shared state。
