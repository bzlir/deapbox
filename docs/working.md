# deapbox Working Log

> 记录每一次提交的更改，结构类似 changelog。每个条目含**日期**与**更改内容**。
> 文末有 **Lesson Learned** 部分，沉淀阶段性经验。

---

## Changelog

### 2026-08-04 — 文档结构初始化 + 架构基线锁定

**更改内容：**

- 新建 `docs/` 目录，作为项目文档根。
- `ARCHITECTURE.md` 从仓库根移动到 `docs/ARCHITECTURE.md`，并按 grill 会话锁定的四项核心决策**整体更新**：
  - 并发模型：Router 非阻塞、task-per-message、不再 `Arc<dyn AgentProcess>`。
  - 协议：驱动 agent 进原生 `--output-format stream-json`；turn-end = `result` 事件（过滤 `subtype: compact/compaction`）；EOF ≠ 完成。
  - session/存储：`ChatSession` 纯绑定（不挂 resume_key）；`PersistentStore` binding 含 workspace + resume_key 独立 KV；`AppConfig.sessions` 降级为首启 seed。
  - driver trait：`AgentDriver`（工厂 per-kind）+ `AgentSession`（`subscribe() -> Receiver<AgentEvent>`，`TurnEnd`/`Exited` 在流里）。
- 新建 `docs/working.md`（本文件）：changelog + lesson learned。
- 同步更新 7 节 mermaid 图（整体架构 / 路由流程 / 事件流 / 依赖 / 主循环 / stream-json 协议 / crate 结构）。
- 代码 crate 仍为 stub，按 [TES-77](../) 子任务 TES-78~85 推进（范围将按锁定设计重画）。

**未改代码**：本次仅文档；`Cargo.lock` 的本地构建副作用已还原，不纳入提交。

---

<!-- 后续条目按以下模板追加：
### YYYY-MM-DD — 简标题

**更改内容：**
- ...
- ...

**影响：** （可选，说明影响的 crate / 子任务 / 行为变化）
-->

---

## Lesson Learned

> 沉淀阶段性经验，避免重复踩坑。条目按发现时间倒序或按主题归类。

### 1. 先例只提供机制，不决定范围

引用 cc-connect（同域飞书↔coding agent 网关）时，一度把它的"一个 chat 下多个 Session + active 切换"整套搬进 deapbox。但 deapbox 的范围是"一个飞书会话绑一个 (agent + workspace)，靠多个会话管多个 agent"——更简单。**先例的机制（stream-json、resume key、per-kind session）可以学，但它的范围决策（多线程-per-chat）不能照抄。** 范围由产品意图决定，不由先例决定。

### 2. turn-end 必须由 agent 自己说，不能由 host 猜

最初的设计用 idle-timeout + EOF + prompt 复现启发式判断"本轮结束"。查 cc-connect 源码发现：它驱动 agent 进原生 `stream-json`，turn-end 是 agent 显式发射的 `result` JSON 事件，确定性、不会在 agent 慢思考时误判。**idle-timeout 只配作 dead-agent 安全网，不能当边界探测器。** 优先用 agent 的原生结构化协议，别抓裸 stdout 文本。

### 3. `result` 带 `subtype: compact/compaction` 是中途压缩，不是完成

claude-code / opencode 在长任务中途会发 `type:"result"` 但 `subtype:"compact"` 的压缩通知——agent 还会继续。**把它当 turn-end 会让长任务被拦腰切断、丢后半轮输出。** 必须显式过滤这个 subtype。

### 4. 热键和冷键要分存，别合并成一条记录

`resume_key` 每个 turn 末写（热），`binding`（agent + workspace）只在 `/switch` 时写（冷）。把它们塞进同一个 `ChatSession` 结构体一起序列化 = 每个 turn 重写整条绑定记录。**sled 的 KV 粒度让分开是免费的**——`binding:{chat_id}` 冷、`resume:{chat_id}` 热。cc-connect 用单个 JSON 快照 blob 没法分，才把 id 塞进 Session 结构体；deapbox 有 sled，分开才是惯用法。

### 5. trait shape 不能自相矛盾

原 `AgentProcess` trait：`get_or_spawn → Arc<dyn AgentProcess>`（共享）+ `recv_output(&mut self)`（独占）+ `shutdown(self: Box<Self>)`（所有权）——三个不可能同时成立，通过 `Arc` 既调不了 `&mut self` 也到不了 `Box<Self>`。**这是类型层面的"非法状态可表示"。** 解法不是加 `Mutex` 硬糊，而是用通道解耦：`subscribe() -> Receiver<AgentEvent>`，读流变成 `&self`，input/output 都 `&self`，死结消失。让类型系统替你暴露设计矛盾。

### 6. 读源码，别读 README

cc-connect 的真实机制（stream-json、result 事件、compaction 过滤、不用 `--continue`）只有读它的 Go 源码（`agent/claudecode/session.go` 等）才看得到。README 和架构图不会写这些陷阱。**做先例调研时，证据要到源码行号，不到文档描述。**

### 7. grill 是双向的，认错要快

用户两次纠正我（Q3 多线程-per-chat 越界、resume_key 该在 sled 不在 ChatSession）。每次我都用源码证据验证后立即认错并修正。**grill 的目的是逼近正确设计，不是守住自己的面子。** 对的决策就接受，证据优先于立场。

---

<!-- 后续 lesson 按主题或时间追加：
### N. 标题
...
-->
