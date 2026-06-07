# AgentCall

当前版本 / Current version: `v4.1.1`

AgentCall is a local coordination plane that lets **Codex supervise a cluster of Claude Code PTY utility workers**. Codex remains the parent agent: it splits work, starts workers, watches board/session state, asks for reports, and accepts or revises results. Claude Code workers execute bounded tasks inside visible PTY sessions.

AgentCall 是一个本地多 Agent 协作控制面，目标是让 **Codex 指挥 Claude Code PTY worker 集群** 完成工程协作。Codex 负责拆分、监督、验收和整合；Claude Code worker 负责执行清晰边界内的实现、检查、报告等任务。

## 产品特点

- **Codex 主控，Claude Code 执行**：Codex 通过 AgentCall board、route、session、report 管理多个 Claude Code worker。
- **PTY-first**：默认使用 Claude Code PTY utility worker，保留人类可视化和 handoff 能力。
- **Daemon single-writer**：live events、claims、sessions、bindings、routes、summary 由 Rust daemon 统一写入。
- **Hook-aware 状态**：Claude/Codex hooks 写入 daemon，summary 优先使用结构化 hook/report 状态，TUI 只做辅助摘要。
- **Readable wrapper**：daemon 维护 raw output、clean output、LLM summary，Codex 默认读取紧凑状态。
- **Patience contract**：summary 提供 wait/retry 提示，减少 Codex 误判 worker 过慢。
- **Plugin-provided MCP**：repo 内 Codex plugin 让 MCP server 和使用说明一起随插件加载，降低不同 Codex session / CODEX_HOME 下工具不注入的问题。

## Features

- **Codex-led orchestration**: Codex manages Claude Code workers through board, route, session, and report tools.
- **PTY-first workers**: Claude Code runs in daemon-owned PTY sessions, so humans can still watch and hand off.
- **Rust daemon authority**: runtime events, file claims, sessions, bindings, routes, and summaries are written by the daemon.
- **Hook-aware summaries**: Claude/Codex hooks provide structured status; TUI text is treated as a weak readability hint.
- **Low-friction control**: compact board/session summaries reduce context cost for Codex.
- **Plugin-provided MCP**: the repo ships a Codex plugin so AgentCall tools can be loaded by the app without hand-copying user-level MCP config.

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

启动 daemon：

```powershell
target\debug\agentcall-daemon.exe --workspace E:\Project\AgentCall
```

打开 Board：

```text
http://127.0.0.1:3293/board
```

## Quick Start

Build the Rust binaries:

```powershell
cargo build -p agentcall-daemon -p agentcall-mcp -p agentcall-hook
```

Create local daemon config:

```powershell
Copy-Item config\agentcall.example.json config\agentcall.local.json
```

Set `claude_workspace` in `config\agentcall.local.json`, then start the daemon:

```powershell
target\debug\agentcall-daemon.exe --workspace E:\Project\AgentCall
```

Open:

```text
http://127.0.0.1:3293/board
```

## 脚本入口

推荐从这个入口做日常检查、安装和发布前校验：

```powershell
python agentcall.py doctor
python agentcall.py install-hooks
python agentcall.py release-check
python agentcall.py daemon-health
python agentcall.py paths
```

这些脚本会尽量给出可定位的报错，例如：

- `cargo` 不在 PATH 时，会自动尝试 `C:\Users\<you>\.cargo\bin\cargo.exe`，找不到才失败。
- Claude hooks 会检查 `claude_workspace\.claude\settings.local.json`，并列出缺失的 hook event。
- daemon health 会短超时请求 `/api/runtime/health`，用于区分 daemon 未启动、旧 daemon 卡住、JSON 返回异常。
- `release-check` 会跑 Python compile、Board JS 语法检查、plugin validation、Cargo tests、pytest 和 `git diff --check`。

## Script Entry

Use the repo entrypoint for routine diagnostics, hook installation, and release checks:

```powershell
python agentcall.py doctor
python agentcall.py install-hooks
python agentcall.py release-check
python agentcall.py daemon-health
python agentcall.py paths
```

The scripts are designed to fail loudly with actionable hints: missing Cargo, stale hook settings, daemon health timeout, plugin validation failure, pytest failure, or whitespace diff errors.

## Hooks 与 cwd 配置

`claude_workspace` 是 AgentCall 最容易误解、也最重要的配置：

- 它是 **Claude Code worker 的强制启动 cwd**。
- 它是 **Claude Code hooks 被读取的位置**。
- 它是 `AGENTCALL_WRAPPER_SESSION` hook binding 的稳定基础。
- route 请求里的 `workspace` 只表示任务目标目录，不决定 Claude Code 进程 cwd。

例如本机配置：

```json
{
  "claude_workspace": "D:\\guKimi"
}
```

则 Claude Code worker 总是在 `D:\guKimi` 下启动，并读取：

```text
D:\guKimi\.claude\settings.local.json
```

安装或刷新 Claude Code hooks：

```powershell
python scripts\install_claude_hooks.py --root E:\Project\AgentCall
```

该命令会读取 `config\agentcall.local.json`，然后写入：

```text
<claude_workspace>\.claude\settings.local.json
```

`--root` 只表示 AgentCall 项目根，用来定位 `scripts\agentcall-claude-hook.py`；它不是 Claude Code worker 的 hook 配置目录。需要显式指定时：

```powershell
python scripts\install_claude_hooks.py --root E:\Project\AgentCall --settings-root D:\guKimi
```

修改 hooks 后，已经启动的 Claude PTY worker 不会热加载新配置；需要重启 worker。

安装或刷新 Codex hooks：

```powershell
python scripts\install_codex_hooks.py --root E:\Project\AgentCall
```

## Hooks And cwd Config

`claude_workspace` is the authoritative runtime directory for Claude Code workers:

- It is the forced cwd for Claude Code PTY sessions.
- It is where Claude Code reads `.claude/settings.local.json`.
- It anchors hook binding through `AGENTCALL_WRAPPER_SESSION`.
- The route `workspace` field is the task target directory, not the Claude process cwd.

If `claude_workspace` is `D:\guKimi`, AgentCall starts Claude Code in `D:\guKimi` and installs hooks into:

```text
D:\guKimi\.claude\settings.local.json
```

Install or refresh Claude Code hooks:

```powershell
python scripts\install_claude_hooks.py --root E:\Project\AgentCall
```

Running Claude workers must be restarted after hook changes.

## MCP / Plugin

AgentCall ships a repo-local Codex plugin:

```text
plugins/agentcall/
  .codex-plugin/plugin.json
  .mcp.json
  skills/agentcall/SKILL.md
```

Register the local marketplace and install the plugin:

```powershell
codex plugin marketplace add E:\Project\AgentCall
codex plugin add agentcall@personal
```

Then fully restart Codex Desktop and create a new thread. Creating a new thread inside an already-running Desktop host may not refresh plugin-provided MCP tools.

Recommended tool flow:

```text
agentcall_daemon(action=start)
agentcall_board(view=compact, filter=attention)
agentcall_route(mode=start, runtime=auto|pty, ...)
agentcall_session(name=..., include=["summary"])
agentcall_session_send(action=continue|request_report|select_option|stop)
agentcall_report(action=request|accept)
```

Note: `tool_search agentcall` can return a false negative. Validate AgentCall MCP by directly calling `agentcall_daemon(action="status")`.

## 常用 API / API

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

## 测试 / Tests

```powershell
cargo test --workspace
python -m pytest -q
python -m compileall scripts src
python C:\Users\MUSHI\.codex\skills\.system\plugin-creator\scripts\validate_plugin.py E:\Project\AgentCall\plugins\agentcall
```

## 文档 / Docs

- [CHANGELOG](CHANGELOG.md)
- [docs/README.md](docs/README.md)
- [About AgentCall](docs/about.md)
- [v4.0 Plugin Provided MCP](docs/v4.0-plugin-provided-mcp.md)
- [v3.0 PTY Utility Workers](docs/v3.0-pty-utility-workers.md)
- [MCP transport recovery](docs/mcp-transport-recovery.md)
