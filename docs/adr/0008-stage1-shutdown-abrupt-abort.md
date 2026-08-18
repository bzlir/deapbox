# Stage 1 shutdown: abrupt abort via ChatDispatcher::drop

## Context

Stage 1 walking skeleton 需要一个 shutdown 策略。Ctrl+C / SIGTERM 时怎么退出？

## Decision

**粗暴 abort**——主循环收到 SIGINT/SIGTERM → break 主 loop → `ChatDispatcher` 走 drop → `JoinHandle::abort` 所有 per-chat task → 进程退出。

实现层面：

- 主循环 `tokio::select!` 监听两个分支：
  - `payload_rx.recv()` — inbound 事件
  - `tokio::signal::ctrl_c()` — SIGINT (Ctrl+C)
  - （unix）`signal(SignalKind::terminate()).recv()` — SIGTERM (`kill <pid>`)
- 收到信号后 `break` 主 loop，主函数 return，`ChatDispatcher` 走 drop
- `Drop for ChatDispatcher`：for loop `handle.abort()`，5 行代码
- 不等待 turn 完成，不等待 WS 连接干净关闭

## Rationale

1. **echo agent 没"turn 中途"概念**——`EchoAgent::send` 同步返 `Vec<AgentEvent>`，要么全返要么全不返，粗暴 abort 不丢部分输出。
2. **Stage 2 真 agent 才有优雅 shutdown 需求**——claude-code 长驻进程被 abort 时 agent 子进程可能挂着，需要 graceful 才能干净 kill。这是 Stage 2 的事，本 ADR 只锁 Stage 1。
3. **比依赖 runtime drop 可控**——tokio runtime drop 时 task abort，但 `LarkWsClient` 的 WS 连接可能不干净关闭。`ChatDispatcher::drop` 显式 abort 比依赖 runtime drop 行为更可预测。
4. **实现最简**——`Drop` impl 5 行，主循环 select! 多两个分支。

## Consequences

- **Stage 1 退出行为**：Ctrl+C / `kill <pid>` → 立即退出，无等待。operator 重启 deapbox 即可。
- **WS 连接不干净关闭**：飞书后台可能短暂记连接异常，但 WS 长连接本来就有断线重连机制，下次启动 deapbox 连上即可。Stage 1 不做 WS 重连（ADR-0005 已锁）。
- **Stage 2 升级路径**：加 graceful shutdown——ChatDispatcher 加 `shutdown(timeout: Duration)` 方法，等各 task 跑完当前 turn 或超时后 abort；agent trait 加 `interrupt()` / `close()` 让真 agent 干净 kill 子进程；WS client 加显式 close 调用。届时本 ADR 被 supersede。
- **SIGTERM 监听平台差异**：unix 用 `tokio::signal::unix::signal(SignalKind::terminate())`；Windows 无 SIGTERM，用 `tokio::signal::ctrl_c()` 覆盖（Ctrl+C 是跨平台唯一保证的信号）。Stage 1 单人桌面场景（macOS/Linux），Windows 支持不在范围。
