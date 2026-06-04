# Plan Review - v3.0 PTY-Only Utility Workers

Reviewer: OPUS  
Date: 2026-06-04

## 结论

OPUS 对 v3.0 的主方向给出强确认：**砍掉 ACP 主线，收敛为 PTY-only utility workers 是正确方向**。原因不是 ACP 没有理论价值，而是当前项目最需要的是可见、可确认、可维护的 worker 控制面。

v2.2 到 v2.4 连续给 ACP 增加模板、权限、后台 supervisor、heartbeat 和 orphan 处理，但每一轮都暴露出更深的底层问题：adapter schema 不稳定、stdio child 不可恢复、Codex App 原生可视性不足、daemon 重启后 ownership 难以解释。继续加机制会把项目拖回双 runtime 复杂度。

## 关键建议

1. **先删掉 ACP 当前实现**
   - 不要把 ACP 暂存成半可用 legacy runtime。
   - MCP schema、route、runtime health、README 当前能力都不应该继续暴露 ACP。
   - 历史文档可以保留，但必须标为历史方向。

2. **PTY-only 的价值在可确认**
   - PTY 对人类和 Codex 都更容易观察。
   - Codex 可以通过 compact board、session summary、clean tail 和 report 做监督。
   - 这比追求“更省 token 的不可见调用”更适合当前阶段。

3. **唯一必须保留并强化的 ACP 经验：权限边界**
   - ACP 最大的可取经验是 daemon 能做真正的权限边界。
   - v3.0 应把这个经验迁移到 PTY hooks：带 `allowed_paths` 的 PTY route 必须用 `PreToolUse` 做 path-scoped deny。
   - 默认 auto PTY 不等于放弃约束。

4. **Prompt submit gate 是 v3.0 的关键正确性**
   - 之前多次出现 session 创建了但 prompt 没真正送达。
   - daemon route 必须返回 `started_and_prompt_submitted`、`started_pending_prompt_ack` 或 `prompt_submit_failed`，不能 silent fail。

5. **MCP 控制面要压缩**
   - 默认工具面只保留 daemon、board、route、session、session_send、report。
   - `agentcall_session` 默认返回 llm_summary，不返回大段 raw output。
   - `agentcall_board(view=compact)` 默认不返回 raw events、transcripts、claims 全量。

## 风险

- PTY-only 会带来更高的可见进程成本，但这是当前版本可接受的代价。
- 默认 auto mode 需要 hook path enforcement，否则 worker 仍可能越界写文件。
- plan mode 不应成为默认，否则会重新增加 Codex 的交互负担；只有不清楚或高风险任务才显式启用 `plan_then_auto`。

## v3.0 验收关注点

- `runtime=acp` 被明确拒绝。
- 默认 route 能稳定启动 PTY utility worker。
- 带 `allowed_paths` 的 PTY route 能拒绝越界写入。
- prompt submit gate 能识别 prompt 是否真的提交。
- board/session summary 足够短，Codex 不需要默认读取 raw terminal。
