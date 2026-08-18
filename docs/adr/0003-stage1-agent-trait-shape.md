# Stage 1 agent trait: batch multi-type events, inbound attachments shape-ready, outbound Image deferred

## Context

Stage 1 walking skeleton 锁定 1-b（双向）+ 多类型返回（`AgentEvent` 多变体）。需要决定：

1. `AgentEvent` 变体集合（multi-type 范围）
2. `Agent::send` 签名（batch `Vec` vs streaming `Receiver`）
3. 入站 Image（operator → 多模态 agent）支持是否进 Stage 1
4. 出站 Image（agent → operator）支持是否进 Stage 1

## Decision

**`AgentEvent` 变体集合**（Stage 1 落地）：

```rust
pub enum AgentEvent {
    Text(String),
    Thinking(String),
    ToolCall(String),
    ToolResult(String),
    Error { message: String, fatal: bool },
    TurnEnd { resume_key: Option<String> },
}
```

`TurnEnd` 必有（ADR-0001 释放 queue 用）；其他变体预留位，Stage 1 echo 只发 `Text` + `TurnEnd{None}`。`Exited` / `Failed` 等进程级生命周期变体 Stage 2 真 agent 落地时再加。

**Echo agent 行为**：原样回显——`EchoAgent::send(chat_id, text, &[])` 返 `[Text(text), TurnEnd{None}]`。不加 prefix、不改内容、不区分 agent_id。operator 发 "hello" → 群里看到 "hello"。这是 Stage 1 唯一的真业务逻辑，验证"飞书 → 路由 → agent → 飞书"链路通的 walking skeleton。

**`Agent::send` 签名**：batch 返回，不 streaming：

```rust
#[async_trait]
pub trait Agent: Send + Sync {
    async fn send(
        &self,
        chat_id: &ChatId,
        text: &str,
        attachments: &[Attachment],
    ) -> Result<Vec<AgentEvent>, CoreError>;
}
```

- `Vec<AgentEvent>` 批返回——echo 自然只返一两条事件。
- `chat_id` 一直 thread（避免评审 F4 那种"trait 缺 chat 身份"）。
- `attachments` 形状先到位，Stage 1 永远空 `&[]`。
- **Stage 2 升级到 streaming**（`subscribe() -> Receiver<AgentEvent>`）时 trait 方法形状会变——这是已知 rework，刻意不在 Stage 1 过早抽 channel。

**入站 Attachment 形状先定**：

```rust
pub struct UserMessage {
    pub chat_id: ChatId,
    pub sender: UserId,
    pub text: String,
    pub msg_id: String,
    pub attachments: Vec<Attachment>,  // Stage 1 永远空 Vec
}

pub enum Attachment {
    Image { image_key: String },  // Feishu image_key；bytes 懒下载
}
```

`Attachment` 是 enum（不是 struct）为未来文件/视频/音频附件留扩展位。`Image` 只存 `image_key` 不存 bytes——Stage 1 forwarder 不下载（反正 echo 不看）；Stage 2 真 agent 需要时由 agent impl 自己调 `LarkMessageApi::download_image(image_key)`。

**出站 `AgentEvent::Image` 推迟**：Stage 1 不加。等第一个 agent 真吐图（PNG bytes 流给 host）时再加变体 + forwarder 上传逻辑。理由：planned agents（claude-code / kimi-code / opencode / codex）即便底层模型多模态，CLI 输出主要是文本+工具调用，不会主动吐图。"agent 生成本地图"是工具调用副产物（如 `matplotlib.savefig`），agent 会发 `ToolResult("saved chart.png")` 文本，operator 自己去 workspace 看图，不需要 deapbox 中转图片字节。加变体但 forwarder 那条 match arm 写 `todo!()` 是半成 seam（评审 F1 教训）。

## Rationale

1. **inbound 形状先定避免 Stage 2 改 trait 签名**——若 Stage 1 签名是 `send(chat_id, text)`，Stage 2 加 image 支持 = 改 trait 签名 = 所有 impl 都得改。现在 `attachments` 参数先放进去，Stage 2 只填 impl 不改形状。
2. **batch 比 streaming 简单**——echo 自然不流式，强行 channel 化是过度设计。Stage 2 真 agent 落地时按真实流式需求改，不在 Stage 1 猜。
3. **outbound 推迟避免半成 seam**——加 `AgentEvent::Image` 但 forwarder 那条 arm 写 `todo!()` 是评审 F1 教训。
4. **`Attachment` enum 留扩展位**——今天只有 `Image`，未来文件/视频/音频复用同形状。

## Consequences

- Stage 1 forwarder 渲染：每个 `AgentEvent` 一条飞书文本消息（`Text` 原样、`Thinking` 加 `[thinking]` 前缀、`ToolCall` 加 `[tool]`、`ToolResult` 加 `[result]`、`Error` 加 `[error]`、`TurnEnd` 不发消息只释放 queue）。chatty 但 walking skeleton 够用。
- Stage 2 升级路径：trait 方法从 `Vec<AgentEvent>` 返回改成 `Receiver<AgentEvent>`（broadcast 或 mpsc）；`AgentEvent` enum 加 `Exited`/`Failed` 变体；`Attachment::Image` 落地下载路径；forwarder 抽 `OutputSink` trait（卡片流式 PATCH 替换简单 send_text）。
- `LarkMessageApi` Stage 1 只需 `send_text(chat_id, text)` + `download_image(image_key)`（后者 Stage 1 不调，形状先到位）。
