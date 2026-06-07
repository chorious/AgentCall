# CHANGELOG

## v4.2.0 - Bounded Write And Policy Block Attention

- PTY route 默认生成 session scratch，并在 containment 中暴露 `writable_paths`、`scratch_path`、`bash_write_policy`。
- 写工具可写 `report_path`、session scratch 和显式 `allowed_paths`；Bash 首版仍保持 readonly-only。
- 重复 policy deny 聚合到 `policy_denials.json`，summary/board 显示 `blocked_by_policy`，不再提示 Codex继续耐心等待。
- policy deny loop 会通过 hook context 注入一次纠偏提示，要求 Claude 不要机械重试同一被拒动作。
- `agentcall_route` 增加 `read_only` 参数；显式 read-only route 不会自动授予 writable scratch。
- Board attention 展示 policy block 类别、次数和 deny reason；`doctor` 增加 scratch 目录提示。
- `.gitignore` 增加本地 Anthropic/router 启动文件忽略规则，避免把本机路由 fork/脚本提交到 main。

## v4.1.1 - Scripted Diagnostics And Release Checks

- 新增 `python agentcall.py ...` 总入口，覆盖 `doctor`、`install-hooks`、`release-check`、`daemon-health`、`paths`。
- `doctor` 检查 repo/config/cargo/python/node/plugin/Claude hooks/daemon/git，并在缺 hook event、缺 cargo、daemon timeout 时给出定位提示。
- `release-check` 固化提交前常用校验：Python compile、Board JS syntax、plugin validation、Cargo workspace tests、pytest、`git diff --check`。
- README 增加脚本入口说明，降低 Codex/Claude/human 重复记命令的上下文成本。

## v4.1.0 - Bilingual Release And Board Refresh

- README 改为中英文双语，明确 AgentCall 的产品定位、MCP/plugin 安装方式、hooks 与 `claude_workspace` / cwd 的关系。
- 插件 release 版本更新到 `4.1.0`。
- Board UI 切到 compact daemon board 数据，优先展示 live sessions、attention、routes 和 reports，减少读取大事件日志的成本。
- 根目录 review/report 文档归档到 `docs/arch/review`，保持项目根目录干净。

## v4.0.1 - AgentCall MCP Smoke Clarification

- 插件 skill 明确：不要用 `tool_search agentcall` 判断 AgentCall 是否可用。
- AgentCall MCP 可用性验收改为直接调用 `agentcall_daemon(action="status")`。
- 修正派生线程反复误判 “AgentCall 不存在” 的操作路径。

## v4.0.0 - Plugin-Provided MCP

- 新增 repo 内 Codex plugin：`plugins/agentcall`。
- 插件通过 `.codex-plugin/plugin.json` 声明 `mcpServers`，通过 `.mcp.json` 暴露 AgentCall MCP server。
- 新增 `skills/agentcall/SKILL.md`，把 AgentCall 默认协作流程、耐心策略、禁止 HTTP fallback 等规则交给 Codex。
- 新增 `.agents/plugins/marketplace.json`，允许通过 `codex plugin marketplace add E:\Project\AgentCall` 安装本地 marketplace。
- README 改为干净 UTF-8，明确 v4.0 的产品定位、hooks 配置、daemon config 和 plugin-provided MCP 安装方式。

## v3.0.1 - Daemon Hook Hardening

- `SessionStart` / `UserPromptSubmit` hook 返回 `context_injection`，把 AgentCall board discipline 注入 worker 上下文。
- HTTP body 增加 1 MiB 上限，超限返回 `413 Payload Too Large`。
- WebSocket frame 增加 64 KiB 上限，超限记录 `ws.frame_too_large` 并断开。
- daemon 增加单实例 runtime lock，避免多个 daemon 同时写共享状态。
- hook event 按类型写入 `.agentcall/logs/hooks/{HookType}.ndjson`。
- `hook.PostToolUse` 的大 stdout/stderr 会写入 artifact，事件里只保留压缩摘要。
- 新增 `scripts/align_hook_logs.py`，用于把旧 events 重新整理成分类 hook 日志。

## v3.0.0 - PTY Utility Workers

- 产品面收敛为 PTY-first：`agentcall_route(runtime=auto|pty)` 启动 Claude Code PTY utility worker。
- 移除 ACP 作为默认 runtime；`runtime=acp` 不再作为当前主线能力。
- PTY worker 默认 `permission-mode auto`；复杂任务可显式使用 `pty_workflow=plan_then_auto`。
- route 增加 `worker_kind=utility`、`containment`、prompt submit gate，降低 session 已创建但任务未送达的误判。
- hook policy 增加 PTY path enforcement，带 `allowed_paths` 的 PTY route 会参与写入边界判断。
- MCP 工具面收敛为 `agentcall_daemon / board / route / session / session_send / report`。
- 新增 [docs/v3.0-pty-utility-workers.md](docs/v3.0-pty-utility-workers.md)。

## v2.4.0 - ACP Background Supervisor

- ACP route 改为后台 supervisor 模式，默认 30 分钟 hard timeout。
- 新增 ACP invocation state、heartbeat、checkpoint_due、capacity cap 与 orphan 标记。
- 该路线在 v3.0 被移除出当前产品能力。

## v2.3.0 - PTY Plan Gate

- PTY route 支持 `plan_then_auto`。
- `ExitPlanMode` hook 会把 route 标记为 `plan_ready`。
- `agentcall_session_send` 增加 `approve_plan`、`start_auto`、`revise_plan`。

## v2.2.0 - ACP SOP Worker Gate

- ACP 收敛为 SOP worker，首版只允许 read/report 类模板。
- 新增 template-aware permission policy 与 report contract。
- 该路线在 v3.0 被移除出当前产品能力。

## v2.1.0 - ACP Child Lifecycle Binding

- ACP route 注入 `AGENTCALL_WRAPPER_SESSION` 并投影 child lifecycle。
- 修正 ACP cwd 与 PTY 一致，均使用 daemon local config 的 `claude_workspace`。

## v2.0.0 - Codex-Controlled Claude Code Cluster

- AgentCall 定位为让 Codex 指挥 Claude Code 集群协作的本地控制面。
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

- PTY 输出拆成 `raw_output`、`clean_output`、`llm_summary`。
- 修复 UTF-8 chunk 边界解码，新增 `decode_health`。

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
