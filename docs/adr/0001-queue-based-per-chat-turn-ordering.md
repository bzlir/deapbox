# Queue-based per-chat turn ordering

## Context

deapbox 的 Router 已锁定 "task-per-message + 非阻塞主循环"（ARCHITECTURE.md 设计基线第 1 项），但 spec 留了半个洞：当某 Chat 的 turn N 仍在跑、Operator 又发来消息 N+1 时，默认怎么处理？

候选三种：

- **A · 队列串行**：N+1 等 N 发 `TurnEnd` 后再 send。
- **B · 中断替换**：N+1 到达 → `interrupt()` 杀 N → 起 N+1。
- **C · 并行**：N 与 N+1 共存。Agent CLI 是 turn-based 单 stdin，C 实际不可行。

## Decision

采用 **A · 队列串行**：同一 Chat 上 turn 严格按到达顺序串行执行，N+1 必须等 N 的 `TurnEnd` 事件（即 agent 自己发射的 `result`，过滤 `subtype: compact/compaction`，见 Lesson #2/#3）落地后才能 `send` 给 AgentSession。

跨 Chat 天然并行——task-per-message 模型不变，主循环不阻塞。

## Rationale

1. **Resume 链不断**：N 写出 `resume_key` 后 N+1 才起跑，agent 续接点明确。中断替换（B）会让 N 的部分工作蒸发且 resume_key 链停在 N-1，对"持续开发"目标不友好。
2. **协议层零改动**：agent 不用关心 host 端排队，stream-json input/output 协议不变。
3. **单人范围支持**：单 Operator 不存在多操作者争抢同一 Chat 的并发需求；"一次只一个 turn"是合理约束。
4. **可观测**：Operator 远程看飞书卡片时，turn 边界清晰（每轮有明确的开始/结束），不会出现"被中途切断"的混乱。

## Consequences

- **UX 代价**：Agent 跑长任务时 Operator 必须等 `TurnEnd` 才能发下一条。这是单人远程驾驶场景下可接受的取舍——若需"算了重发"语义，后续可作为 `/cancel` 显式命令叠加，不进默认路径。
- **实现影响**：`AgentSession::send(&self)` 不需要加内部互斥锁——host 侧 queue 保证同一 Chat 上 send 串行。Router 需要一个 per-chat 的"待发队列"或"turn-in-flight 标志"。
- **`interrupt()` trait method 仍然保留**：作为未来 `/cancel` 命令的挂载点，不进默认路径。评审 F2 的"trait 宽于调用面"问题靠未来 wire `/cancel` 解决，不靠删除方法。
