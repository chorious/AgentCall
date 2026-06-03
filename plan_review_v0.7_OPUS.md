# Plan Review — AgentCall v0.7 (Opus)

Reviewer: Opus 4.8 · Date: 2026-06-03
Scope: 评估 v0.7 plan（Readable Wrapper + Low-friction Codex Control），基于对 `crates/agentcall-daemon`、`crates/agentcall-mcp`、`src/agentcall` 当前源码的实读。

---

## 1. 总体判断

**保留方向，但需要三处修正再开工。**

- ✅ 非目标清单（不做生命周期销毁 / 不做 60s grace / 不做双写一致性 / 不删 Python）是这份 plan 最大的优点，克制得当。
- ✅ "更新后旧 daemon/viewer/Claude PID 不吃配置，必须重启全套" 是少见但正确的运行态诚实声明。
- 🔴 **P0 的 UTF-8 根因判断错误**：env 注入修不了实际乱码（详见 §2）。
- 🔴 **P3 工具是净增而非收敛**：与现有 ~25 个 MCP 工具大面积重叠，违反项目自身 CLAUDE.md「Simplicity First」（详见 §3）。
- 🟡 **P2 TUI 规则提取器**高投入低耐久，需明确失败语义并优先信 hook 而非 TUI 文本（详见 §4）。

---

## 2. P0 UTF-8：根因判断错误（必须修正）

### 2.1 现象已确认

用户实测：**viewer 的 Claude TUI 输出花屏**（间歇性闪现的乱码点，非全乱）。

### 2.2 plan 的 env 方案对此无效

plan 要注入 `PYTHONUTF8=1 / PYTHONIOENCODING=utf-8 / LANG=C.UTF-8 / LC_ALL=C.UTF-8`。

问题：这四个全是 **Python + glibc** 取向的变量。

- `PYTHONUTF8` / `PYTHONIOENCODING` 只对 **Python 子进程**生效——仅救正在降级的 legacy 路线。
- `LANG` / `LC_ALL` 在 **Windows 原生进程上是 no-op**。
- Claude Code CLI 是 **Node/Ink TUI**，上述变量一个都管不到它。

**结论**：env 注入不应号称 "UTF-8 全链路"，它只对 legacy Python 路线声明有效。

### 2.3 真正根因：`from_utf8_lossy` 的 chunk 边界

输出链路有三个 lossy 解码点：

| 位置 | 场景 | 是否真凶 |
|---|---|---|
| `crates/agentcall-daemon/src/session.rs:181` | **实时流**，每 8KB chunk 独立 `from_utf8_lossy` | 🔴 **就是它** |
| `crates/agentcall-daemon/src/http.rs:215` | SSE replay，对整个 buffer 一次性解码 | 🟢 基本无害\* |
| `crates/agentcall-daemon/src/http.rs:261` | WS replay，同上 | 🟢 基本无害\* |

\* replay 对完整 buffer 一次解码，内部字符不会被切；唯一风险是 `REPLAY_LIMIT` 的 `drain`（`session.rs:175-177`）按裸字节裁剪可能切坏开头第一个字，可忽略。

**机理**：`spawn_reader`（`session.rs:166-181`）用固定 `[0u8; 8192]` 缓冲，每个 chunk 独立 `String::from_utf8_lossy`。TUI 不停重绘 → 8KB 读边界命中频繁 → 跨界的中文 / box-drawing 字符前半截变 `U+FFFD` → 间歇性花屏。这正是 chunk 边界的特征，与终端、与 env 都无关。

注意：`replay`（`session.rs:173-174`）**已经保存 raw bytes**，所以 plan 说的"保留 raw bytes"大部分已完成；缺的不是存储，是**流式解码器**。

### 2.4 修复（外科手术级，约 15 行）

在 `spawn_reader` 维护 `pending: Vec<u8>` 跨 read 缓存不完整尾字节：

```rust
let mut pending: Vec<u8> = Vec::new();
// loop 内拿到 bytes 后：
pending.extend_from_slice(bytes);
let valid_up_to = match std::str::from_utf8(&pending) {
    Ok(_) => pending.len(),
    Err(e) => e.valid_up_to(),
};
let data: String = String::from_utf8(pending.drain(..valid_up_to).collect()).unwrap();
if data.is_empty() { continue; } // 整块都是半个字符，等下一块
// broadcast(data) ...
```

- `replay`（raw bytes）照旧存，不动。
- `decode_health` 统计 `pending` 长期未清空 / 真正非法字节次数——这才是有意义的指标，**不是 env 状态**。
- http.rs 的 replay 解码可顺手走同一 helper，但非必须。

### 2.5 P0 重定义建议

| 原 plan | 修正后 |
|---|---|
| env 注入 = UTF-8 全链路 | env 注入仅声明对 legacy Python 路线有效 |
| 保留 raw bytes 作为状态源 | 已部分完成；**核心交付改为流式 UTF-8 decoder** |
| decode_health 记录 decode 健康 | decode_health = decoder 残留/替换字符计数 |

**此修复独立于整个 v0.7，可立即单独落地，是唯一能即时验证 "viewer 不花" 的改动。**

---

## 3. P3 MCP 工具：从净增改为收敛（25 → 13）

### 3.1 现状：~25 个工具，6 个是 `board` 的切片

`summary.rs:30-39` 的 `board_state` 已一次性返回 `pty_sessions / active_sessions / file_claims / transcripts / reports / recent_events / project_state`。下列工具只是它的投影：

**A. 状态只读 —— 全是 `board` 投影 🔴**

| 工具 | 实质 |
|---|---|
| `agentcall_board` | 超集 |
| `agentcall_project_sessions` | = board.pty_sessions/active_sessions |
| `agentcall_session_list` | = board.pty_sessions |
| `agentcall_events_tail` | = board.recent_events |
| `agentcall_reports_list` | = board.reports |
| `agentcall_file_claims` | = board.file_claims |
| `agentcall_transcripts_list` | = board.transcripts |

**B. 单 session 只读 —— 三合一 🔴**
`agentcall_session_summary` / `agentcall_session_status` / `agentcall_session_tail`（summary ⊇ status；tail 仅要输出）

**C. ACP 派工 —— 近乎同 schema 🔴**
`agentcall_delegate_acp` / `agentcall_workflow_simulate`（参数 `root/claude_workspace/max_turns` 几乎一致）；`agentcall_session_spawn` 是裸 PTY，属不同 runtime，保留。

**D. 健康/诊断 🟡** `agentcall_runtime_health` / `agentcall_concurrency_probe`（probe 是 health 的 section）

**E. 静态 schema 🟡** `agentcall_capabilities` / `agentcall_report_schema`

### 3.2 关键：plan 的 5 个新工具应是「合并」不是「新增」

| plan 想要 | 正确实现 | 替代对象 |
|---|---|---|
| `board_compact` | `board` 加 `view:"compact"` | — |
| `attention` | `board` 加 `filter:"attention"` | — |
| `delegate` | 合并 `delegate_acp` + `workflow_simulate` | C 组 |
| `nudge` | `session_send` 扩 `action` 字段 | `session_send` |
| `accept_report` | 与 `checkpoint_request` 合并为 `report` 生命周期工具 | `checkpoint_request` |

plan 原写法会让工具面从 25 → 30，35 个工具反而**加重** Codex 选择负担，与 "low-friction" 自相矛盾。

### 3.3 收敛规划

1. **投影坍缩（A 组，-6）**：删 `project_sessions / session_list / events_tail / reports_list / file_claims / transcripts_list`；`board` 加 `view: full|compact`、`filter: all|attention`、`section: sessions|events|reports|claims|transcripts`。同时交付 plan 的 `board_compact` + `attention`。
2. **单 session 三合一（B 组，-2）**：合并为 `agentcall_session`，`include: [status|tail|summary]`，默认返回扩展 `llm_summary` + 可选 tail。
3. **ACP 派工二合一（C 组，-1）**：`delegate_acp` + `workflow_simulate` → `agentcall_delegate`（保留 `driver: acp|scripted`）；`session_spawn`(PTY) 独立保留。
4. **诊断/schema 各并一个（D+E，-2）**：`concurrency_probe` → `runtime_health` section；`report_schema` → `capabilities` 字段。
5. **控制语义归一**：`session_send` 扩 `action` 吸收 `nudge`；`checkpoint_request` 升级为 `agentcall_report`（request/accept 两 op）吸收 `accept_report`。
6. **`workflow_inspect`**：评估能否并入 `board(task_id)`（保守先留）。

### 3.4 最终保留清单（13）

```
读：  board(view/filter/section) · session(单条) · runtime_health · capabilities
写：  session_spawn(PTY) · delegate(ACP) · session_send(含nudge) · report(request/accept)
规划：route_task · codex_preflight · context_packet_create
摄入：hook_ingest · transcript_index
```

净效果：工具面 **25 → 13**，同时把 plan 的 5 个新功能全部交付——它们变成参数与合并，而非第 26–30 个工具。

---

## 4. P2 TUI 提取器：高投入、低耐久

`llm_summary` 靠规则识别 `waiting input / interrupted / reports generated / tokens / context used / Auto-update failed`——这是**正则解析一个会随版本重排的 Ink TUI**，viewport reflow + ANSI + box-drawing 让它很脆。

要求：

- 每条规则配 fixture（plan 的 Test Plan 已列，✅）。
- **失败语义显式化**：规则未命中时 `confidence` 降级、`needs_attention` 显式置位，**禁止默默给空 headline 让 Codex 误判"没事"**。当前 plan 未写清此点。
- **信 hook > 信 TUI**：`status` 优先采信 hook 事件（稳定接口），TUI 文本仅作补充。plan 已说 hooks 不承担 PTY 主状态判断（对），但 summary 的 `status` 来源优先级要写死。
- 排期建议放在本版**最后**做，避免吃掉工期还不稳。

---

## 5. 做得好的地方（保留）

- 非目标清单：教科书级克制，尤其"不做生命周期销毁 / 60s grace / 双写一致性"。
- "旧 daemon/viewer/Claude PID 不吃配置，必须重启全套"——诚实的运行态约束。
- legacy 降级而非删除（`legacy_detached_sessions` vs `live_daemon_sessions`），务实。
- Test Plan 对 UTF-8 边界、mojibake tail、ANSI noise 有覆盖意识。

---

## 6. 收敛建议（本版砍成两刀）

1. **P0 重定义**：env 注入 ≠ 全链路；核心交付是「流式 UTF-8 decoder + decode_health」，配跨边界 fixture（如 3 字节中文切在 read 边界，断言无 `U+FFFD`）。这是唯一能即时验证"中文不乱码"的真因修复。
2. **P3 先减后加**：按 §3.3 收敛到 13 个工具，净增 ≤ 0。
3. **P2 殿后**：明确"信 hook > 信 TUI"与失败降级语义，再实现 TUI 提取器。

---

## 7. 待用户拍板

- [ ] P0 的流式 decoder 修复是否立即单独落地（与 v0.7 解耦）？
- [ ] P3 是否采纳 25 → 13 收敛清单替换原"新增 5 工具"写法？
- [ ] `workflow_inspect` 是否并入 `board(task_id)`？
