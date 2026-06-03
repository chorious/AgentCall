# Plan — Hook-Aware Summary Binding (v0.7.1)

作者: Codex + Opus 4.8 · 日期: 2026-06-03
前置: v0.7 已落地 readable-wrapper 层(流式 UTF-8 decoder / clean_replay / llm_summary / MCP 25→14)。本补丁解决 v0.7 **未闭合的一块**:`session_summary.status` 没有把 hook 事件作为状态主源。

---

## 1. 问题

v0.7 的 plan 原则是:

```
hook/daemon 结构化事件  >  report/file validator  >  TUI extractor
```

但当前实现里 `session_summary.status` 实际来自:

```
daemon session exited/error
+ clean_output 里 grep: interrupted / reports generated / tasks completed / waiting for input
```

也就是说,hook 事件(`hook.Stop` / `hook.Notification` / `hook.SubagentStop`)虽然已经进了 daemon state/events,但 **summary 没把它们当状态主源合并进来**。§4 的"信 hook > 信 TUI"没有真正闭合。

### 根因:两套 ID 没有桥

- hook 事件按 **Claude 的 `session_id`** 入库(`crates/agentcall-daemon/src/hooks.rs:54-58`,落到 `active_sessions[session_id]`)。
- PTY wrapper session 按 **wrapper `name`**(如 `androidpet-v11`)存。
- 两者之间没有绑定 → hook 事件无法可靠归到某个 PTY wrapper session → summary 只能退回读屏。

---

## 2. 核心决策:join key 用注入的 env,不用 cwd

这是本补丁最关键、且最容易在实现时翻车的决策,先钉死。

| 候选 key | 可行性 | 理由 |
|---|---|---|
| **cwd** | ❌ | `is_claude_command → force_claude_workspace`(默认 `D:\guKimi`)把所有 Claude 强制塞进同一 workspace。多个 wrapper → 同一 cwd,无法区分(两个 `androidpet-v11` cwd 完全一样)。 |
| **claude session_id** | ⚠️ 仅作反向索引 | spawn 时未知,只能等第一个 hook 习得,不能作指派主键。 |
| **transcript_path** | ⚠️ 衍生 | 唯一但仍需先有 binding 才能映回 wrapper name。 |
| **注入 env `AGENTCALL_WRAPPER_SESSION`** | ✅ 主键 | spawn 时指派,第一个 hook 起即成立,免疫 cwd 碰撞与 session_id 未知。daemon `session.rs:113` 已在注入 env,加一行即可。 |
| PID 父链 | ❌ | Windows 下脆弱,放弃。 |

**结论:env 当主键,claude session_id 当反向索引兜底。**

---

## 3. runtime_binding schema

daemon 维护一张表,每个带 env 的 hook 刷新它:

```jsonc
// .agentcall/state/runtime_binding.json
{
  "androidpet-v11": {
    "wrapper_session": "androidpet-v11",
    "claude_session_id": "abc123-...",     // 第一个 hook 习得
    "transcript_path": "C:/.../abc123.jsonl",
    "cwd": "D:/guKimi",
    "child_pid": 4567,
    "last_seen": 1717400000000
  }
}
```

反向索引(内存即可,可由上表派生):

```
claude_session_id -> wrapper_session   // 兜底个别未带 env 的 hook
```

有了这张表,4 级来源的第 2/3 级才有可靠 join。

---

## 4. 改动点(三处,各几行)

### 4.1 daemon spawn 注入 env
`crates/agentcall-daemon/src/session.rs:113`(已有 env 注入块):
```rust
command.env("AGENTCALL_WRAPPER_SESSION", &req.name);
```

### 4.2 hook 脚本回传 env
`scripts/agentcall-claude-hook.py`(已经在 POST payload):
```python
wrapper = os.environ.get("AGENTCALL_WRAPPER_SESSION")
if wrapper:
    payload["wrapper_session"] = wrapper
```
(`agentcall-codex-hook.py` 同理,若 Codex 也走 wrapper。)

### 4.3 daemon ingest 写 binding
`crates/agentcall-daemon/src/hooks.rs::ingest_hook`(已解析 session_id/cwd/transcript_path):
- 读 `payload["wrapper_session"]`。
- upsert `runtime_binding[wrapper_session] = { claude_session_id, transcript_path, cwd, child_pid, last_seen }`。
- 同步反向索引。

---

## 5. status 重写:加 `status_source` + 按维度分源(非线性优先级)

### 5.1 拆字段,让优先级可审计
当前 status 是混合来源压出的单一 enum。改为:

```jsonc
{
  "status": "waiting_input",
  "status_source": "tui",        // hook | lifecycle | report | tui
  "confidence": 0.82,
  "needs_attention": true
}
```

Codex 一眼看到 `status_source: tui` + 低 confidence,就知道这是弱信号,无需猜来源。零成本、高价值。

### 5.2 按维度分源(对 Codex "linear precedence" 的修正)

不是"hook 永远赢",而是**不同维度信不同源**:

| 维度 | 权威源 | 说明 |
|---|---|---|
| liveness / lifecycle(working / stopped / subagent done)| **hook** | `PreToolUse`/`PostToolUse`=working;`Stop`=idle/awaiting;`SubagentStop`=子代理完成 |
| 任务/报告产出(report_ready)| **report/file validator** | 读 `.agentcall/tasks` report 文件,比 grep "reports generated" 可靠 |
| "屏幕在问人话"(plan-mode 提问 / 自由文本 prompt)| **TUI** | 很多**不触发任何 hook**;`Notification` 只覆盖 permission 提示。此维度 TUI 是合法主源,不是兜底 |
| 解码健康 | decode_health | 已有 |

实现上 `session_summary` 解析顺序:
```
1. 经 runtime_binding 取 claude_session_id
2. 读该 session 的 hook lifecycle (active_sessions / recent events) -> liveness
3. 读 report/file validator -> report_ready
4. clean_output regex -> 仅补 hook 不发的交互提问(waiting_input 等)
每条结论标注 status_source + confidence
```

> 关键提醒:别因为"信 hook"就把 waiting_input 也交给 hook 判 —— 那会漏判 hook 不发的提问。liveness 信 hook,交互提问仍信 TUI。

---

## 6. 动手前先实测 env 继承(必做)

整套绑定押在"Claude(Node)把 env 传给 hook 子进程"上。Node 默认会传、Windows 下大概率成立,但**便宜且必须先验**:

1. 临时在 spawn 注入 `AGENTCALL_WRAPPER_SESSION=probe-1`。
2. 跑一个会触发 hook 的最小 Claude 操作。
3. 检查 ingest 收到的 payload 是否含 `wrapper_session=probe-1`。
4. 顺带验 `SubagentStop`:子代理 hook 是否也带同一个 env(预期带,归到父 wrapper)。

**通过再建整张表;不通过则改用 transcript_path 学习 + session_id 反查的次优方案。**

---

## 7. 范围

- 独立补丁 **v0.7.1**(或 v0.8),**不回灌 v0.7**——v0.7 readable-wrapper 层已自洽(编译 + 7/7 测试绿),避免污染。
- scope 锁三件事:env 绑定 → `runtime_binding` 表 → `session_summary` 按分源重写 status。
- 不在本补丁里做:Claude 生命周期销毁、session 续接、双写一致性(沿用 v0.7 非目标)。

---

## 8. Test Plan

- **env 继承**: §6 实测脚本化为 fixture/手测记录。
- **binding**: 同一 wrapper 多次 hook,`runtime_binding` 只一条且 `claude_session_id` 稳定;两个不同 wrapper 不串。
- **分源 status**:
  - 注入 `Stop` 事件 → status_source=hook, status=idle/awaiting。
  - 注入 report 文件 → status_source=report, report_ready。
  - clean_output 含 plan-mode 提问且无对应 hook → status_source=tui, waiting_input, needs_attention。
  - 无任何信号 → confidence 降级(沿用 v0.7 的 0.2 空输出语义)。
- **回归**: `cargo test -p agentcall-daemon -p agentcall-mcp`;`python -m pytest -q`;hook 脚本 compile/smoke。

---

## 9. 待拍板

- [ ] §6 env 继承实测:先验后建,还是直接建表带兜底?
- [ ] `status_source` 字段是否纳入(建议纳入)。
- [ ] 维度分源表(§5.2)是否作为最终 status 规则,取代线性优先级。
- [ ] 版本号:v0.7.1 还是 v0.8。
