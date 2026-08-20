# Stage 2: streaming trait + per-chat agents + opencode impl

## Context

Stage 2 落地第一个真 agent (opencode) 需要三个 foundational 升级：

1. **Agent trait 从 batch 改 streaming**——opencode 是 NDJSON 流式输出，不能继续用 `send -> Vec<AgentEvent>` batch 形状
2. **ChatDispatcher 签名从 (bindings, agents) 改 per-chat agents**——OpenCodeAgent 需要 per-chat workspace + session_id，不能跨 chat 共享
3. **opencode 独立 wire 层**——ADR-0010 已锁，opencode 协议跟 claude stream-json 共享面太小

## Decision

**1. Agent trait 升级到 streaming**：

```rust
// Stage 1 (batch)
async fn send(...) -> Result<Vec<AgentEvent>, CoreError>;

// Stage 2 (streaming)
async fn send(...) -> Result<AgentEventStream, CoreError>;
// AgentEventStream = mpsc::Receiver<AgentEvent>
```

用 `mpsc::Receiver` 而非 `broadcast::Receiver`——ADR-0001 per-chat 串行意味着每 turn 只有一个消费者（dispatcher），不需要 broadcast 的多接收端。Stream end = sender drop（agent impl spawn 的 task 结束后 channel 自然 close）。

**2. ChatDispatcher::start 签名改成 per-chat agents**：

```rust
// Stage 1
pub fn start(
    bindings: HashMap<ChatId, Binding>,
    agents: HashMap<AgentId, Arc<dyn Agent>>,
    lark_api: Arc<dyn LarkMessageApi>,
) -> Self

// Stage 2
pub fn start(
    chat_agents: HashMap<ChatId, Arc<dyn Agent>>,
    lark_api: Arc<dyn LarkMessageApi>,
) -> Self
```

CLI 负责按 binding + workspace 构造每个 agent 实例。EchoAgent 仍可跨 chat 共享（Arc clone）；OpenCodeAgent per-chat 独立（带 workspace + session_id chain）。Dispatcher 不再感知 AgentId / Binding——只管 ChatId → Agent 路由。

**3. opencode 独立 wire 层**（ADR-0010 已锁）：

```
deapbox-agent/src/opencode/
  adapter.rs    # pure function wire 层（NDJSON parse + event mapping）
  agent.rs      # OpenCodeAgent: Agent impl（spawn + stream + session resume）
```

`parse_ndjson_line` 各 kind 各 own 一份 8 行函数（ADR-0010 的 deletion test 结论）。

**4. OpenCodeAgent 设计**：

- Process-per-turn：每次 `send` spawn `opencode run --format json --auto [--session <prev>] "<text>"`
- Stdout NDJSON 每行调 `adapter::parse_event_line` + `event_to_agent_events`，推到 mpsc channel
- `step_finish{reason=stop}` 时从 event 拿 `sessionID` 存进 `Arc<Mutex<Option<String>>>`
- 下一次 `send` 用 `--session <prev>` 续接
- 进程退出后 channel 自然 close（dispatcher 的 `while let recv` 结束）

## Rationale

1. **mpsc 而非 broadcast**——per-chat 串行（ADR-0001）= 单消费者，broadcast 多接收端能力是 YAGNI
2. **per-chat agents 而非 shared agents + bindings**——OpenCodeAgent 需要每个 chat 独立 workspace + session_id chain；echo 可共享但 per-chat 实例化成本极低（stateless）
3. **dispatcher 不感知 AgentId/Binding**——seam 收窄，dispatcher 只管 ChatId → Agent 路由，binding 逻辑在 CLI 装配层
4. **opencode adapter 独立**——ADR-0010 deletion test 通过，共享面只有 NDJSON 行解析（8 行），不值得抽 module

## Consequences

- **所有 Agent impl 升级**——EchoAgent + FakeAgent 都改成 spawn task 推 channel 后返回 receiver（行为等价，接口形状变）
- **dispatcher per_chat_task 改 while let recv**——从 `for event in events` 改 `while let Some(event) = stream.recv().await`
- **CLI build_chat_agents_registry**——新函数，按 session config 构造 per-chat agent 实例（echo 共享 / opencode 独立）
- **workspace 字段对 opencode 必填**——`CliError::MissingWorkspace` 新 variant；echo 仍 optional
- **resume_key 跨 turn 续接**——OpenCodeAgent 内部 `Arc<Mutex<Option<String>>>` 存 sessionID，跨 deapbox 重启需要 sled 持久化（Stage 3）
- **Stage 1 测试全部保持绿色**——FakeAgent + EchoAgent 行为等价，dispatcher 测试断言不变，只是 helper 签名改
