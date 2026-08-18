# Stage 1 module boundaries, interfaces, and seams

## Context

ADR-0001 through ADR-0008 locked behavioral decisions. Before TDD implementation, module boundaries and interface signatures must be explicit so tests can be written against stable seams.

## Decision

**Module layout**:

```
deapbox-core/
  src/
    lib.rs              # re-export
    types.rs            # domain types (pure data, no behavior)
    agent.rs            # Agent trait
    lark_api.rs         # LarkMessageApi trait
    dispatcher.rs       # ChatDispatcher + DispatchError + impl
    test_support.rs     # #[cfg(test)] pub fakes: FakeAgent, FakeLarkMessageApi
deapbox-agent/
  src/
    lib.rs
    echo.rs             # EchoAgent: Agent
deapbox-lark/
  src/
    lib.rs
    api.rs              # OpenLarkMessageApi: LarkMessageApi
    event.rs            # parse_text_message + LarkEventBridge
    ws.rs               # LarkWsClient 启动 + payload channel
deapbox-store/
  src/
    lib.rs
    config.rs           # load_config → AppConfig
deapbox-cli/
  src/
    main.rs             # tracing init → run_from_args
    lib.rs              # 装配 + 主循环 + shutdown
```

**Seam rules**:

1. **`LarkMessageApi` trait lives in `deapbox-core/src/lark_api.rs`** — not `deapbox-lark`. `ChatDispatcher` (in core) depends on `Arc<dyn LarkMessageApi>`; putting the trait in lark would force core to reverse-depend on lark (workspace cycle). Same pattern as `Agent` trait in core, impl in agent crate.

2. **`Agent` trait lives in `deapbox-core/src/agent.rs`** — separate from `types.rs`. `types.rs` is pure data structures (no behavior); `agent.rs` is the behavioral contract. This matches the old code's `types.rs` + `traits.rs` split.

3. **`ChatDispatcher::start` takes two HashMaps**:

   ```rust
   pub fn start(
       bindings: HashMap<ChatId, Binding>,
       agents: HashMap<AgentId, Arc<dyn Agent>>,
       lark_api: Arc<dyn LarkMessageApi>,
   ) -> Self
   ```

   Dispatcher internally looks up `binding.agent_id` in the agents map and Arc-clones into the per-chat task. CLI assembles both HashMaps; dispatcher owns the agent-registry lookup. More symmetric than passing pre-resolved `(ChatId, Binding, Arc<dyn Agent>)` tuples.

4. **Shared test fakes live in `deapbox-core/src/test_support.rs`** — `#[cfg(test)] pub mod test_support` exposed from `lib.rs`. `FakeAgent` + `FakeLarkMessageApi` are shared across `dispatcher.rs` unit tests and `deapbox-cli` integration tests (avoids review F5's FakeStore duplication). Per-module unit tests still inline their own one-off fakes when divergence is real.

**Interface signatures (Stage 1)**:

```rust
// deapbox-core/src/agent.rs
#[async_trait]
pub trait Agent: Send + Sync {
    async fn send(
        &self,
        chat_id: &ChatId,
        text: &str,
        attachments: &[Attachment],
    ) -> Result<Vec<AgentEvent>, CoreError>;
}

// deapbox-core/src/lark_api.rs
#[async_trait]
pub trait LarkMessageApi: Send + Sync {
    async fn send_text(&self, chat_id: &ChatId, text: &str) -> Result<(), LarkApiError>;
}

// deapbox-core/src/dispatcher.rs
pub struct ChatDispatcher { /* internal: N task handles + HashMap<ChatId, mpsc::Sender> */ }

impl ChatDispatcher {
    pub fn start(
        bindings: HashMap<ChatId, Binding>,
        agents: HashMap<AgentId, Arc<dyn Agent>>,
        lark_api: Arc<dyn LarkMessageApi>,
    ) -> Self { ... }

    pub fn dispatch(&self, msg: UserMessage) -> Result<(), DispatchError> { ... }
}

impl Drop for ChatDispatcher {
    fn drop(&mut self) { /* abort all tasks */ }
}

pub enum DispatchError {
    UnboundChat(ChatId),
    ChannelClosed(ChatId),
}

// deapbox-core/src/types.rs
pub struct Binding {
    pub agent_id: AgentId,
    pub workspace: Option<WorkspacePath>,  // Stage 1: Option; Stage 2: required
}

pub enum AgentEvent {
    Text(String),
    Thinking(String),
    ToolCall(String),
    ToolResult(String),
    Error { message: String, fatal: bool },
    TurnEnd { resume_key: Option<String> },
}

pub enum Attachment {
    Image { image_key: String },
}

pub struct UserMessage {
    pub chat_id: ChatId,
    pub sender: UserId,
    pub text: String,
    pub msg_id: String,
    pub attachments: Vec<Attachment>,
}

pub enum AgentKind {
    Echo,
    ClaudeCode,
    KimiCode,
    Opencode,
    Codex,
}

// deapbox-store/src/config.rs
pub fn load_config(path: &Path) -> Result<AppConfig, ConfigError>;

pub struct AppConfig {
    pub lark: LarkConfig,
    pub agents: Vec<AgentConfig>,
    pub sessions: Vec<SessionConfig>,
}

pub struct SessionConfig {
    pub chat_id: ChatId,
    pub agent_id: AgentId,
    pub workspace: Option<WorkspacePath>,  // optional per ADR-0007
}

pub struct AgentConfig {
    pub id: AgentId,
    pub kind: AgentKind,
    pub command: String,
}

// deapbox-lark/src/event.rs
pub fn parse_text_message(payload: &[u8]) -> Result<UserMessage, LarkEventError>;
```

## Rationale

1. **`LarkMessageApi` in core avoids workspace cycle** — core cannot depend on lark; trait-in-core + impl-in-lark is the only sound direction.
2. **`agent.rs` separate from `types.rs`** — pure data vs behavioral contract; matches old code split.
3. **Two-HashMap `start` signature** — CLI assembles two registries; dispatcher owns the join. More symmetric than pre-resolved tuples; CLI stays thin.
4. **`test_support.rs` shared fakes** — directly applies review F5's lesson (FakeStore was duplicated across router + manager tests in old code).
5. **Signatures are the test surface** — locking them first lets TDD proceed without redesign mid-test.

## Consequences

- **Test-first order T1-T12** (in ADR body above) proceeds with stable seams.
- **`ChatDispatcher::start` signature may change in Stage 3** — when b-lazy lands, `bindings` may shift to a dynamic source. `dispatch` signature stays stable.
- **`Binding.workspace: Option<WorkspacePath>`** — Stage 1 config parser uses `#[serde(default)]` for `workspace`; Stage 2 changes to required + adds validation.
- **`test_support.rs` is `#[cfg(test)] pub`** — only visible in test builds; no production API surface cost.
- **No `AgentDriver` trait in Stage 1** — old code had `AgentDriver` as a factory per kind; Stage 1 only has `EchoAgent` and the CLI directly constructs `Arc<dyn Agent>` instances. `AgentDriver` returns in Stage 2 when real agents need factory-driven spawn (per-kind `start_session`).
