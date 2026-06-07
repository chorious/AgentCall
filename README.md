# AgentCall

当前版本：`v4.0.0`

AgentCall 是一个本地多 Agent 协作控制面，目标是让 **Codex 指挥 Claude Code PTY worker 集群** 完成工程协作。Codex 负责拆分、监督、验收和整合；Claude Code worker 负责执行清晰边界内的实现、检查、报告等任务。

v4.0 的重点是 **plugin-provided MCP**：AgentCall 不再只依赖用户级 `~/.codex/config.toml` 裸 MCP 配置，而是提供一个 Codex plugin，让 MCP server 和使用说明一起随插件加载，降低不同 Codex session / CODEX_HOME 下工具不注入的问题。

## 产品特点

- **Codex 主控，Claude Code 执行**：Codex 通过 AgentCall board、route、session、report 管理多个 Claude Code worker。
- **PTY-first**：默认使用 Claude Code PTY utility worker，保留人类可视化和 handoff 能力。
- **Plan gate 可选**：复杂任务可以先走 plan mode，确认后切到 auto mode；默认 utility worker 走 auto，Codex 可显式要求 plan。
- **Daemon single-writer**：live events、claims、sessions、bindings、routes、summary 由 Rust daemon 统一写入。
- **Hook-aware 状态**：Claude/Codex hooks 写入 daemon，summary 优先使用结构化 hook/report 状态，TUI 只做辅助摘要。
- **Readable wrapper**：daemon 维护 raw output、clean output、llm summary，Codex 默认读取紧凑状态。
- **Patience contract**：summary 提供 wait/retry 提示，减少 Codex 误判 worker 过慢。
- **Plugin-provided MCP**：v4.0 新增 repo 内 Codex plugin，解决裸 MCP 配置在 Codex Desktop / background thread 中不稳定加载的问题。

## 快速开始

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
  "claude_workspace": "D:\\guKimi"
}
```

`claude_workspace` 是 Claude Code PTY 的强制启动 cwd，也是 hook binding 的基础。route 请求中的 `workspace` 表示任务目标目录，不决定 Claude Code 进程 cwd。

启动 daemon：

```powershell
target\debug\agentcall-daemon.exe --workspace E:\Project\AgentCall
```

打开 board：

```text
http://127.0.0.1:3293/board
```

## Hooks 配置

安装或刷新 Claude Code hooks：

```powershell
python scripts\install_claude_hooks.py --root E:\Project\AgentCall
```

该命令会读取 `config\agentcall.local.json` 中的 `claude_workspace`，并写入：

```text
<claude_workspace>\.claude\settings.local.json
```

本机通常是：

```text
D:\guKimi\.claude\settings.local.json
```

`--root` 只表示 AgentCall 项目根，用来定位 `scripts\agentcall-claude-hook.py`；它不是 Claude Code worker 的 hook 配置目录。若需要手动指定 Claude 配置目录，可以使用：

```powershell
python scripts\install_claude_hooks.py --root E:\Project\AgentCall --settings-root D:\guKimi
```

修改 hooks 后，已经启动的 Claude PTY worker 不会热加载新配置；需要重启 worker。

安装或刷新 Codex hooks：

```powershell
python scripts\install_codex_hooks.py --root E:\Project\AgentCall
```

Hook 行为：

- `PreToolUse` / `PostToolUse` / `UserPromptSubmit` / `Notification` / `Stop` / `SubagentStop` 都写入 daemon。
- `Stop` 是普通 turn end，不作为 checkpoint。
- permission notification 保留为 `needs_permission`。
- 无可靠 wrapper binding 的 hook 标记为 `unbound`，不靠 cwd、PID、窗口标题或启动顺序猜归属。

## MCP / Plugin

v4.0 提供 repo 内插件：

```text
plugins/agentcall/
  .codex-plugin/plugin.json
  .mcp.json
  skills/agentcall/SKILL.md
```

注册本地 marketplace：

```powershell
codex plugin marketplace add E:\Project\AgentCall
```

安装插件：

```powershell
codex plugin add agentcall@personal
```

安装后请完整重启 Codex Desktop，再新开 Codex thread。仅在运行中的 Desktop 里新建线程，可能不会刷新 plugin-provided MCP 工具面。

重启后，AgentCall 插件应提供 MCP server 和 skill guidance。当前推荐工具面：

```text
agentcall_daemon
agentcall_board
agentcall_route
agentcall_session
agentcall_session_send
agentcall_report
```

注意：`tool_search agentcall` 可能返回 0，这是已观察到的假阴性。验收 AgentCall MCP 是否可用，应直接尝试 `agentcall_daemon(action="status")`。

典型流程：

1. `agentcall_daemon(action=start)` 确认 daemon 正常。
2. `agentcall_board(view=compact, filter=attention)` 查看需要介入的 worker。
3. `agentcall_route(mode=start, runtime=auto|pty, objective=..., allowed_paths=..., acceptance_criteria=...)` 启动 PTY utility worker。
4. `agentcall_session(name=..., include=["summary"])` 查看紧凑状态。
5. `agentcall_session_send(action=continue|request_report|revise_plan|approve_plan|start_auto)` 控制 worker。
6. `agentcall_report(action=request|accept)` 请求或验收报告。

## 常用 API

```text
GET  /api/runtime/health
GET  /api/board?view=compact&filter=attention
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

## 测试

```powershell
cargo test --workspace
python -m pytest -q
python -m compileall scripts src
python C:\Users\MUSHI\.codex\skills\.system\plugin-creator\scripts\validate_plugin.py E:\Project\AgentCall\plugins\agentcall
```

## 文档

- [CHANGELOG](CHANGELOG.md)
- [docs/README.md](docs/README.md)
- [v4.0 Plugin Provided MCP](docs/v4.0-plugin-provided-mcp.md)
- [v3.0 PTY Utility Workers](docs/v3.0-pty-utility-workers.md)
- [MCP transport recovery](docs/mcp-transport-recovery.md)
