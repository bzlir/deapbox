# ADR-0002 修正: opencode 自成 wire 层，不共享 adapter.rs

## Context

ADR-0002 锁定 "`adapter.rs` 是 stream-json kinds 共享的 wire 解析层"——当时假设 opencode 跟 claude/kimi 一样走 stream-json 协议，共享完整 wire 层（NDJSON 解析 + result 事件过滤 + resume_key 抽取 + assistant-block 解析）。

调研 opencode CLI 实际协议（`opencode run --format json`）发现**协议跟 claude stream-json 共享面比预想小**：

| 项 | opencode | claude stream-json | 共享价值 |
|---|---|---|---|
| NDJSON 行解析 | ✓ | ✓ | 共享（trim + len check + serde_json::from_str，8 行） |
| `type` 字段分派 | ✓ | ✓ | 共享（按 raw["type"] 分派） |
| turn-end 事件 | `step_finish` | `result` | **不共享**——event type 名不同 |
| resume_key 字段 | `sessionID` | `session_id`/`sessionId`/`resume_key`/`resumeKey` | **不共享**——字段名不同 |
| assistant-block 解析 | 无（直接 `text` 事件） | 有（assistant 事件带 content blocks） | **不共享** |
| compaction 过滤 | 无 | 有（`subtype: compact/compaction`） | **不共享** |

共享面只有"NDJSON 行解析 + type 分派"这一薄层。turn-end、resume_key、assistant-block 全是 per-kind 的。

## Decision

**修正 ADR-0002**：`adapter.rs` 是 **claude 风格 stream-json kinds**（claude-code / kimi-code）共享的 wire 解析层；**opencode 有独立 wire 解析层**（自己的 NDJSON 协议，`step_finish` 作 turn-end）；codex 也独立。

```
deapbox-agent/src/
  adapter.rs          # claude/kimi 共享（result/assistant/compaction）
  opencode.rs         # opencode 独立（step_start/text/step_finish）
                      # 各自 own parse_ndjson_line（8 行 utility，不抽 module）
```

`parse_ndjson_line` 不抽成 `ndjson.rs` utility module——3 个 kind 各 own 一份 8 行函数（trim + length check + serde_json::from_str）比抽 cross-module use 简单。**deletion test**：删掉共享 utility，每个 kind 各 own 8 行，复杂度不集中也不分散——"一个 adapter = 假想 seam"。

## Rationale

1. **deletion test 通过**：删 opencode.rs，把它的 wire 解析搬进 adapter.rs——adapter.rs 同时承载两套 turn-end 规则（claude `result` + opencode `step_finish`）、两套 resume_key 字段抽取、两种 assistant 结构，复杂度不是"集中"是"混在一起"。opencode.rs 是 load-bearing 的深 module。
2. **真 seam vs 假想 seam**：opencode 协议跟 claude stream-json 共享面太小（NDJSON 行解析 8 行），把"共享 8 行"当"真 seam"是过度抽象——claude/kimi 还没落地，强行抽象是 YAGNI。
3. **ADR-0002 原则保留**："TurnEnd 是 host 侧抽象" + "per-kind 映射各自协议的 turn-end"两条不变。修正的只是"`adapter.rs` 服务范围"——从"所有 stream-json kinds"缩到"claude 风格 stream-json kinds"。
4. **`parse_ndjson_line` 各自 own**：8 行 utility，3 个 kind 各写一份比抽 ndjson.rs + cross-module use 简单。这是 deletion test 的边界——共享面太薄不足以独立 module。

## Consequences

- **`adapter.rs` scope 收窄**：服务 claude-code / kimi-code 两个 kind（claude 风格 stream-json），不服务 opencode。
- **opencode.rs 自包含**：own NDJSON 解析 + step_start/text/step_finish 分派 + sessionID 抽取 + OpenCodeAgent: Agent impl。
- **codex 也独立**：未来 codex 落地时，按它自己的协议（app-server 模式）独立 wire 层，不进 adapter.rs。
- **未来可能性**：如果 opencode 协议演化出 assistant-block / compaction（向 claude 风格收敛），届时再考虑把 opencode 的部分逻辑搬进 adapter.rs——但这是未来事，不预先抽象。
- **`adapter.rs` 在 Stage 2 仍可暂不落地**：claude/kimi 都还没接，adapter.rs 可以等 Stage 3 接 claude/kimi 时再写。Stage 2 只写 opencode.rs。
