# Stage 1 inbound source: openlark WebSocket; outbound: chat_id uniform

## Context

Stage 1 walking skeleton 需要：
1. 入站消息源——老代码 `deapbox-cli/src/lib.rs:289` bail `InboundEventsUnavailable`，注释说 "openlark WebSocket handler does not expose event payload forwarding yet"。
2. 出站路径——老代码 `deapbox-lark/src/api.rs:51` `OpenLarkMessageApi::send_text` 一律用 `MessageRecipient::chat_id(&chat_id.0)`。

经查 openlark 0.20 源码（`examples/01_getting_started/websocket_echo_bot.rs`）：入站"不可用"是**假摩擦**——openlark 有 `websocket` feature flag，deapbox workspace `Cargo.toml` 没开。开了之后 `EventDispatcherHandler::builder().payload_sender(tx).build()` + `LarkWsClient::open(config, handler)` 是完整可用的 WS 入站 API，转发 raw event bytes 到 `mpsc::UnboundedReceiver<Vec<u8>>`。

老 `deapbox-lark/src/event.rs::LarkEventBridge::handle_event_payload(&[u8])` 已经把 `im.message.receive_v1` JSON 解析成 `UserMessage` 且测试过——直接接到 `payload_rx` 后面即可。

## Decision

**Inbound source = openlark WebSocket**（开 `websocket` feature）；**outbound 一律 `chat_id`**（不区分 `chat_type`）。

具体：

1. **workspace `Cargo.toml` 加 `websocket` feature**：
   ```toml
   openlark = { version = "0.20.0", default-features = false, features = ["auth", "communication", "websocket"] }
   ```

2. **WS 入站链路**：
   ```
   LarkWsClient::open(config, handler)
     → EventDispatcherHandler.payload_sender(tx) 转发 raw bytes
     → mpsc::UnboundedReceiver<Vec<u8>>
     → LarkEventBridge::handle_event_payload(&[u8])
     → parse_text_message() 解析 im.message.receive_v1
     → UserMessage { chat_id, sender, text, msg_id, attachments: vec![] }
     → mpsc::UnboundedSender<UserMessage> 进 forwarder
   ```
   老 `event.rs` 的 `LarkEventBridge` + `parse_text_message` + 测试沿用；新建一个 `deapbox-lark/src/ws.rs`（或类似）承载 WS 连接生命周期 + 把 `payload_rx` 接到 `LarkEventBridge`。

3. **Outbound 一律 `chat_id`**——`LarkMessageApi::send_text(chat_id, text)` 内部 `MessageRecipient::chat_id(&chat_id.0)` 不分 p2p/group。飞书 `im.message.create` 接口 `receive_id_type=CHAT_ID` 对 p2p chat 也成立（p2p chat 也有 `oc_` 开头的 chat_id）。如果实测发现 p2p 发不出，再升级到按 `chat_type` 分发——届时 `UserMessage` 加 `chat_type` 字段，forwarder 按 type 选 `MessageRecipient`。这是 Stage 1 已知 fallback，不预先抽象。

4. **非 text 消息 Stage 1 忽略**——`parse_text_message` 已经在 `message_type != "text"` 时返 `UnsupportedMessageType` err。Stage 1 行为：日志记 "ignored non-text message" + 不回执。image 消息等 Stage 2 真 agent 落地时再加 attachment 下载路径（ADR-0003 已为 `Attachment::Image { image_key }` 留形状）。

5. **WS 重连策略**——openlark 示例注释 "#421 不发明重连策略；生产侧自行在 ConnectionClosed 后决定是否重连"。Stage 1 行为：WS 关闭 → deapbox 整体退出（带错误日志），operator 重启 deapbox。重连逻辑留给 Stage 3+。

## Rationale

1. **修假摩擦而非新设计**——开 feature flag 是一行改动；老 `event.rs` 解析逻辑 + 测试沿用；forwarder 已经准备好接 `mpsc::Receiver<UserMessage>`（老 `run_console_loop` 就是这形状）。
2. **outbound 一律 chat_id 简单**——`MessageRecipient::chat_id` 是飞书 OpenAPI 的统一收件人格式，p2p chat 也有 `chat_id`。Stage 1 不为想象中的差异预先分支。
3. **WS 重连推迟**——单人远程驾驶场景下 deapbox 退出后 operator 远程重启是可接受的（SSH/启动脚本）。Stage 1 验证链路通即可，不急着做生产级重连。
4. **非 text 消息忽略**——Stage 1 echo agent 用不上 attachments，attachment 下载路径是 Stage 2 的事（ADR-0003 已锁形状）。

## Consequences

- **`deapbox-lark` 公开 API**：`LarkWsClient` 启动函数 + `LarkMessageApi::send_text` + `LarkEventBridge`（或合并成一个 `LarkInbound` 抽象）。具体形状实现时定。
- **`deapbox-cli` startup**：`run_service` 不再 bail `InboundEventsUnavailable`，改成启动 WS client + spawn forwarder task。
- **WS 关闭 = deapbox 退出**——Stage 1 不做重连。operator 看到 "WebSocket closed: ..." 日志后重启 deapbox。
- **Stage 2 升级路径**：attachment 下载（`download_image(image_key)`）+ p2p outbound 分支（如果实测发现需要）+ 重连策略。
