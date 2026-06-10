# Pro Feedback Analysis for v5.4+

Date: 2026-06-11

Source reports:

- [pro-control-plane-hardening-review.md](reports/pro-control-plane-hardening-review.md)
- [pro-session-api-tui-contract-review.md](reports/pro-session-api-tui-contract-review.md)

## Summary

Pro 的两份反馈指向同一个根问题：AgentCall 已经有清楚的产品形状，但控制面契约还没有被硬化到足以支撑高并发、长时间、低干预的 supervisor 工作流。

第二份反馈更急，因为它直接影响 Codex/TUI 每次观察 worker 的成本。`agentcall_session` 目前把 summary、clean tail、events、hook raw、policy debug 混进同一个响应，导致一次普通查看就返回大量重复、脏、不可决策的信息。v5.4 应先解决这一块。

第一份反馈更深，属于控制面可靠性和安全边界问题。它不适合塞进一个版本，需要拆成 v5.4.1、v5.4.2、v5.4.3 三个连续硬化版本。

## Problem Matrix

| Source | Problem | Current Symptom | Target Version |
|---|---|---|---|
| Session API/TUI review | `agentcall_session` 单次返回契约太粗 | `include=["summary","clean_tail","events"]` 返回 prompt/raw/tool output/write content | v5.4 |
| Session API/TUI review | clean tail 是 ANSI strip，不是 TUI 语义抽取 | Claude Ink repaint 变成 `✢ C n / * a / ...` 单字符噪声 | v5.4 |
| Session API/TUI review | events 默认暴露 raw payload | PostToolUse/PostToolBatch 复制 stdout，UserPromptSubmit 复制整段 prompt | v5.4 |
| Session API/TUI review | policy denial 没投影成可行动状态 | events 里连续 deny，但 summary 仍可能是 `attention_status=none` | v5.4 |
| Session API/TUI review | TUI 缺少 path diagnosis | cwd、target workspace、scratch path、policy compare 坐标系不清 | v5.4 |
| Control-plane review | route start 同步等待 hook ack，可能拖垮 MCP | daemon 仍在等 ack，MCP 已 timeout/transport closed | v5.4.1 |
| Control-plane review | stop/kill 语义混合，lease 释放过早 | stop 近似 kill，旧进程退出前 lease 可被释放 | v5.4.1 |
| Control-plane review | actor 单队列、无优先级、无 panic guard | stop/interrupt 可能排队，actor panic 难投影 | v5.4.1 |
| Control-plane review | destructive commands precondition 不够硬 | 空 `{}` precondition 可过，lease id/generation 复用 | v5.4.2 |
| Control-plane review | route policy 依赖 wrapper binding，缺失时不 fail closed | hook 丢失后退化成 file claim，而不是硬阻断 | v5.4.2 |
| Control-plane review | path policy/canonicalization 不可靠 | target workspace、claude cwd、scratch path 可能错位 | v5.4.2 |
| Control-plane review | local HTTP/WS 无认证且 CORS `*` | 本地恶意页面可尝试调用 daemon API | v5.4.3 |
| Control-plane review | MCP stdin 和 daemon HTTP 输入缺少硬预算 | 大 JSON/大响应可能导致 transport closed | v5.4.3 |
| Control-plane review | Python legacy / hook crate direct-write 仍是架构债 | 单写者边界不够清楚，schema 漂移 | v5.4.3 |

## Proposed Version Split

### v5.4: Session API Contract + TUI Extraction

Make observation cheap and actionable. This version should not try to solve every control-plane safety issue. It should reduce `agentcall_session` payload weight, introduce compact events, and replace raw clean tail as the default human/LLM surface.

### v5.4.1: Short Route Transactions + Lifecycle Semantics

Make route/session control calls short, observable, and non-blocking. Split stop/kill and move lifecycle closure to process-exit observation.

### v5.4.2: Preconditions + Binding/Path Policy Hardening

Make destructive commands, wrapper binding, lease identity, and containment semantics stricter. This version focuses on "do not act on stale or unbound state".

### v5.4.3: Local Daemon Boundary + Legacy Cleanup

Make the local service safer and easier to reason about: HTTP/WS guardrails, MCP input caps, log redaction, and direct-write legacy cleanup.

## Immediate Non-Code Decision

For v5.4, `clean_tail` should be treated as a debug surface, not as the supervisor default. Codex should read `summary` or `tui` view first. Raw event payloads should require explicit debug/raw access.

