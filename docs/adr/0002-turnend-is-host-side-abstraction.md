# TurnEnd is a host-side abstraction; per-kind maps its protocol's turn-end signal

## Context

Q2 锁定了 per-chat turn 串行——`TurnEnd` 是 queue 的释放信号，load-bearing。现有 spec（`ARCHITECTURE.md` 设计基线第 2 项 + Lesson #2/#3）把 TurnEnd 等同于 stream-json 的 `result` 事件（过滤 `subtype: compact/compaction`）。但 `ARCHITECTURE.md §7` 已为 codex 留了 "app-server 模式后续" 的口子——app-server 协议不走 stream-json，没有 `result` 事件。需要决定 TurnEnd 是协议级抽象还是流式级具体。

## Decision

**TurnEnd 是 host 侧抽象**，不是 stream-json 的具体事件。`AgentEvent::TurnEnd { resume_key: Option<String> }` 是 host 侧 turn 完成的统一信号；per-kind `AgentSession` 实现负责把**各自原生协议的 turn-end 信号**映射到这个 variant。

- stream-json kinds（claude-code / kimi-code / opencode）→ 映射非 compaction 的 `result` 事件（沿用 Lesson #2/#3）。
- 非 stream-json kinds（codex app-server 等）→ 映射各自协议的 turn-end 信号，wire 格式等 codex 落地时再定。

Lesson #2 的核心——"turn-end 必须由 agent 自己说，不是 host 用 idle-timeout 猜"——被泛化保留：agent 自己说，用什么协议说由 per-kind 决定。idle-timeout 仍仅作 dead-agent 安全网，不作 turn 边界探测器。

**Adapter 作用域子决策**：`deapbox-agent/src/adapter.rs` 是 stream-json kinds 共享的 wire 解析层（NDJSON 解析 + `result` 事件过滤 + resume_key 抽取 + assistant-block 解析）；非 stream-json kinds（codex）有独立的 wire 解析层，不共享 `adapter.rs`。这顺势解决 architecture review F1 标记的 "半成 seam" 问题——assistant-block parsing 从 per-kind session 搬进 `adapter.rs`，让 stream-json kinds 共享完整 wire 层。`working.md:57` 的 "后续可统一" 落地。

## Rationale

1. **不堵死 codex 的口子**——选 stream-only 等于推翻 `ARCHITECTURE.md §7` 的 codex 规划。
2. **`AgentEvent::TurnEnd` 已经是抽象**——只是 `adapter.rs` 把 `result` 映射成它。抽象边界本就在 per-kind session 这层（`AgentSession::subscribe() -> Receiver<AgentEvent>`）。
3. **Lesson #2 可泛化**——原则是 "agent 自己说 done"，"`result` 事件" 只是 stream-json kinds 的具体实现。
4. **`resume_key: Option<String>` 已经是 Option**——codex 若无 resume 概念返回 None 即可，不破坏 trait 形状。
5. **F1 评审的半成 seam 被顺势解决**——assistant-block parsing 搬进 adapter.rs 是被 `working.md:57` 与 `kimi_code.rs:344-345` 双重 sanction 的，本决策把"后续可统一"明确为"现在统一"。

## Consequences

- **`adapter.rs` 服务于 stream-json kinds 全集**（claude/kimi/opencode）；assistant-block parsing 不再 per-kind 复制（评审 F1 修复）。
- **codex 落地时新增 `deapbox-agent/src/codex/`（或同级 module）独立 wire 层**，不复用 `adapter.rs`。codex 的 compaction 信号（如果有）由 codex 自己的 impl 过滤，不复用 stream-json 的过滤规则。
- **`is_compaction_result` 与 `extract_resume_key` 仍在 `adapter.rs`**——它们的语义是 stream-json 协议级的，不泛化到 codex。
- **`StreamJsonEvent` enum 不上移到 core**——它代表 stream-json 协议的具体事件分类，是 wire 层概念，不进 host 侧抽象。host 侧只认 `AgentEvent`。
