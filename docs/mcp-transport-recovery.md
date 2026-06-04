# MCP Transport Recovery

AgentCall daemon 和 AgentCall MCP bridge 是两层不同的东西：

- daemon：HTTP 控制面，默认 `http://127.0.0.1:3293`。
- MCP transport：Codex app 启动的 stdio 连接，命令来自 `C:\Users\MUSHI\.codex\config.toml`。

如果 daemon 正常，但 Codex 工具调用返回：

```text
Transport closed
```

说明坏的是 Codex 当前会话持有的 MCP stdio transport，而不是 daemon。

## 重要边界

关闭的 MCP transport 不能靠 AgentCall MCP 工具原地修复，因为工具调用本身已经走不进来。

当前 Codex 暂未暴露可调用的 `reload MCP server` 工具。因此恢复只能 out-of-band：

- 首选：新建/重开 Codex thread，获得新的 MCP stdio transport。
- 次选：重启 Codex app。
- 强制：终止当前 Codex runtime 进程，让 Codex app 重新创建运行时。这个动作会中断当前 turn。

公开搜索也支持这个判断：

- [openai/codex#4955](https://github.com/openai/codex/issues/4955) 请求“restart individual MCP servers”能力，仍是开放的 enhancement。
- [openai/codex#7155](https://github.com/openai/codex/issues/7155) 记录了 Windows 上 `Transport closed` 与 MCP stdio/stderr/进程生命周期相关的问题。

也就是说，这不是 AgentCall daemon 内部能单独修好的问题。

## 诊断

```powershell
python scripts\codex_mcp_transport_recovery.py
```

输出会包含：

- daemon health。
- 当前 AgentCall MCP 进程。
- 当前命令的 Codex runtime 祖先进程。
- 可用于强制终止 runtime 的命令。

如果 Windows 进程枚举退回到 `Get-Process` 后备，脚本可能拿不到父进程链，只会列出候选 Codex runtime。此时可以用下面的命令人工确认当前 app-server / runtime：

```powershell
Get-CimInstance Win32_Process |
  Where-Object { $_.Name -match 'codex|agentcall|powershell|python|node' } |
  Select-Object ProcessId,ParentProcessId,Name,CommandLine
```

## 强制重置当前 runtime

只在确认 daemon 正常、MCP tool 持续 `Transport closed` 时使用：

```powershell
python scripts\codex_mcp_transport_recovery.py --kill-runtime <pid> --yes
```

这个命令是 out-of-band 恢复手段，不应由 AgentCall MCP 工具自身调用。

## 预防

v0.8.1 以后，业务工具面由 daemon 提供：

- `GET /api/mcp/tools`
- `POST /api/mcp/call`

因此 route、summary、ACP 等业务更新应优先只重启 daemon，不重启 MCP stdio bridge。只有 MCP stdio 外壳本身变化时，才需要重启 transport。
