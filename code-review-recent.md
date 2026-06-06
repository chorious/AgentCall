# AgentCall 最近改动审查报告

审查对象：当前工作树未提交改动（`git diff`，相对 HEAD `af4667d`）。
共 9 个文件、+382 / -43 行。基线：`cargo build` ✅ / `cargo test` 36 passed ✅ / clippy 仅轻微警告。

## 改动构成

| 类别 | 文件 | 说明 |
|------|------|------|
| **新功能** | `summary.rs` (+280) | Plan artifact 抽取：从 Claude transcript 解析 ExitPlanMode / plan_mode 附件 / plan 文本 |
| **接口** | `mcp.rs`、`tools.rs` | `agentcall_session` 的 `include` 增加 `plan` 选项 |
| **修复** | `routes.rs:1` | 删除文件首部 BOM（`\u{feff}use`），属真实潜在问题修复 |
| 纯格式 | `hooks.rs`、`state.rs`、`session.rs`、`hook/main.rs`、`runtime_lock.rs` | rustfmt 重排，无逻辑变更 |

实质新增逻辑集中在 `summary.rs`：`plan_artifact_from_binding`、`session_plan_artifact`、`looks_like_plan_text`、`extract_plan_text_from_value`、`clip_chars`。

## 发现问题（按严重度）

### 🟠 1. `session_summary` 每次调用都全量读取并解析 transcript —— 热路径性能回退
`summary.rs:298` 在 `session_summary` 中无条件调用 `plan_artifact_from_binding(&binding, &clean_output, false)`，后者会 `fs::read_to_string(transcript)` 并对**整个 JSONL 逐行 `serde_json::from_str`**（`summary.rs:773-820`）。
而 `session_summary` 处于高频热路径：
- `attention_items`（`summary.rs:484`）对**每个 live session** 调用 `session_summary`；
- `board_state` compact/attention 视图又调用 `attention_items`。

因此每次 board 轮询 = 每个 session 各读一遍完整 transcript（Claude transcript 常达数 MB）。session 数 × 轮询频率下，这是明显的 IO/CPU 放大。
- 建议：仅对 `pty_workflow=plan_then_auto` 且 phase=plan 的 session 计算 plan artifact；或对 transcript 读取做 mtime/大小缓存；或只在 `include=plan` 显式请求时才解析，summary 里降级为 `plan_ready` 的轻量判断（如复用已有的 route `workflow_status==plan_ready`）。

### 🟡 2. 对所有普通 session 也付出 transcript 解析成本却只得到 `plan_ready=false`
非 plan 工作流（默认 auto）的 session 没有 plan 概念，但 `session_summary` 仍解析其 transcript 仅为填入 `plan_ready=false`/`plan_source=none`。与 #1 同源，属无效计算；按工作流类型短路即可消除。

### 🟡 3. `looks_like_plan_text` 启发式易误判长助手消息
`summary.rs:899-934`：阈值为「>400 字符 且 strong_score>0 且 strong+weak≥2」。strong 标记含 `"# "`/`"## "`（任意 Markdown 标题）、weak 含 `goals`/`steps`/`risks`。一条讨论 “goals/steps” 且带任意标题的长助手回复即可被判为 plan，导致非 plan 阶段 `plan_ready=true`、`plan_source=transcript_text` 误报。
- 建议：transcript 路径优先依赖 `ExitPlanMode` tool_use / `plan_mode` 附件这类强信号；`transcript_text`/`clean_tail` 文本启发式仅作为兜底并标注低置信度。

### 🟢 4. 轻微
- `plan_artifact_from_binding` 会读取 transcript 中 `planFilePath` 指向的任意本地文件（`summary.rs:840-851`）。本地工具，风险低，但路径来源不可信，建议限定在预期目录内。
- transcript 多次出现计划时取**最后一次**（覆盖式赋值），符合直觉但未注释；长会话语义可补充说明。
- clippy 警告（collapsible-if、`format!` 等）若干，`cargo clippy --fix` 可清。
- 本次改动新增 2 个单测覆盖 plan 抽取（detection / transcript-without-plan-file），方向正确；建议补「非 plan 长输出不误报」「include=plan 返回 content」用例。

## 结论
逻辑正确、测试通过，BOM 修复是加分项。**主要问题是 #1**：把重量级 transcript 解析放进了 board/attention 热路径，且对所有 session 无条件执行。建议按工作流类型短路 + 仅在显式 `include=plan` 时做完整解析，再合入。
