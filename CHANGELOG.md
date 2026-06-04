# CHANGELOG

## v3.0.0 - PTY Utility Workers

- 产品面收敛为 PTY-first：`agentcall_route(runtime=auto|pty)` 启动 Claude Code PTY utility worker。
- ACP 从当前实现中移除：MCP schema 不暴露 ACP 参数，`runtime=acp` 返回非法 runtime，daemon 不再启动 ACP worker。
- 删除 Rust ACP runtime/supervisor 和 Python ACP driver/test，避免残留路线误导调用方。
- PTY worker 默认 `permission-mode auto`；`plan_then_auto` 只在调用方显式请求时启用。
- route 增加 `worker_kind=utility`、`containment`、prompt submit gate，降低“session 创建了但任务没送达”的误判。
- hook policy 增加 PTY path enforcement；带 `allowed_paths` 的 PTY route 会拒绝越界写入。
- MCP 工具面收敛为 `agentcall_daemon / board / route / session / session_send / report`。
- README、about、v3 文档更新为 PTY-first 当前能力说明。
- 新增 [docs/v3.0-pty-utility-workers.md](docs/v3.0-pty-utility-workers.md)。

## v2.4.0 - ACP Background Supervisor

- ACP route 曾改为后台 supervisor 模式，默认 30 分钟 hard timeout。
- 新增 ACP invocation state、heartbeat、checkpoint_due、capacity cap 与 orphan 标记。
- 该路线在 v3.0 被移除，不再作为当前产品能力。

## v2.3.0 - PTY Plan Gate

- PTY route 支持 `plan_then_auto`。
- `ExitPlanMode` hook 会把 route 标记为 `plan_ready`。
- `agentcall_session_send` 增加 `approve_plan`、`start_auto`、`revise_plan`。

## v2.2.0 - ACP SOP Worker Gate

- ACP 曾收敛为 SOP worker，支持 read/report 类模板。
- 新增 template-aware permission policy 与 report contract。
- 该路线在 v3.0 被移除。

## v2.1.0 - ACP Child Lifecycle Binding

- ACP route 曾注入 `AGENTCALL_WRAPPER_SESSION` 并投影 child lifecycle。
- 修正 ACP cwd 与 PTY 一致，均使用 daemon local config 的 `claude_workspace`。
- 该路线在 v3.0 被移除。

## v2.0.0 - Codex-Controlled Claude Code Cluster

- AgentCall 定位为让 Codex 指挥 Claude Code 集群协同工作的本地控制面。
- README 改为产品说明，明确 route-first MCP、hook-aware binding、file claim、readable wrapper 和 daemon single-writer。

## v0.8.1 - Stable MCP Bridge

- MCP stdio bridge 收敛为稳定外壳，工具实现由 daemon 动态提供。
- 新增 `agentcall_daemon(action=status|start)` bootstrap。

## v0.8a - Route-First MCP

- MCP 默认流程收敛为 `board -> route -> session/report`。
- `agentcall_delegate*` 从默认工具面移除。

## v0.7.1 - Hook-Aware Summary Binding

- 新增 wrapper session 与 Claude hook session 的 runtime binding。
- `session_summary` 拆出 `liveness_status`、`attention_status`、`report_ready`。

## v0.7 - Readable Wrapper + Low-Friction Codex Control

- PTY 输出拆为 `raw_output`、`clean_output`、`llm_summary`。
- 修复 UTF-8 chunk 边界解码并新增 `decode_health`。

## v0.6.1 - Close Daemon Single-Writer Gap

- Claude/Codex hook 客户端改为 daemon-first：POST `/api/hooks/ingest`。
- Python hook ingest 降级为 legacy/fallback。

## v0.6 - Concurrent Trust Base

- 建立 daemon single-writer 模型。
- File claim 区分 write claim 与 read observe。

## v0.5.1 - Codex Hooks And MCP

- Codex 接入 AgentCall hooks/preflight。
- Rust MCP 暴露 board、route、claims、reports 等能力。

## v0.5 - Claude Code Handoff Visualization

- 新增 Claude Code hooks、file claim、transcript index 与 PTY handoff board。

## v0.4 - Control Plane Origin

- 建立 AgentCall 控制面原型，探索 agents-as-tools 与 Claude Code handoff 两条路线。
