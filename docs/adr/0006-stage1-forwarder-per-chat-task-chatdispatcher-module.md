# Stage 1 forwarder: per-chat task (b-init) behind ChatDispatcher module

## Context

ADR-0001 锁定 "per-chat turn 串行 + 跨 chat 天然并行"。Stage 1 walking skeleton 需要一个 forwarder 实现这个语义。候选三种：

- (a) 单 task 串行处理所有 chat — 违反"跨 chat 天然并行"
- (b) per-chat mpsc + per-chat task — 符合 ADR-0001
- (c) per-chat Mutex + 共享 task pool — 复杂度高于 (b) 无收益

(b) 有两个子选项：
- b-init: 启动时按 config 一次性起所有 chat task
- b-lazy: 首次见到 chat_id 时起 task

Q8 选定 (b-init)，但要求 chat dispatcher 模块独立，让 Stage 3 升级到 (b-lazy) 时不波及其他模块。

## Decision

**Forwarder = `ChatDispatcher` 模块，内部 (b-init) per-chat task，对外暴露窄接口。**

**对外 API**（其他模块只看这个）：

```rust
pub struct ChatDispatcher { /* N 个 task handle + HashMap，内部细节不对外 */ }

impl ChatDispatcher {
    /// 启动 N 个 per-chat worker task，返 dispatcher 实例。
    /// 每个 task 跑无限循环：收 mpsc → 跑 turn → 发回执。
    /// 实例 drop 时所有 task abort。
    pub fn start(
        bindings: Vec<(ChatId, Binding, Arc<dyn Agent>)>,
        lark_api: Arc<dyn LarkMessageApi>,
    ) -> Self { ... }

    /// 主循环调——把 inbound 消息派给对应 chat task。
    /// 未绑定返 Err，主循环自己决定回执。
    pub fn dispatch(&self, msg: UserMessage) -> Result<(), DispatchError> { ... }
}

pub enum DispatchError {
    UnboundChat(ChatId),   // 主循环回执 "未绑定" 用
    ChannelClosed(ChatId), // Stage 3+ 才会发生，Stage 1 task 永不退出
}

impl Drop for ChatDispatcher {
    fn drop(&mut self) { /* abort 所有 task */ }
}
```

**`start` 语义**：构建实例 + 启动 N 个后台 tokio task。返回的实例是轻量 handle，但**启动时就产生了 N 个后台 task**——它们跑无限循环（收 mpsc → 跑 turn → 发回执），生命周期跟 `ChatDispatcher` 实例绑定，实例 drop 时所有 task abort。命名 `start` 而非 `spawn` 避免跟 tokio `task::spawn` 混淆；语义 A（启动后台 worker + 返实例），不是语义 B（同步路由器，违反 ADR-0001 跨 chat 并行）。

**内部细节**（不对外，Stage 3 可改）：

- `HashMap<ChatId, mpsc::UnboundedSender<UserMessage>>` 启动时一次性建完，只读
- 每个 chat 一个 tokio task，task 内部 loop：
  1. `mpsc::Receiver::recv()` 收 UserMessage（FIFO 自然保证 per-chat 串行，无需 Mutex）
  2. 查 `Arc<dyn Agent>`（启动时 Arc clone 进 task）
  3. `agent.send(chat_id, text, &attachments).await` → `Vec<AgentEvent>`
  4. for event in events: match arm 渲染 + `lark_api.send_text(chat_id, rendered).await`
  5. `AgentEvent::TurnEnd` 释放 queue（loop 顶部自然回到 recv 下一条）
- `LarkMessageApi` 用 `Arc<dyn LarkMessageApi>` 共享，每个 task Arc clone
- `Agent` 用 `Arc<dyn Agent>` 共享，每个 task Arc clone

**边界划分**：

- 主循环不接触 task / channel / HashMap，只调 `dispatcher.dispatch(msg)`
- 主循环职责：WS payload_rx → parse → `dispatcher.dispatch(msg)` → 若 Err(UnboundChat) 自己调 `lark_api.send_text(chat_id, "未绑定...")` 回执
- Stage 3 升级到 (b-lazy) 时：
  - `start` 签名可能变（bindings 参数从 `Vec` 改成动态 source）
  - `dispatch` 签名不变
  - 主循环代码不动

## Rationale

1. **ADR-0001 直接落地**——per-chat task = per-chat 串行；N 个 task = 跨 chat 并行。
2. **b-init 代码最简**——startup 一个 for 循环 spawn N 个 task，建 `Arc<HashMap>` 只读。无需 `Mutex<HashMap>`，无运行期同步成本。
3. **模块独立降低升级耦合**——ChatDispatcher 是个黑盒，主循环只看 `start` + `dispatch`。Stage 3 改内部从 b-init 到 b-lazy（动态 HashMap + 首次见到时 spawn），主循环零改动。
4. **资源开销在单人规模下是噪音**——2-5 个 task + 2-5 个 mpsc channel，内存差 < 1MB。决策依据是代码简单度 + 升级路径，不是资源。
5. **`start` 命名精确**——清楚表明"启动后台 worker + 返实例"，跟 tokio `spawn` 区分。语义 A（后台 task）是 ADR-0001 的强制要求，语义 B（同步路由器）违反跨 chat 并行。
6. **mpsc FIFO 天然串行**——per-chat task 单线程跑 `recv → send → render → recv`，无需 Mutex。turn queue 就是 mpsc receiver 本身。
7. **未绑定 chat 不起 task**——主循环查 binding miss 直接回执，不进 dispatcher 路径。Stage 1 不存在动态创建 task 的需求。

## Consequences

- **`ChatDispatcher` 模块位置**：`deapbox-core/src/dispatcher.rs`（或 `deapbox-cli/src/dispatcher.rs`，具体实现时定）。它依赖 `Agent` trait + `LarkMessageApi` trait + `UserMessage` / `Binding` / `ChatId` 类型，不依赖任何具体 agent impl 或 Lark SDK。
- **Drop semantics**：Stage 1 粗暴 abort task（`JoinHandle::abort`），不等待 turn 完成。Stage 2+ 可以加 graceful shutdown（等 TurnEnd 或超时后 abort）。
- **`Arc<dyn Agent>` 共享**：同一 agent_id 可被多个 chat 绑定（config 里 `[[sessions]]` 多条用同一 agent_id），每个 chat task 持一个 Arc clone。Echo agent 无状态，共享安全。Stage 2 真 agent 可能 per-chat 独立 session（每个 chat 一个 Agent 实例，不共享）——届时 `start` 签名改成 `bindings: Vec<(ChatId, Arc<dyn Agent>)>`，bindings 在调用方已按 chat 分配好 agent 实例。
- **Stage 3 升级路径**：内部 HashMap 改 `Arc<Mutex<HashMap>>` 或 `DashMap`；`start` 签名可能改（bindings 从 `Vec` 改成动态 source 或加 `add_binding` / `remove_binding` 方法）；`dispatch` 签名不变；主循环零改动。
- **错误处理**：`agent.send` 失败或 `lark_api.send_text` 失败时，chat task 内部日志记错 + 继续 recv 下一条（不退出 task）。turn 中途失败不影响下一个 turn。具体错误策略 Stage 1 实现时定，不进 ADR。
