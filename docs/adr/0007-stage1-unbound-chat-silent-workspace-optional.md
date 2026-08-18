# Stage 1 unbound chat handling + workspace field shape

## Context

Stage 1 walking skeleton 还有两个小决策悬着：

1. 未绑定 chat 收到消息时主循环怎么处理？（之前 Q9 提出回执文本选项）
2. `config.toml` 的 `[[sessions]]` 段要不要 `workspace` 字段？echo agent 不用 workspace，但 Stage 2 真 agent 落地时需要。

用户决定：未绑定 chat 处理推迟到 Stage 3 斜杠命令系统（`/bind` 或 `/switch`），Stage 1 不处理。workspace 字段选 (x') 选填。

## Decision

**未绑定 chat 行为（Stage 1）**：静默丢弃 + 日志记一行。

- 主循环查 binding miss 时：`tracing::info!("unbound chat {}, ignored", chat_id)` + 不发任何飞书回执。
- operator 第一次绑定流程推迟到 Stage 3 的斜杠命令系统——届时 operator 在群里发 `/bind echo-a` 或 `/switch echo-a` 主动绑定，不需要预先知道 chat_id + 编辑 config 重启。
- Stage 1 的 bootstrap 路径：operator 在飞书群发消息 → 群里静默（无回执）→ operator 切到 deapbox 终端看日志抄 chat_id → 编辑 config.toml 加 `[[sessions]]` → 重启 deapbox → 第二次发消息正常路由。
- 这个摩擦在 Stage 1 walking skeleton 阶段可接受——单人远程驾驶场景的"远程自助"目标是 Stage 3 才完整落地。

**Workspace 字段（Stage 1）**：`[[sessions]]` 段选填 `workspace`，`Binding.workspace: Option<WorkspacePath>`。

```toml
# config.toml Stage 1 形状
[[sessions]]
chat_id = "oc_xxx"
agent_id = "echo-a"
# workspace 选填，echo 不用，Stage 2 真 agent 落地时改必填
# workspace = "/Users/me/lab/project-A"
```

```rust
pub struct Binding {
    pub agent_id: AgentId,
    pub workspace: Option<WorkspacePath>,  // Stage 1: Option; Stage 2: 改 required
}
```

- Stage 1 echo agent 收到 `Binding` 忽略 `workspace` 字段。
- Stage 2 真 agent 落地时改 `Binding.workspace: WorkspacePath`（非 Option）+ 加 config validation 要求 `[[sessions]]` 必填 `workspace`。这是 Stage 2 已知 schema migration，不波及 trait 形状（`Agent::send` 签名不变，agent impl 自己读 `Binding.workspace`）。
- forward-compat 思路同 ADR-0003 的 `attachments: &[Attachment]` 参数——形状先到位，Stage 1 选填/空，Stage 2 改必填/非空。

## Rationale

1. **未绑定回执推迟符合 Stage 分层**——Stage 1 验证"飞书 → 路由 → agent → 飞书"链路通，不验证"远程自助绑定"。后者是 Stage 3 斜杠命令系统的事。Stage 1 强行处理会引入命令解析路径（forwarder 入口要区分 `/` 前缀命令 vs 普通消息），增加 Stage 1 scope。
2. **静默丢弃比错误回执简单**——群里发"未绑定"会让 operator 困惑（"那我怎么办？"）；静默 + 日志让 operator 知道该去 config 加绑定。单人场景下 operator 有机器访问权，切终端看日志可接受。
3. **workspace 选填符合 YAGNI + forward-compat 平衡**——必填（x）冒犯 operator（"echo 不用 workspace 为什么要写"）；不放（y）Stage 2 要改 schema + 所有 `Binding` 用例；选填（x'）形状先到位、operator 不被冒犯、Stage 2 只改 Option → required 一个字段类型。
4. **跟 ADR-0003 的 attachments 形状先到位一致**——两者都是"Stage 2 会用但 Stage 1 不用的字段，先放形状后填 impl"。

## Consequences

- **Stage 1 主循环极简**——查 binding hit → `dispatcher.dispatch(msg)`；查 binding miss → 日志一行 + continue。无命令解析、无未绑定回执分支。
- **Stage 1 `Binding` schema**：`agent_id: AgentId`（必填）+ `workspace: Option<WorkspacePath>`（选填）。config parser 用 `#[serde(default)]` 让 `workspace` 缺省 `None`。
- **Stage 3 升级路径**：加斜杠命令系统（`/bind <agent_id>` / `/switch <agent_id>` / `/new` / `/session`），forwarder 入口分叉命令 vs 消息。届时未绑定 chat 收到 `/bind` 命令 → 创建 binding 进 sled + 回执 "已绑定到 X"；收到普通消息仍静默丢弃（或回执"未绑定，请发 /bind <agent_id> 绑定"）。
- **Stage 2 升级路径**：`Binding.workspace` 从 `Option<WorkspacePath>` 改 `WorkspacePath`；config validation 加 "workspace required" 检查；agent impl 读 `Binding.workspace` 用作 agent 进程的 `current_dir`。
- **ChatDispatcher::start 签名**（ADR-0006）：`bindings: Vec<(ChatId, Binding, Arc<dyn Agent>)>` 里的 `Binding` 携带 `Option<WorkspacePath>`，Stage 1 不影响 dispatcher 内部逻辑（它不读 workspace，只把 `Arc<dyn Agent>` 跟 chat 绑定）。
