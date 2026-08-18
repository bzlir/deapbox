# Stage 1 Verification — L3 Manual End-to-End

> L1 单元测试（37 个，`cargo test --workspace`）覆盖 config/dispatcher/echo/event。
> L2 集成测试（cli 内 9 个）覆盖装配 + handle_payload。
> 本文档是 L3——真实飞书群手动验证，operator 自己跑。

## Prerequisites

1. **飞书应用已创建**——有 `app_id` + `app_secret`，已启用机器人能力，已订阅 `im.message.receive_v1` 事件。
2. **`config.toml`** 已配置（参考 `config.toml.example`）：
   ```toml
   [lark]
   app_id = "cli_xxx"
   app_secret = "sec_xxx"

   [[agents]]
   id = "echo-a"
   kind = "echo"
   command = ""

   [[sessions]]
   chat_id = "oc_test_group_a"
   agent_id = "echo-a"
   ```
3. **`chat_id` 获取方式**——把 bot 拉进飞书群，在群里发任意消息，deapbox 终端日志会打印 `unbound chat oc_xxx, ignored`（如果群未绑定），抄下 `oc_xxx` 填入 `config.toml` 的 `[[sessions]]` 段。

## Test cases

### V8.1 · 已绑定群发消息 → echo 原样回显

**步骤**：
1. `cargo run -p deapbox-cli`（或 `cargo run` 启动 deapbox）
2. 终端看到 `deapbox Stage 1 running; press Ctrl+C to shut down`
3. 在已绑定群（`oc_test_group_a`）发消息 `hello`

**期望**：
- 群里收到 `hello`（deapbox 回显的原样文本）
- 终端日志无报错

### V8.2 · 未绑定群发消息 → 静默丢弃 + 日志

**步骤**：
1. deapbox 运行中
2. 把 bot 拉进另一个未绑定群（不在 `config.toml` 的 `[[sessions]]` 里）
3. 在该群发消息 `test`

**期望**：
- 群里**无任何回执**（静默）
- deapbox 终端日志打印 `unbound chat oc_xxx, ignored`

### V8.3 · 同一群连发两条 → 按序回显（per-chat 串行）

**步骤**：
1. deapbox 运行中
2. 在已绑定群快速连发 `first` 和 `second`

**期望**：
- 群里依次收到 `first`、`second`（顺序保证，不乱序）
- 不丢失任何一条

### V8.4 · 两个群同时发消息 → 各自回显（跨群并行）

**步骤**：
1. deapbox 运行中
2. `config.toml` 配两个绑定：
   ```toml
   [[agents]]
   id = "echo-a"
   kind = "echo"
   command = ""

   [[agents]]
   id = "echo-b"
   kind = "echo"
   command = ""

   [[sessions]]
   chat_id = "oc_group_a"
   agent_id = "echo-a"

   [[sessions]]
   chat_id = "oc_group_b"
   agent_id = "echo-b"
   ```
3. 在群 A 发 `from-a`，**几乎同时**在群 B 发 `from-b`

**期望**：
- 群 A 收到 `from-a`
- 群 B 收到 `from-b`
- 两个群都收到回显，不互相阻塞

### V8.5 · 中文 / emoji / 空文本 → 原样回显

**步骤**：
1. 在已绑定群分别发：
   - `中文测试`
   - `🎉 emoji test`
   - 空消息（如果飞书允许）

**期望**：
- 群里依次收到 `中文测试`、`🎉 emoji test`、空字符串（如果飞书允许发空消息）
- 不出现乱码

### V8.6 · Ctrl+C → 优雅退出

**步骤**：
1. deapbox 运行中
2. 在终端按 `Ctrl+C`

**期望**：
- 终端打印 `received SIGINT (Ctrl+C), shutting down` + `deapbox shut down`
- 进程退出（exit code 0）
- 不卡死

### V8.7 · `kill <pid>` → SIGTERM 退出

**步骤**：
1. deapbox 运行中，记下 pid
2. 另开终端 `kill <pid>`

**期望**：
- deapbox 终端打印 `received SIGTERM, shutting down` + `deapbox shut down`
- 进程退出

### V8.8 · WS 连接断开 → deapbox 退出

**步骤**：
1. deapbox 运行中
2. 断网 / 飞书后台关连接 / 等待 WS 空闲超时

**期望**：
- deapbox 终端打印 `WebSocket inbound channel closed, shutting down` 或 `WebSocket connection closed`
- 进程退出（ADR-0005：Stage 1 不做重连，operator 重启 deapbox）

### V8.9 · 非文本消息 → 忽略 + 日志

**步骤**：
1. 在已绑定群发一张图片（不是文本消息）

**期望**：
- 群里无回显
- deapbox 终端日志打印 `failed to parse Lark event payload... UnsupportedMessageType("image")`（或类似）
- deapbox 不崩溃，继续运行

### V8.10 · p2p 私聊 → 也能 echo

**步骤**：
1. 把 `[[sessions]]` 的 `chat_id` 改成私聊 chat_id（p2p chat 也有 `oc_` 开头的 chat_id）
2. 重启 deapbox
3. 私聊 bot 发消息 `private hello`

**期望**：
- 私聊里收到 `private hello`（ADR-0005：p2p 和群聊统一用 `chat_id` 路由）
- 不需要区分 `chat_type`

## 故障排查

| 现象 | 可能原因 | 检查 |
|---|---|---|
| 启动报 `LarkApiError::ClientConfig` | app_id/app_secret 错 | 检查 `config.toml` `[lark]` 段 |
| 启动报 `UnsupportedAgentKind` | 配了非 echo agent | Stage 1 只支持 `kind = "echo"` |
| 群里发消息无回显 + 日志 `unbound chat` | 群没在 `[[sessions]]` 里 | 抄日志里的 chat_id 加进 config 重启 |
| 群里发消息无回显 + 无日志 | WS 没连上 | 看启动日志是否有 `starting Feishu WebSocket` |
| deapbox 启动后立即退出 | WS 连接失败 | 看终端日志的 WS error |
