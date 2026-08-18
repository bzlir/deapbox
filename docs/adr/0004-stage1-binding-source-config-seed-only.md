# Stage 1 binding source: config seed only, no runtime /switch, no sled

## Context

Stage 1 walking skeleton 需要一个 binding（chat_id → agent_id 映射）来源。三条候选：
- (a) config seed only — operator 在 `config.toml` 写 `[[sessions]]` 预绑，重启生效
- (b) `/switch` 命令 only — runtime 命令绑，内存 binding，重启丢
- (c) config seed + `/switch` — 两者叠加

Q5 选定 (a)。

## Decision

**Stage 1 binding 来源 = `config.toml` 的 `[[sessions]]` 段，启动时一次性读入内存，运行期不可变**。

具体：

```toml
# config.toml
[[agents]]
id = "echo-a"
kind = "echo"
command = ""

[[agents]]
id = "echo-b"
kind = "echo"
command = ""

[[sessions]]
chat_id = "oc_xxxxxxxxxxxxxxxx"
agent_id = "echo-a"
workspace = "/Users/me/lab/project-A"

[[sessions]]
chat_id = "oc_yyyyyyyyyyyyyyyy"
agent_id = "echo-b"
workspace = "/Users/me/lab/project-B"
```

- 启动时读 config → 建 `HashMap<ChatId, Binding>` 内存表 → 路由就查这张表。
- **无 `/switch` 命令**——Stage 1 不实现命令解析。`/switch` 留给 Stage 3（配 sled 持久化时一起落）。
- **无 sled**——Stage 1 无任何持久化层。Config 是唯一状态源。
- **catch-22 工作流**——operator 第一次在群里发消息时无 binding，deapbox 把 chat_id 写进日志 + 飞书群回执（"未绑定，请编辑 config.toml 的 [[sessions]] 段后重启"）。operator 抄下 chat_id → 编辑 config → 重启 → 第二次发消息正常路由。

## Rationale

1. **最简单**——不需要命令解析器、不需要 sled、不需要 binding 持久化抽象。Stage 1 只验证"飞书 → 路由 → agent → 飞书"链路通。
2. **config 是显式真相源**——version-controlled，operator 一眼看清绑定关系，不靠 runtime 状态。
3. **catch-22 可接受**——单人远程驾驶场景下 operator 拥有机器，第一次发消息看日志抄 chat_id 是一次性成本，不是日常路径。
4. **`AppConfig.sessions` 不是 seed**——Stage 1 它是唯一 binding 源。Stage 3 落 sled + `/switch` 时它降级为首启 seed（沿用 Lesson #4 的设计），届时再写新 ADR。
5. **`/switch` 推迟**——Stage 1 不实现命令路径，forwarder 入口不区分命令/消息，所有进来的 text 都走 agent 路径（echo 收到 `/switch echo-a` 会回 "echo-a: /switch echo-a"，滑稽但可接受）。

## Consequences

- **第一次绑定的 bootstrap 流程**：发消息 → 看日志/chat 回执抄 chat_id → 编辑 config → 重启。Stage 1 接受这个摩擦。
- **Stage 3 升级路径**：加 sled + `/switch` + `/new` + `/session` 命令。届时 `AppConfig.sessions` 改回 seed 语义，sled 成为 runtime binding 源，restart 时 config 覆盖 sled（或 sled 优先，看 Stage 3 决策）。
- **workspace 字段进 config schema**——Stage 1 echo 不用 workspace，但 schema 里先放（forward-compat，避免 Stage 2 加字段迁移）。`Binding { agent_id, workspace }` 结构沿用老设计（types.rs:57-61）。
- **不绑定的 chat 收到消息时**——Stage 1 行为固定：日志 + 飞书群回执 "未绑定"。不路由到 agent，不报错崩溃。
- **私聊（p2p）和群聊（group）统一用 `chat_id` 作 binding key**——inbound 路由层不区分 `chat_type`，两种 chat 都按 `chat_id` 查 binding 表。`chat_type` 只在 outbound 选 `MessageRecipient` 时需要区分（私聊可能需要用 `open_id` 而不是 `chat_id` 收发，具体差异由 Stage 1 实测时决定，不预先抽象）。Stage 1 不为 p2p 单独写逻辑分支。
