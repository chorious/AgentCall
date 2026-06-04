# AgentCall

当前版本：`v2.4.0`

AgentCall 是一个本地多 Agent 协作控制面，用来让 **Codex 指挥 Claude Code 集群协同工作**。它把 Codex 作为主控，把多个 Claude Code 实例作为可观察、可路由、可验收的工作单元，并用 Rust daemon 统一管理 PTY 会话、ACP 调用、hooks、file claim、runtime binding、summary、board 和 HTTP API。

核心目标不是“多开几个终端”，而是让 Codex 能低成本回答这些问题：

- 哪些 Claude Code worker 正在工作、等待输入、需要权限或已经跑偏？
- 哪个 worker 正在改哪些文件，是否发生同文件冲突？
- 任务应该走可视化 PTY handoff，还是 bounded ACP agents-as-tools？
- Claude 完成任务后有没有 report，Codex 是否需要验收、复派或接管？
- 更新 daemon 后如何保持 MCP transport 稳定，避免每次工具面变化都断线？

## 产品特点

- **Codex 主控，Claude Code 集群执行**：Codex 通过 `agentcall_board -> agentcall_route -> agentcall_session/agentcall_report` 组织多个 Claude Code worker。
- **Route-first 调度**：`agentcall_route` 是唯一高层入口，支持 `runtime=auto|pty|acp`。ACP 在 v2.2 起被收敛为轻量化 SOP worker；v2.4 起由 daemon 后台 supervisor 管理，默认 30 分钟 hard timeout。PTY 在 v2.3 起默认走 `plan_then_auto`，先让 Claude Code 产出计划，再由主管批准进入 auto mode。
- **ACP SOP Worker Supervisor**：`runtime=auto` 不再根据 `estimated_*` 猜“小任务”。调用方必须提供 `template`、`target_files`、`report_path`、`allowed_paths` 和 `acceptance_criteria`；通过 gate 才会进入 ACP。ACP 后台运行，heartbeat 只更新 `acp_invocations.json`，无进展进入 `checkpoint_due`，超时进入 `failed_timeout`。
- **PTY Plan Gate**：非 SOP 或复杂任务默认进入 PTY plan mode。`ExitPlanMode` 会被 hook 识别成 `plan_ready`；Codex 可用 `agentcall_session_send(action=approve_plan|revise_plan)` 批准或要求修订。
- **Rust daemon 单写状态**：daemon 是 live events、claims、sessions、bindings、routes、summary 的权威写者，避免 Python/Rust 双写漂移。
- **Hook-aware 状态绑定**：Claude/Codex hooks 进入 daemon，`AGENTCALL_WRAPPER_SESSION` 把 wrapper session 和 Claude hook session 可靠绑定。
- **File claim 冲突保护**：`Write/Edit/MultiEdit/NotebookEdit` 才 claim；`Read/Glob/Grep` 只 observe。同文件并发写入会被 daemon 稳定拒绝。
- **Readable wrapper**：PTY 输出分为 raw、clean、llm_summary。Codex 默认读 summary，不需要长期扫 raw terminal。
- **稳定 MCP bridge**：MCP 本地只固定暴露 `agentcall_daemon`，业务工具面由 daemon 动态提供，减少 Codex MCP transport 重启需求。
- **本机配置隔离**：`D:\guKimi` 这类 Claude Code 启动目录只放在 local config，不作为发布硬编码默认。

## 快速启动

构建 Rust 组件：

```powershell
cargo build -p agentcall-daemon -p agentcall-mcp -p agentcall-hook
```

创建本机 daemon 配置：

```powershell
Copy-Item config\agentcall.example.json config\agentcall.local.json
```

编辑 `config\agentcall.local.json`：

```json
{
  "claude_workspace": "D:\\guKimi",
  "acp_command": ["npx", "-y", "@agentclientprotocol/claude-agent-acp"],
  "acp_default_timeout_seconds": 1800,
  "acp_max_timeout_seconds": 1800,
  "acp_checkpoint_due_seconds": 600,
  "acp_heartbeat_interval_seconds": 60,
  "acp_max_active_invocations": 2
}
```

`config\agentcall.local.json` 不提交到 git。Claude PTY 的 cwd 只取这里的 `claude_workspace`；route/session 请求里的 `workspace` 只表达任务目标和上下文，不决定 Claude Code 启动目录。ACP route 默认使用这里的 `acp_command`。缺少 `claude_workspace` 时，daemon health 会返回 `status=config_missing`，Claude PTY route 会拒绝启动并提示补配置。

启动 daemon：

```powershell
target\debug\agentcall-daemon.exe --workspace E:\Project\AgentCall
```

打开 board：

```text
http://127.0.0.1:3293/board
```

## Hooks 配置

AgentCall v2.0 需要 Claude Code hooks 和 Codex hooks 都走 daemon-first 路线。

安装或刷新 Claude hooks：

```powershell
python scripts\install_claude_hooks.py --root E:\Project\AgentCall
```

安装或刷新 Codex hooks：

```powershell
python scripts\install_codex_hooks.py --root E:\Project\AgentCall
```

hooks 行为：

- `PreToolUse` / `PostToolUse` / `UserPromptSubmit` / `Notification` / `Stop` / `SubagentStop` 会 POST 到 daemon `/api/hooks/ingest`。
- daemon 根据 hook payload 更新 runtime binding、file claims、summary 和 board。
- `Notification` 中的 permission 信号会保留为 `needs_permission`，进入 attention。
- `Stop` 是普通 turn end，不会被误判为 checkpoint。
- 如果 hook 没带 `AGENTCALL_WRAPPER_SESSION` 且没有既有绑定，daemon 会标记为 `unbound`，不会猜 cwd、PID 或窗口标题。

## Config 配置

提交到 git 的模板：

```text
config/agentcall.example.json
```

本机使用但不提交：

```text
config/agentcall.local.json
```

字段：

```json
{
  "claude_workspace": "D:\\guKimi",
  "acp_command": ["npx", "-y", "@agentclientprotocol/claude-agent-acp"],
  "acp_default_timeout_seconds": 1800,
  "acp_max_timeout_seconds": 1800,
  "acp_checkpoint_due_seconds": 600,
  "acp_heartbeat_interval_seconds": 60,
  "acp_max_active_invocations": 2
}
```

`claude_workspace` 是 Claude Code 的强制启动 cwd，也是 hooks 绑定语义的一部分。所有 Claude PTY session 无论 route 传入什么 `workspace`，都会使用该值。非 Claude 命令才使用请求 cwd 或 daemon workspace。

`acp_command` 是 daemon-owned ACP adapter 命令。Codex 调用 `agentcall_route(runtime=acp)` 时不需要每次传 `adapter_command`；daemon 会优先使用请求里的 `adapter_command`，其次使用 local config 的 `acp_command`，最后才看 `AGENTCALL_ACP_COMMAND`。

ACP supervisor 配置：

- `acp_default_timeout_seconds`：ACP worker 默认 hard budget，默认 `1800`。
- `acp_max_timeout_seconds`：单次 ACP 允许的最大 hard budget，默认 `1800`；请求超过该值会被拒绝。
- `acp_checkpoint_due_seconds`：无 hook/update/report 进展多久后标记 `checkpoint_due`，默认 `600`。
- `acp_heartbeat_interval_seconds`：后台 heartbeat 覆盖更新 state 的间隔，默认 `60`。
- `acp_max_active_invocations`：同时运行的 ACP worker 上限，默认 `2`；超过时返回 `acp_capacity_exceeded`，不排队、不自动转 PTY。

## MCP / Codex 配置

Codex 的 MCP 配置应指向编译后的 `agentcall-mcp.exe`：

```toml
[mcp_servers.agentcall]
command = 'E:\Project\AgentCall\target\debug\agentcall-mcp.exe'
args = ["--workspace", 'E:\Project\AgentCall']
enabled = true
```

本地 MCP 固定工具：

```text
agentcall_daemon
```

daemon 动态提供 canonical 工具：

```text
agentcall_board
agentcall_route
agentcall_session
agentcall_session_send
agentcall_report
```

`agentcall_daemon(action=start)` 是 bootstrap 入口。业务工具面更新时通常只需要重启 daemon；只有 MCP stdio bridge 本身变化时才需要重启 Codex MCP transport。

## 常用 Daemon API

```text
GET  /api/runtime/health
GET  /api/mcp/tools
POST /api/mcp/call
GET  /api/board?view=compact&filter=attention
GET  /api/board?section=acp
GET  /api/sessions
GET  /api/sessions/{name}/summary
GET  /api/sessions/{name}/output/clean
POST /api/routes
GET  /api/routes/{id}
POST /api/sessions
POST /api/sessions/{name}/input
POST /api/sessions/{name}/checkpoint
POST /api/sessions/{name}/stop
POST /api/context
POST /api/transcripts/index
POST /api/hooks/ingest
```

## 典型工作流

1. Codex 调用 `agentcall_board(view=compact, filter=attention)` 看全局状态。
2. Codex 调用 `agentcall_route(mode=recommend, runtime=auto, template=..., target_files=..., report_path=..., allowed_paths=..., acceptance_criteria=...)` 让 daemon 校验 SOP contract；缺 contract 会返回 `needs_contract`。
3. Codex 调用 `agentcall_route(mode=start, runtime=pty|acp, ...)` 派发任务；ACP 只用于 5 个 SOP worker，快速返回 `route_id/invocation_id` 后由 daemon supervisor 后台管理，复杂长任务默认走 PTY `plan_then_auto`。
4. Claude Code worker 执行任务，hooks 把状态和文件 claim 写回 daemon。
5. PTY worker 若进入 `plan_ready`，Codex 或用户用 `agentcall_session_send(action=approve_plan)` 批准执行，或用 `revise_plan` 要求补计划。
6. Codex 读取 `agentcall_session` 的 summary 和 worker report。
7. 无风险直接接受；只有 conflict、drift、blocked、failed、low confidence 时复派或 review。

## 测试

```powershell
cargo test --workspace
python -m pytest -q
python -m compileall scripts src
```

## 文档

- [About](docs/about.md)
- [CHANGELOG](CHANGELOG.md)
- [文档索引](docs/README.md)
- [当前 MCP/daemon 控制面](docs/v3.0-mcp.md)
- [PTY Plan Gate](docs/v2.3-pty-plan-gate.md)
- [MCP transport 恢复](docs/mcp-transport-recovery.md)
