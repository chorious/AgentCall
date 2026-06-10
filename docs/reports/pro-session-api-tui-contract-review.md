有，而且意见比较明确：**不要靠“让 TUI 少显示一点”解决根问题。根问题是 `agentcall_session` 的单次返回契约太粗，把 supervisor 摘要、终端回放、事件审计、hook raw payload、policy debug 全塞进一个 JSON 字符串。** 你贴的样本里一次 `agentcall_session` 调用了 `include=["summary","clean_tail","events"]`、`limit=20`，但返回内容同时包含乱码化的 `clean_tail`、18 条完整事件、hook raw、prompt 全文、tool output、policy_block、Write content 等，已经不是 TUI 能舒服消费的形态了。 

我建议按 **“三层视图 + 两种 payload + 一个 cursor”** 重做。

---

## 1. 先把 Session API 分层，不要一个 include 管所有

现在 `agentcall_session` 的 schema 允许 `include=["summary","clean_tail","plan","events","artifacts","policy","metrics","debug"]`，实现里只要 include 了 `clean_tail` 或 `events`，就会把额外信息塞进同一个 response。  这个设计早期方便调试，但到了 TUI 阶段会变成负担。

我会拆成这几个固定 profile：

```text
agentcall_session(view="summary")
agentcall_session(view="tui")
agentcall_session(view="events")
agentcall_session(view="debug")
agentcall_session(view="raw")
```

默认必须是 `summary` 或 `tui`，而不是自由组合 include。

### `summary`：给 Codex/LLM 控制面

只返回可决策字段，不带 tail，不带 raw events。

```json
{
  "session": "rvk-par-vic2-import",
  "liveness": "working",
  "attention": "blocked_by_policy|needs_permission|none",
  "phase": "execute",
  "last_progress": {
    "brief": "PreToolUse denied Write outside allowed paths",
    "age_seconds": 3,
    "seq": 18
  },
  "next_action": "wait|select_option|interrupt|fix_policy|request_report",
  "report_ready": false,
  "projection_seq": 18,
  "warnings": []
}
```

这个 response 应该稳定在 **1–3 KB**。

### `tui`：给面板

只返回 TUI 直接画得出来的聚合状态，不返回 raw payload。

```json
{
  "session": "rvk-par-vic2-import",
  "header": {
    "status": "working",
    "attention": "policy_blocking",
    "cwd": "D:\\guKimi",
    "target_workspace": "E:\\GameProject\\RKV\\.agentcall-parallel\\vic2"
  },
  "activity": [
    {"seq": 18, "kind": "denied_write", "text": "Write denied: backend/internal/vic2/domain.go"},
    {"seq": 17, "kind": "denied_write", "text": "Write denied: backend/internal/vic2/doc.go"},
    {"seq": 14, "kind": "tool_batch", "text": "Bash denied: non-read-only command"}
  ],
  "blocks": [
    {
      "kind": "policy",
      "reason": "write outside allowed paths",
      "target": "backend/internal/vic2/domain.go",
      "repeat_count": 1,
      "recommended_action": "fix_policy_or_interrupt"
    }
  ],
  "counters": {
    "events": 18,
    "denials": 5,
    "files_written": 0
  }
}
```

这个 response 应该稳定在 **5–10 KB**，最多 20 KB。

### `events`：给审计列表

只返回事件 envelope + compact data，不返回 raw。

```json
{
  "session": "...",
  "cursor": 38773,
  "next_cursor": 38793,
  "events": [
    {
      "seq": 38793,
      "session_seq": 18,
      "type": "hook.PreToolUse",
      "severity": "warning",
      "summary": "Write denied: backend/internal/vic2/domain.go",
      "tool": "Write",
      "decision": "denied",
      "target": "backend/internal/vic2/domain.go",
      "raw_ref": "event:evt-038793"
    }
  ]
}
```

### `debug` / `raw`：给排查，不给默认 UI

只有显式打开 debug 才返回 raw payload，并且必须分页、按 event id 查，不要一口气塞 20 条完整 raw。

```text
agentcall_event(event_id="evt-038793", detail="raw")
agentcall_event_payload(event_id="evt-038793", field="raw.tool_input.content")
```

你现在这个样本的问题就在这里：普通 session 请求把 `UserPromptSubmit` 的 prompt 全文、`PostToolBatch` 的 tool output、多个 `Write` 的 content 全部带出来了，导致单条 JSON 巨大，而且其中不少内容对 TUI 没有直接价值。 

---

## 2. 事件要做“projection event”，不要直接把 hook raw 给 UI

现在 `session_events` 会从 store 取 event，然后把 `event.payload` 作为 `data` 原样塞回 response。 这就是单次 JSON 变重的根因。

应该在事件写入或读取时生成一个轻量版：

```rust
struct EventListItem {
    event_id: String,
    global_seq: u64,
    session_seq: Option<u64>,
    event_type: String,
    severity: String,
    ts: String,
    summary: String,
    category: EventCategory,
    tool_name: Option<String>,
    decision: Option<String>,
    target: Option<String>,
    raw_ref: Option<String>,
}
```

转换规则可以很实用：

```text
hook.PreToolUse + decision.allowed=false
→ kind=policy_denial
→ summary="Write denied: backend/internal/vic2/domain.go"
→ target=normalized relative path
→ raw omitted

hook.PostToolBatch
→ kind=tool_batch
→ summary="Bash completed/denied: pwd && ls -la"
→ stdout omitted, artifact_ref if large

command.accepted/completed
→ kind=command
→ summary="SendInput accepted/completed"
→ payload omitted

hook.UserPromptSubmit
→ kind=prompt_submit
→ summary="Prompt submitted, 1616 chars"
→ prompt omitted by default
```

你样本里最有价值的信息其实只有几条：worker 在 `D:\guKimi` 跑，目标 workspace 是 `E:\GameProject\RKV\.agentcall-parallel\vic2`；多次 Write 被 policy 拒绝，原因是 `PTY path policy denies write outside allowed_paths or writable_paths`；summary 仍显示 `attention_status: none`，这明显不对，因为 worker 已经连续被 policy 拒绝。  

这里有一个很关键的产品判断：**TUI 不应该看 raw events 自己判断阻塞，daemon 应该把阻塞投影成 `attention_status=blocked_by_policy`。** 现在 summary 里 `needs_attention=false`、`attention_status=none`，但 events 里明明已经有多个 policy deny，这是 projection 层没有把事件折叠成可行动状态。

---

## 3. TUI 面板应该做“信息预算”，不是日志查看器

TUI 主屏不要显示 JSON，不要显示 raw hook，不要显示完整 terminal tail。它应该按信息优先级显示：

```text
[rvk-par-vic2-import] working · blocked_by_policy? · seq 18
cwd: D:\guKimi
target: E:\GameProject\RKV\.agentcall-parallel\vic2

Last activity:
  20:05:15  DENIED Write backend/internal/vic2/domain.go
  20:05:08  DENIED Write backend/internal/vic2/doc.go
  20:05:04  DENIED Write .gitignore
  20:05:03  DENIED Write go.mod

Problem:
  Write target resolved under D:\guKimi\.agentcall\workspaces...
  Policy expects allowed/writable path containment differently.

Recommended:
  fix path normalization / scratch root, then inject supervisor instruction
```

主面板最多放：

```text
5 条 recent activity
1 条 current blocker
1 条 next recommended action
少量 counters
```

详细内容走按键展开：

```text
e = events
r = raw event
t = terminal tail
p = policy
o = output artifact
```

也就是说 TUI 应该是 **dashboard-first, drilldown-second**。现在你的 API 是 **dump-first, filter-later**，这会把 TUI 做得很累。

---

## 4. clean_tail 要分成 “screen tail” 和 “text tail”

你这次的 `clean_tail.clean_output` 基本不可读：大量 `✢`, `✶`, `✻`, 单字符换行，像是 Claude TUI 的动画/布局字符被清洗坏了。样本里 decoder health 还显示 invalid/replacement 都是 0，说明不是 UTF-8 解码错误，而是“终端语义清理”没做对。

建议不要再把这个字段叫 `clean_tail`，拆成两个：

```text
terminal_tail_raw       原始/近原始，给 debug
terminal_screen_snapshot 经过 ANSI/alternate screen 解释后的屏幕快照
semantic_tail           从 hooks/transcript/projection 生成的人类可读摘要
```

TUI 默认用 `semantic_tail`，不要用 `clean_output`。`clean_output` 这种字段只能给 debug tab。

如果要继续做 terminal 清洗，正确路线是引入一个 VT parser/screen model，把 PTY 输出当终端控制流解释，维护一个虚拟屏幕 buffer，然后导出最后 N 行。不要用简单字符串过滤去处理 Claude TUI。

---

## 5. 单次 JSON 要有硬预算和“降级响应”

MCP bridge 现在已有输出 cap：`TOOL_TEXT_CAP_BYTES=128KB`，preview 是 16KB。 超过后会返回 truncated preview。 这个方向对，但它是在 MCP bridge 层最后兜底；真正应该在 daemon API 层就控制 payload。

我建议在 daemon 层加：

```text
max_response_bytes 默认 32KB
max_event_items 默认 10
max_event_data_bytes 默认 1024
max_tail_bytes 默认 4096
raw=false 默认
```

返回时带 budget metadata：

```json
{
  "budget": {
    "max_response_bytes": 32768,
    "estimated_bytes": 12400,
    "truncated": false,
    "omitted": {
      "raw_events": 18,
      "tool_outputs": 3,
      "tool_inputs": 5
    }
  }
}
```

如果超预算，不要让 MCP bridge 才截断整个 JSON；daemon 应该语义化降级：

```json
{
  "events": [
    {
      "event_id": "evt-038793",
      "summary": "Write denied: backend/internal/vic2/domain.go",
      "raw_omitted": true,
      "raw_ref": "/api/events/evt-038793/raw"
    }
  ]
}
```

这样 TUI 和 LLM 都知道“省略了什么、怎么取”，而不是拿到一个巨大字符串或一个不可用 preview。

---

## 6. 你这个样本还暴露了一个比 JSON 更严重的问题：路径语义错了

worker 的 route 目标 workspace 是：

```text
E:\GameProject\RKV\.agentcall-parallel\vic2
```

但 Claude 实际 cwd 是：

```text
D:\guKimi
```

样本里 worker 写的是：

```text
D:/guKimi/.agentcall/workspaces/rvk-par-vic2-import/...
```

然后 policy 用 writable paths：

```text
.agentcall/workspaces/rvk-par-vic2-import
report.md
.
```

却把 `D:/guKimi/.agentcall/workspaces/...` 判成 outside allowed paths。 

这说明 TUI 需要专门显示 **path diagnosis**，不然你只看到“worker 不动 / 写不了”，但真正问题是 cwd/root/containment 的坐标系不一致。

我建议 TUI 对 policy denial 做一个专用卡片：

```text
Policy denial
  tool: Write
  target: D:/guKimi/.agentcall/workspaces/rvk-par-vic2-import/go.mod
  route target workspace: E:/GameProject/RKV/.agentcall-parallel/vic2
  claude cwd: D:/guKimi
  allowed writable:
    - .agentcall/workspaces/rvk-par-vic2-import
    - report.md
    - .
  diagnosis:
    target resolved under claude_workspace, but policy compare likely uses route workspace
```

这个比 raw JSON 有价值太多。

---

## 7. 推荐的 API 改造草案

我会加三个新 endpoint/tool，不急着删旧的。

### `agentcall_session(view="tui")`

给 TUI 主面板：

```json
{
  "name": "rvk-par-vic2-import",
  "status": {
    "liveness": "working",
    "attention": "blocked_by_policy",
    "phase": "execute",
    "report_ready": false
  },
  "paths": {
    "claude_cwd": "D:\\guKimi",
    "target_workspace": "E:\\GameProject\\RKV\\.agentcall-parallel\\vic2"
  },
  "last_activity": [...],
  "current_blocker": {...},
  "next_action": "fix_path_policy_or_interrupt",
  "cursors": {
    "event": 38793,
    "session": 18
  }
}
```

### `agentcall_session_events(compact=true, cursor, limit)`

给事件列表：

```json
{
  "events": [
    {
      "seq": 18,
      "type": "hook.PreToolUse",
      "kind": "policy_denial",
      "summary": "Write denied: backend/internal/vic2/domain.go",
      "severity": "warning",
      "raw_ref": "evt-038793"
    }
  ],
  "next_cursor": 38793
}
```

### `agentcall_event(id, detail="raw|payload|tool_output")`

给 drilldown：

```json
{
  "event_id": "evt-038793",
  "raw": {...}
}
```

旧的 `include=["events"]` 保留，但标成 debug/deprecated，默认只返回 compact events。

---

## 8. TUI 过滤规则可以先硬编码，别过早抽象

先做这几个 filter 就够：

```text
All
Attention
Policy
Permission
Progress
Commands
Reports
Debug
```

每个 filter 的逻辑：

```text
Attention:
  severity >= warning
  OR decision.allowed=false
  OR attention_status != none
  OR report_ready=true
  OR session ended/failed/orphaned

Policy:
  hook.PreToolUse with decision.allowed=false
  policy_denial.blocked
  policy_denial.guidance_injected

Progress:
  UserPromptSubmit
  PostToolBatch summarized
  report_ready
  files_written_count changed

Commands:
  command.accepted
  command.completed
  command.awaiting_observation
```

并且 TUI 默认只展示 `Attention + Progress`，不要默认展示 Commands 和 raw Hooks。命令 accepted/completed 对开发调试有价值，对监督 worker 基本是噪音。

---

## 9. 一条很硬的建议：不要让 LLM 读 TUI JSON

TUI 数据和 LLM 控制数据要分开。LLM 需要的是：

```text
现在是否该干预？
为什么？
下一步做什么？
需要哪个 precondition/cursor？
```

TUI 需要的是：

```text
让我人眼快速看懂 worker 在哪、卡在哪、最近发生什么。
```

Debug 需要的是：

```text
完整 raw event / hook payload / tool input output。
```

这三者不是同一个 payload。现在 `agentcall_session` 同时服务三者，所以才重。

---

## 10. 最小落地方案

不大改架构的话，我建议你先做这 5 个 patch：

1. `agentcall_session` 默认只允许 `summary`，`events` 默认 compact，不带 `data.raw`。
2. 增加 `detail: "compact" | "full"`，默认 compact；只有 `detail=full` 才返回 raw。
3. `session_events` 里把 `event.payload` 改成 `event_summary(payload)`，raw 只给 `raw_ref`。
4. `summary` projection 把 policy denial 折叠成 `attention_status=blocked_by_policy`，不要继续显示 `none`。
5. TUI 主屏只调用 `view=tui`，不要调用 `include=["clean_tail","events"]`。

这几个改完，单次 JSON 会从“几十 KB 到上百 KB、还包含一堆 escaped raw”降到“几 KB 的可画状态”。更重要的是，TUI 会从日志浏览器变成控制面板。
