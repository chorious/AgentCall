# Plan Review — v2.3：PTY 两阶段工作流(Plan → Review Gate → Auto Resume)

Reviewer: Opus 4.8 · 日期: 2026-06-04
关联: `plan_review_v2.2_*`、`acp_sop_gate_review_OPUS.md`、`acp_python_vs_rust_OPUS.md`、CHANGELOG `v0.7.1`(hook-aware binding)、`v0.6.1`(single-writer)
状态: 方向认可、应做;有一条承重假设需先 smoke 验证

---

## 0. 一句话结论

v2.3 的价值不是"切 mode",而是**让 Codex 在 PTY 长任务里拥有一个强制的 plan gate**。
实现形态应是 **PTY 两阶段生命周期**(plan PTY → 显式验收 → auto PTY resume),
而**不是**在同一个 TUI 里模拟按键切 mode——后者 CLI 根本没给程序化 handle,只会脆。

---

## 1. 本机 CLI 事实(已验证)

`claude --help` 实测确认:

- `--permission-mode <mode>`：**启动期** flag,choices 含 `plan | auto | acceptEdits | bypassPermissions | default | dontAsk`。
- `-r, --resume [value]`：按 **session id** 恢复会话。
- `--session-id <uuid>`：指定会话使用的 session id。
- `-c, --continue`：恢复 **cwd 内最近一个** 会话。
- `--fork-session`：resume 时创建新 session id。
- 注记:resume 后的自动续跑相关行为带 "only works with `--print`" 提示 → **交互式 resume 只加载历史并等待输入,不会自动接着干**。

**关键推论**：CLI 没有"对运行中会话结构化 set mode"的接口。`--permission-mode` 只在启动生效。所以 plan→auto 的切换,程序化、可记录、可验收的唯一稳路是 **restart + resume**,而非 TUI 内部状态切换。

---

## 2. 推荐方案：Plan PTY → Review Gate → Auto PTY Resume

```
agentcall_route(runtime=pty, pty_workflow=plan_then_auto)

[plan 阶段]
  daemon mint <uuid>
  claude --permission-mode plan --session-id <uuid>
  prompt: 只产出计划,不改文件
  防线(两层,见 §4):plan 模式默认只读 + PreToolUse deny hook 拦 Write/Edit/MultiEdit
  收尾信号:hook 捕获 ExitPlanMode → plan_ready,payload 落成 plan_report 工件

[Review Gate]
  Codex 读 llm_summary + clean_tail + plan_report 验收
  不清楚 -> session_send action=revise_plan -> plan_needs_revision
  通过   -> session_send action=approve_plan -> daemon 记 plan_accepted

[auto 阶段]
  approve 后:flush 确认 -> kill plan PTY(持久化会话仍在)
  claude --permission-mode auto --resume <uuid>
  daemon 主动踢 prompt: "执行上面已批准的计划"(复用 v2.2 prompt-submit gate)
  -> auto_running
```

---

## 3. 改法 1(最重要)：session id 自铸,消除发现期竞态

原方案从 hooks/runtime_binding/transcript **事后反查** Claude session id 再 resume。问题:这是**多 agent 并发控制面**,反查存在**发现期竞态**——必须等 plan PTY 先产出 transcript/hook 才知道 id,这个"等 + 认领"本身就是 race(同 `v0.6.1` 关 single-writer gap 时的同类问题)。

**改法**：daemon 开工前自己 mint 一个 UUID,启动即钉死。

```
plan:  claude --permission-mode plan --session-id <uuid>
auto:  claude --permission-mode auto --resume <uuid>
```

id 在手、启动前已知 → **完全消除发现期竞态**,且无需解析 transcript。这是对原方案最实质的提升。

- ❌ **否掉 `--continue`**：恢复"最近一个"在并发下选错对象是必然 → 坚决用 `--resume <显式id>`。
- `--fork-session`：若 auto 阶段想要独立 audit id 同时继承上下文,可 fork;首版同 id 续接亦可。实测时一并验。

---

## 4. 改法 2：plan 阶段禁写——机制是 PreToolUse deny hook,分两层

PTY 与 ACP 执行基底不同,必须点名 daemon 拿什么强制:

- ACP：daemon 坐在 JSON-RPC permission 通道,直接拒。
- **PTY：daemon 不在 Claude 工具授权路径上**。唯一强制杠杆是**既有 hook 系统**(`/api/hooks/ingest` + `crates/agentcall-hook`)。

**两层防线**：
1. **默认层**：`--permission-mode plan` 本身只读,Claude 在 plan 模式不落盘。
2. **保证层**：**PreToolUse hook 在 plan phase 对 `Write/Edit/MultiEdit` 返回 deny**。这是"不信任 vendor"的兜底——即使将来某版 plan 模式行为变化,hook 仍拦得住。

> 原则与 `acp_sop_gate_review_OPUS.md` 的"牙齿"一致:强制下沉到执行层,不停留在"相信 Claude 不会写"。

---

## 5. 改法 3：plan_ready 信号用 ExitPlanMode,不 scrape clean_tail

`plan_ready` 的检测不能靠扫 `clean_tail` 文本(又脆又回到 TUI 文本依赖)。

Claude 收尾计划会调 **`ExitPlanMode` 工具**呈现 plan。用 hook 捕获它:

- 结构化信号 → 触发 `plan_ready`;
- payload = 计划正文 → 落成 **`plan_report` 工件**(写 `report_path`)。

Codex 验收对着这个**稳定工件**,而非滚动 TUI 文本。满足"结构化"诉求 + "对契约验收、不对自评/文本"的原则。

---

## 6. 流程补洞

1. **resume 后必须主动踢 prompt**。交互式 resume 只加载历史并等待(§1 的 `--print` 注记)。auto PTY 起来后 daemon 须发"按已批准计划执行" → **复用 v2.2 的 PTY 自动提交 + 等 UserPromptSubmit + 三态返回**。**v2.3 依赖 v2.2 的 prompt-submit gate**。
2. **plan PTY 一次性,持久化会话才是交接物**。`--resume` 读落盘状态,不依赖 live 进程。approve 后可直接 kill plan PTY。唯一要确认:**kill 前会话已 flush 到盘**(Stop hook 触发通常意味已持久化)——实测验时序。
3. **补终态/失败态**。原状态枚举缺收尾,至少加:
   - `resume_failed`(resume 是新引入的最大失败点,需独立状态,勿混进 `failed`)
   - `auto_completed`
   - `failed`

---

## 7. 需新增的最小能力(在原提案上补强制机制)

- `agentcall_route` 新增参数:
  - `pty_workflow: normal | plan_then_auto`
  - `initial_permission_mode: plan | auto | default`
- `agentcall_session_send` 新增 action:
  - `revise_plan` · `approve_plan` · `start_auto`
- daemon 内部(非 MCP 参数):
  - 自铸 `claude_session_id`(UUID),记入 route/runtime_binding。
  - plan phase PreToolUse deny 策略(Write/Edit/MultiEdit)。
  - ExitPlanMode hook → plan_ready + plan_report 工件。
- board/summary 状态:
  - `plan_running | plan_ready | plan_needs_revision | plan_accepted | auto_starting | auto_running | resume_failed | auto_completed | failed`

---

## 8. 首版不做(承接原提案,赞同)

- ❌ 同一 PTY 内模拟按键切 mode 作主路径。
- ❌ 只靠 prompt 说"请进入 auto mode"。
- ❌ plan phase 允许写文件。
- ❌ 自动接受 plan;必须 Codex 或用户显式 accept。
- ❌ 与 ACP SOP gate 混在一起。

---

## 9. 与 ACP SOP gate 的关系

**作为 feature 分开,作为原则统一。**

| | ACP SOP worker | PTY plan_then_auto |
|---|---|---|
| 定位 | 轻量、有界、报告驱动 | 复杂、长生命周期、需 steering |
| 强制基底 | JSON-RPC permission 通道 | PreToolUse hook + permission-mode |
| 共同原则 | 硬 gate + 显式 approve + 不信任 agent 自律 | 同 |

控制面因此有一致心智模型,而非两套各玩各的。

---

## 10. 承重假设：先 smoke,再投入

整套设计压在一条假设上:

> **resume 一个 `plan` 模式启动的会话、但用 `--permission-mode auto`,新模式能生效,且 plan 上下文完整继承,Claude 不卡在"等批准计划"的内部 UI 状态。**

flag 齐备、机制合理,但 mode 跨 resume 能否覆盖是经验问题。

**建议**:先写一个 ~20 行脚本验这一条(plan 起 `--session-id` → 产计划 → kill → `--resume --permission-mode auto` → 踢 prompt → 观察是否真执行、是否禁写),**通过后再投入状态机与 session_send 新 action**。若不成立,v2.3 形态需变。

---

## 11. 待拍板

- [ ] 自铸 `--session-id` vs 事后反查:确认前者(本 Review 强烈建议前者)。
- [ ] auto 阶段同 id 续接 vs `--fork-session`(audit lineage 取舍)。
- [ ] plan_report 工件路径与 schema(对齐 SOP gate 的 report 契约风格)。
- [ ] PreToolUse deny 的判定键:按 route 注入的 `phase=plan` 还是按 session binding 状态。
- [ ] §10 smoke 由谁先跑(建议先行,夯实承重假设)。
