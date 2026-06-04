# Review — report.md(AgentCall Codex 运行状态诊断报告)

Reviewer: Opus 4.8 · 日期: 2026-06-03
方法: 对照 daemon 源码、`.agentcall/events.ndjson`、`runtime_binding.json`、guKimi `.claude` 配置、运行中进程时序实测。

---

## 0. 总评

report 在运维层面有用(进程通、端口正常、定位到观测链路而非 Codex 本体崩溃,方向对),**RC1/RC5 判断准确**。但 **RC2 误诊**——它把"binding 没形成"归为"hook 没触发/Claude 没进入 hook 阶段",而实测证明 **hook 触发了、env 没传到 hook 子进程**。RC3 是真症状但根因没挖到。下面逐条核验。

---

## 1. 时序事实(用于纠正 RC2)

| 时间 (UTC) | 事件 |
|---|---|
| 2026-06-03 11:33 | v25 binding 写入(`binding_source=env`)——重启前的旧记录,持久化在 json 里 |
| **12:41:41** | **daemon 重启**(PID 29688,二进制 build 于 12:41:30,内含 `AGENTCALL_WRAPPER_SESSION` 字符串,已确认) |
| 12:52 / 12:55 / 12:58 | v26 三个 session `pty.session_started`(**重启后**,由当前 daemon 拉起) |
| 12:59 / 13:04 | 2 个 `hook.Stop` 到达,payload **`wrapper_session=None` / `binding_source=unbound`** |

**推论**:当前 daemon 确实 set env,v26 由它拉起,hook 也确实回传了——但 env 没进 hook。`runtime_binding.json` 里的 v25 是重启前的旧残留,不代表"现在 env 能 work"。排除"老二进制""残留旧 session"两种解释。

---

## 2. 逐条核验

### RC1 — Codex hook UTF-8 损坏 ✅ 成立,修法要改
- 确认 `scripts/agentcall-codex-hook.py:46` 为 `text = sys.stdin.read()`,按 locale(cp932)解码 → 中文入口即损坏,`sanitize()` 只替换不还原。
- **report 的 env 修法(PYTHONUTF8=1)不可靠**:Codex hook 由 Codex 经 `.codex/hooks.json` 用 anaconda python 启动,**非 daemon 拉起**,daemon 的 env 注入够不到它——这正是它停在 cp932 的原因。
- **正解(自包含、不依赖 env)**:
  ```python
  text = sys.stdin.buffer.read().decode("utf-8", errors="replace")
  ```
  两个 hook 脚本都改。Claude hook 当前"没坏"只是因为继承了 daemon 注入给 PTY 的 `PYTHONUTF8=1`;同样应改 buffer 读,别靠 env 兜底。

### RC2 — v26 没形成 binding ❌ 误诊
report 给的两个猜测("没产生可绑定的 hook""Claude 没进入 hook 阶段")**都不成立**:
- guKimi `settings.local.json` **已配** agentcall-claude-hook(9 处,line 399+)✅
- hook **已触发**(2 个 Stop 到达 daemon)✅
- **真因**:hook payload `wrapper_session=None` → `AGENTCALL_WRAPPER_SESSION` env **没从 Claude 传到 hook 子进程** → daemon 正确判 `unbound`、不写 binding。
- **v0.7.1 binding 代码无 bug**,它诚实标 unbound;断点在更上游的 **env 传播**。这正是 v0.7.1 review §6"动手前先实测 env 继承"被跳过的后果。
- 证据链:`upsert_runtime_binding_locked` 在 `env_wrapper_session=None` 且 `find_known_wrapper_binding` 落空(新 session_id 无前序绑定)时返回 unbound——逻辑正确,输入缺失。

### RC3 — PTY 活着但不可观测 ⚠️ 真症状,根因未挖到
- `replay_bytes=4` + `clean_output` 空 = Claude 几乎无 TUI 输出。真交互态 `claude` 启动即刷数百字节 banner。
- 说明这些 PTY 里 **Claude 没真正进入交互 TUI**(疑似卡在启动/trust 提示/login,或 spawn 的不是交互态)。与"无 hook 输出"同源。
- 待查:v26 的 spawn `command` 具体是什么。

### RC4 — board attention 噪声 ✅ 成立,易修
- 3 unbound + 8 legacy 全堆在 compact board。修:compact/attention **默认排除 legacy** + legacy 目录 GC。
- 注意:8 个 legacy 堆积部分由 RC5 导致(停不掉)。

### RC5 — stop 锁等待 ✅ 代码坐实的死锁
- `spawn_waiter`:`let mut child = session.child.lock(); child.wait()` —— **wait() 全程持 child 锁**。
- `stop_session`:`session.child.lock().kill()` 需同一把锁 → 进程活着时永久阻塞,**kill 不掉**。
- **正解**(`portable-pty 0.9` 支持 `clone_killer`):
  ```rust
  // Session 增加字段: killer: Mutex<Box<dyn ChildKiller + Send + Sync>>
  let killer = child.clone_killer();   // spawn 时、move child 进 waiter 之前
  // stop_session 改为: session.killer.lock().unwrap().kill()   // 不碰 child 锁
  ```

---

## 3. 优先级

| 优先 | 问题 | 动作 | 性质 |
|---|---|---|---|
| **P1** | RC5 死锁 | `clone_killer` 解锁 stop | 明确 bug,改动小,连带缓解 RC4 堆积 |
| **P1** | RC2 env 传播 | **先跑 §6 实测**(见下) | hook-aware 设计命门,不解决再多代码白搭 |
| **P2** | RC1 UTF-8 | 两个 hook 脚本改 `sys.stdin.buffer.read().decode("utf-8")` | 自包含 |
| **P2** | RC3 | 查 v26 spawn 命令,确认 Claude 是否真进交互态 | 诊断 |
| **P3** | RC4 噪声 | compact/attention 排除 legacy + GC | 体验 |

---

## 4. RC2 的 §6 env 实测(一锤定音)

spawn 一个 PTY,让 Claude hook 把 env 写文件:
```python
# 临时加到 agentcall-claude-hook.py 顶部
import os, pathlib
pathlib.Path("E:/Project/AgentCall/.agentcall/env_probe.txt").write_text(
    f"AGENTCALL_WRAPPER_SESSION={os.environ.get('AGENTCALL_WRAPPER_SESSION')}\n", encoding="utf-8")
```
触发一次 hook 后看文件:

- **若有值** → env 能传,只是 v26 那几个 session 走了别的 spawn 路径没 set → 修 spawn 路径。
- **若为 None** → Claude 剥掉了 hook 子进程的 env → env 通道作废。替代方案受限:Claude 给 hook 的 payload 只有 `cwd`/`session_id`/`transcript_path`,而 cwd 被 `force_claude_workspace` 全撞成 `D:\guKimi`(v26 三个同 cwd,已印证此死路)。届时唯一可靠替代是**给每个 session 唯一 cwd**(或 spawn 时记 cwd→wrapper,但并发同 cwd 会串)。**env 能修就别动 cwd 方案。**

---

## 5. 一句话

report 运维诊断有用,RC1/RC5 准;但 **RC2 必须改写**——不是"hook 没触发",而是"**hook 触发了、`AGENTCALL_WRAPPER_SESSION` env 没传到 hook 子进程**",v0.7.1 binding 层无 bug。下一步以 §6 实测为先,再谈 binding 怎么修。
