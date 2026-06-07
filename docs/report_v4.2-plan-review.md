# v4.2 Plan 评估报告：Readable TUI Cleanup + Low-Friction Session Control

> 评估日期：2026-06-07  
> 被评估文档：`docs/v4.2-readable-tui-control.md`  
> 评估方式：**对照当前代码逐条核实**（`crates/agentcall-daemon/src/*`、`crates/agentcall-mcp/src/*`、`scripts/agentcall_dev.py`），非仅文档内审。  
> 仓库状态：plan 为未跟踪新增（`?? docs/v4.2-readable-tui-control.md`），基线 HEAD `a8f7224`。

---

## 0. 总评

**这是一份高质量、问题导向、且基本忠于代码现状的工程计划。** 它的核心判断——"policy deny 循环被错误地呈现为健康 `working`，导致 Codex 继续耐心等待"——是**真实存在且可在代码中定位的缺陷**（deny 文案 `hooks.rs:937` 与 summary 的乐观 working 逻辑并存）。原则清晰、Non-goals 收得很紧、实施顺序把"止血"放在最前，都是优点。

主要不足集中在一点：**P0 bounded-write 与当前 path policy 的实际行为存在直接冲突，而 plan 没有点破。** 当前"enforced"模式对非只读 Bash 是**整条拒绝**（`hooks.rs:937`），而 P0 却要求"Bash 可写入 scratch/report"。这不是加一个目录就能实现的，需要重写 Bash 写入的路径级判定。这是落地前必须先解决的设计缺口。其余为范围偏大、字段冗余、几处启发式阈值偏脆等中低风险问题。

**结论：方向正确、可执行，但 P0 需要补一段"如何让 Bash 写入被路径级约束而非整条 deny"的设计；建议据此修订后再开工。**

---

## 1. 现状描述准确性核对（plan「现状」段 vs 代码）

| plan 声称「已完成」 | 代码核实 | 结论 |
|---|---|---|
| `terminal.rs` 流式 UTF-8 decoder | `decode_utf8_stream` @ `terminal.rs:13`，含 split-utf8 测试 | ✅ 属实 |
| `clean_terminal_text` 去 ANSI/压空行 | `clean_terminal_text` @ `terminal.rs:132` | ✅ 属实 |
| `session_summary` 已拆 liveness/attention/confidence/patience | 全部存在 @ `summary.rs:371/438/576`，另有 `status_source`/`patience_status` | ✅ 属实 |
| `session_send` 支持 `select_option`/`interrupt`/queued | action enum @ `mcp.rs:88`；`select_option` @ `mcp.rs:224`，单数字校验 @ `mcp.rs:364`；queued 注入 + PostToolBatch 检测 @ `mcp.rs:319-335` | ✅ 属实 |
| 权限菜单识别 → 阻止盲打自然语言 | hint @ `mcp.rs:315` | ✅ 属实 |
| policy deny「被记录为事件，但重复 deny 没进 attention」 | deny 文案 `hooks.rs:858/922/933/937`；summary 无任何 deny 聚合/`policy_block` | ✅ 属实——缺口确认 |

**核实结论：plan 对"已完成/仍糟糕"的描述高度准确**，未发现夸大或臆造的现状。所引用的 deny 文案 `PTY path policy denies non-read-only bash when allowed_paths are enforced` 与代码**逐字一致**（`hooks.rs:937`），说明 plan 出自真实日志而非想象。

> ⚠️ **唯一的现状遗漏（重要）**：plan「现状」段**完全没提到 route 已有 `allowed_paths` 强制与 `containment` 字段**。事实上 `routes.rs:442-444` 已输出 `containment.mode = prompt_only | enforced`，`summary.rs:521` 已把 `containment.mode` 透传到 summary。因此 P0/P5 的 `containment` **不是从零新增，而是扩展**——这直接影响 P0 的真实工作量与冲突点（见 §3.1）。

---

## 2. 优点

1. **问题定位精准、可复现**：核心缺陷在代码中可定位，不是空想需求。
2. **原则先行**："状态权威仍是 hook/daemon/route/report，TUI 只做可读摘要"贯穿全篇，且 Non-goals（line 352-366）把"不让 TUI 成为权威、不自动放宽权限、不无限正则、不让 Bash 绕过 allowed set"等边界钉得很死，约束力强。
3. **实施顺序合理**：P0 → P1/P2 先止血（消除"健康 working 假象"），再做 P3/P4 清洗管线，符合"先修危害最大的误判"原则。
4. **分层管线清晰**：`raw → decoded → clean → semantic → tui_signals → llm_summary`（line 223-230）层次分明，且 `extract_tui_signals` 明确"不直接改权威状态"（line 236），与原则自洽。
5. **测试可被强制执行**：plan 多处依赖 `python agentcall.py release-check`——已核实该命令真实存在（`agentcall_dev.py:66 → cmd_release_check:145`），且会跑 `cargo test --workspace` + `pytest -q`（line 168/171）。新增 Rust fixture / pytest 用例**会被 CI 门禁实际拉起**，验收标准非空话。
6. **typed interaction model**：把权限/workflow/plan/question 统一成 `interaction.kind`，让 Codex"读结构化字段而非猜 tail"，是正确的抽象方向。

---

## 3. 主要问题与风险（按严重度排序）

### 🔴 3.1【高】P0 bounded-write 与现有 Bash path policy 直接冲突，plan 未点破
- 现状：`enforced` 模式下，对非只读 Bash 是**整条拒绝**——`hooks.rs:937` 返回 `PTY path policy denies non-read-only bash when allowed_paths are enforced`。即"有 allowed_paths"就等于"Bash 不许写任何东西"。
- P0 要求（line 102）：**"Bash 写入/重定向/生成文件只能落到 writable allowed set 内"**——这要求按**路径级**判定 Bash 写入，而非现在的"非只读即拒"。
- **冲突**：bounded_write 想让 Bash 写 scratch，但现有 gate 在该模式下对 Bash 写入是一刀切 deny。仅"自动建 `.agentcall/workspaces/<session>/`"目录**不能**解决——必须**重写 `hooks.rs` 里 Bash 的写入判定逻辑**（解析命令的写目标 / 重定向并与 writable set 比对）。
- 这恰是 plan 自己在 Non-goals（line 366）承认很难的事："不让 Bash 通过重定向/临时目录/shell trick 绕过 allowed set"——但**没说怎么正向实现路径级允许**。
- **建议**：P0 增补一节"Bash 写入的路径级判定"：要么显式声明 bounded_write 下 Bash 仍只读、写入只走 `Write/Edit`（最简单、与现状一致），要么给出命令写目标解析的具体策略与其已知盲区。**这是开工前必须拍板的设计决策。**

### 🟠 3.2【中】范围偏大：一个版本塞了两个主题 + 10 个工作项
- P0–P9 实际横跨两条线：**(A) 可读性清洗**（P3/P4/P5/P6）与 **(B) 会话控制/策略**（P0/P1/P2/P7/P8/P9）。
- summary.rs 已 **1435 行**，是仓库最大模块；P5 再往里塞 `interaction`/`policy_block`/`containment`/`tui_signals` 会进一步膨胀。P4"抽规则表"的直觉正确，但 plan 没说新管线模块落在哪个文件、是否拆分 summary.rs。
- **建议**：明确"(A) 清洗管线"与"(B) 策略/控制"是否可拆成两个可独立合并的里程碑；P0+P1+P2 单独成第一个可发布增量（止血最有价值），其余随后。

### 🟠 3.3【中】P1 deny 聚合的归一化与阈值偏脆
- plan 按 `wrapper_session + tool + normalized_command_or_path + deny_reason` 聚合（line 148），"连续 2-3 次"升格（line 150）。
- 风险一：**任意 Bash 命令的归一化很难**（参数/环境/重定向/路径差异），归一化过松会误并、过紧会漏并。plan 未给归一化规则。
- 风险二：**"2-3 次"是魔数**，且缺少**成功即清零**的明确语义——若 Claude 改对命令后成功，counter 必须立刻复位，否则会残留 `blocked_by_policy` 假阳性。
- 风险三：deny-loop 检测**依赖 daemon 真把每次 deny 当作离散事件落库**（带 session+tool+reason）；当前 `hooks.rs` 只返回 reason 字符串，需确认事件发射链路存在。
- **建议**：P1 补"归一化规则 + 成功/新命令即 reset"两条；阈值设为可配置常量并写明默认值来源。

### 🟡 3.4【低-中】P5 字段冗余：`tui_signals[]` 与 `interaction{}` 重叠
- 两者都携带 menu/question 的 `kind`+`options`+`confidence`（line 273-286）。`tui_signals` 像"多条弱信号"，`interaction` 像"选定的那一条"——语义可区分，但 plan 未说明二者关系（是 `interaction = argmax(tui_signals)`？还是各自独立？）。Codex 同时读两处易混淆。
- 另：管线里叫 `semantic_output`（line 227），summary 字段叫 `semantic_tail`（line 272）——一个是全量、一个是尾部，命名需刻意区分并在文档点明，否则实现期易混。
- **建议**：明确 `interaction` 为 `tui_signals` 的"已裁决投影"，并在 summary 只暴露 `interaction` + 短 `semantic_tail`，`tui_signals` 仅 debug/低置信展开。

### 🟡 3.5【低】`interaction.options[].semantic`（approve/inspect/deny）依赖菜单文案映射，脆弱
- 把"Yes, run it / View raw script / No"映射成语义，依赖 Claude Code 的 UI 文案，**正是 plan 别处反复告警的脆弱点**。
- plan 已合理对冲（P7 line 326："首版只在 summary 暴露 semantic，实际仍发数字"）——保留此对冲即可，但建议在验收里明确"映射失配只降 confidence、不阻断 select_option"。

### 🟡 3.6【低】测试面铺得大，但现有脚手架很薄
- 现仅 2 个 python 测试（`test_sop_flow.py`、`test_v061_hook_daemon_ingest.py`），**均不覆盖 session/policy/select_option/containment**。
- plan 的 Test Plan（line 370-409）新增近 30 条断言，等于新建一整套测试基建。工作量需计入排期，别低估。

---

## 4. 逐项快评（P0–P9）

| 项 | 评价 |
|---|---|
| **P0** 默认 bounded-write | 方向对、价值高，但**与现有 Bash policy 冲突未解决**（§3.1），且 containment 是扩展非新建（§1）。**开工前必须先定 Bash 写入判定。** |
| **P1** deny→可恢复 attention | 最高价值的止血项；需补归一化与 reset 语义（§3.3）。 |
| **P2** deny 后注入纠偏 | 合理；已正确承认依赖 PostToolBatch、否则 pending（line 217）。属"尽力而为"，非保证送达——验收应接受"pending"为合法终态。 |
| **P3** 清洗 pipeline | 分层清晰、可测；需指明新模块落点（勿全压进 summary.rs）。 |
| **P4** 规则表可测试 | 很好的反"无限正则"设计，与 Non-goals 自洽；首版规则集（line 256-264）覆盖真实痛点，务实。 |
| **P5** llm_summary 新字段 | 方向对；存在字段冗余/命名问题（§3.4）。 |
| **P6** MCP 默认返回更小 | 合理且低风险；与"Codex 默认读摘要"原则一致。 |
| **P7** 菜单包装 | typed interaction 是正确抽象；semantic 映射脆弱已对冲（§3.5）。 |
| **P8** queued 可观测性 | 直接补齐现有盲区（`mcp.rs:319` 已 queued 但反馈弱），低风险高收益。 |
| **P9** Board 降噪 | 与原则一致（"不把 idle/Stop 当 attention"）；纯展示层，低风险。 |

---

## 5. 一致性 / 与原则的契合

- **与 Non-goals 高度自洽**：未发现 plan 自身违背"不放宽权限/不让 TUI 成权威/不无限正则/不让 Bash 绕过 allowed set"等红线。
- **与现有代码契合**：`select_option` 单数字、queued+PostToolBatch、permission hint 等均已在 `mcp.rs` 落地，plan 的"统一"是在既有基础上收口，而非推翻重来——迁移风险低。
- **唯一硬一致性问题**：P0「Bash 可写 scratch」(line 102) vs 现实「enforced 下 Bash 写入整条 deny」(hooks.rs:937) 与 Non-goals line 366，三者需要被一段明确设计调和（§3.1）。

---

## 6. 建议（按优先级）

1. **【必做·开工前】** 在 P0 增补"Bash 写入路径级判定"设计，或显式降级为"bounded_write 下 Bash 只读，写入仅经 `Write/Edit`"——消解与 `hooks.rs:937` 的冲突。
2. **【必做】** 在「现状」段补一句：route 已有 `allowed_paths`/`containment{prompt_only|enforced}`；将 P0 重新定位为"扩展为三态 + 自动 scratch"，并据此重估工作量。
3. **【强烈建议】** P1 增加：命令/路径归一化规则、`2-3` 阈值设为可配置常量、**成功或换命令即清零** counter。
4. **【建议】** 拆里程碑：`P0+P1+P2`（止血）作为第一个可独立合并增量；清洗管线（P3-P6）随后。
5. **【建议】** P5 澄清 `interaction` 与 `tui_signals` 的关系（裁决投影），统一 `semantic_output`/`semantic_tail` 命名语义。
6. **【建议】** 给 scratch 目录（`.agentcall/workspaces/<session>/`）定义生命周期/清理策略——plan 目前只建不收。
7. **【建议】** 把"新增测试基建"的工作量显式写入 Implementation Order（现仅 2 个 python 测试、零 session/policy 覆盖）。

---

## 7. 复核方式

- **现状核对**：本报告 §1 的每行可用 `grep -n` 在所列 `file:line` 复验。
- **P0 冲突复验**：阅读 `hooks.rs` 中返回 `denies non-read-only bash when allowed_paths are enforced` 的函数（@937 附近），确认 enforced 模式对 Bash 写入为一刀切 deny。
- **release-check 真实性**：`python agentcall.py release-check`（实际入口 `scripts/agentcall_dev.py:cmd_release_check`）会跑 cargo workspace test + pytest，可据此确认新 fixture 会被门禁拉起。

---
*本报告所有 file:line 引用与「现状」核对均经命令行直接核实于 HEAD `a8f7224`；评价性结论（范围、冗余、脆弱性、排期）为基于该核实的工程判断，最终以仓库当前状态为准。*
