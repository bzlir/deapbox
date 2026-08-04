# deapbox 架构图

## 1. 整体系统架构

```mermaid
graph TB
    subgraph "飞书"
        LarkWS[WebSocket 长连接]
        LarkAPI[OpenAPI]
    end

    subgraph "deapbox-cli"
        MainLoop["tokio::select! 主循环"]

        subgraph "deapbox-core"
            Router[Router\n消息路由]
            AgentMgr[AgentManager\n进程生命周期]
            Types[Types\n核心数据结构]
            Traits[Traits\n核心抽象接口]
        end

        subgraph "deapbox-lark"
            LarkEvent[Event Loop\n飞书事件接收]
            LarkCard[Card Streamer\n流式卡片刷新]
            LarkAPIAdapter[API Adapter\n飞书 API 调用]
        end

        subgraph "deapbox-agent"
            PtyProto[PTY Protocol\nstdio 通信]
            Adapter[ProtocolAdapter\n输出清洗层]
            OC[Opencode Adapter]
            CX[Codex Adapter]
            CC[ClaudeCode Adapter]
            KC[KimiCode Adapter]
        end

        subgraph "deapbox-store"
            SledDB[Sled\nKV 持久化]
            TomlConfig[TOML\n用户配置]
        end
    end

    subgraph "Agent 子进程"
        Proc1["opencode\npid:1234"]
        Proc2["codex\npid:5678"]
    end

    LarkWS -->|事件推送| LarkEvent
    MainLoop --> LarkEvent
    MainLoop --> Router
    MainLoop --> AgentMgr

    Router -->|读配置| SledDB
    Router -->|读绑定| TomlConfig
    Router --> AgentMgr

    AgentMgr -->|spawn/manage| PtyProto
    PtyProto -->|PTY stdin/stdout| Proc1
    PtyProto -->|PTY stdin/stdout| Proc2

    Adapter --> OC
    Adapter --> CX
    Adapter --> CC
    Adapter --> KC

    Router -->|转发消息| AgentMgr
    AgentMgr -->|send_input| PtyProto
    PtyProto -->|原始 stdout| Adapter
    Adapter -->|NormalizedEvent| Router
    Router -->|卡片刷新| LarkCard
    LarkCard -->|update message| LarkAPI
```

## 2. 消息路由流程

```mermaid
sequenceDiagram
    participant User as 飞书用户
    participant Lark as 飞书 WebSocket
    participant Event as Event Loop
    participant Router as Router
    participant Store as 持久化存储
    participant Mgr as AgentManager
    participant Adapter as ProtocolAdapter
    participant Agent as Agent 子进程

    User->>Lark: 发送消息
    Lark->>Event: 推送事件
    Event->>Event: 解析事件类型

    alt 命令消息 (/new, /session, /switch)
        Event->>Event: 解析 BotCommand
        Event->>Mgr: 执行命令
        Mgr-->>Lark: 返回结果卡片
    else 普通消息
        Event->>Router: route_user_message(msg)
        Router->>Store: 查找 ChatSession 绑定
        Store-->>Router: (AgentId, Workspace)
        Router->>Mgr: get_or_spawn(session)

        alt 进程不存在
            Mgr->>Agent: spawn (config, workspace)
            Agent-->>Mgr: AgentProcess 句柄
        else 进程已存在
            Mgr-->>Router: 复用现有句柄
        end

        Router->>Agent: send_input(text)
        loop 流式读取输出
            Agent-->>Adapter: 原始 stdout 行
            Adapter->>Adapter: process_line() 清洗
            Adapter-->>Router: NormalizedEvent

            alt Text
                Router->>Lark: 流式刷新卡片正文
            else Thinking
                Router->>Lark: 流式刷新折叠区域
            else ToolCall / ToolResult
                Router->>Lark: 显示操作提示
            else Error
                Router->>Lark: 报错通知
            else TurnComplete
                Router->>Lark: 完成卡片
            else SessionCreated
                Router->>Store: 持久化 resume key
            end
        end
    end
```

## 3. Agent Adapter 数据处理流

```mermaid
flowchart LR
    subgraph Agent输出
        Raw1["opencode stdout\n[12:30] ⠋ Processing...\n## 分析结果\n\n发现 bug..."]
        Raw2["claude-code stdout\n<thinking>检查空指针</thinking>\n<tool_use>read_file</tool_use>\n修复完成"]
    end

    subgraph Adapter层
        OC_Adapt["OpencodeAdapter\n启发式清洗:\n- 过滤 spinner\n- 过滤时间戳行\n- 保留正文\n- 标记操作行为 ToolCall"]
        CC_Adapt["ClaudeCodeAdapter\n结构化解析:\n- <thinking> → Thinking\n- <tool_use> → ToolCall\n- 其余 → Text"]
    end

    subgraph 统一事件流
        T["Text\n最终回复"]
        Th["Thinking\n思考过程"]
        Tc["ToolCall\n操作调用"]
        Tr["ToolResult\n操作结果"]
        TC["TurnComplete\n本轮结束"]
        E["Error\n错误"]
    end

    Raw1 --> OC_Adapt
    Raw2 --> CC_Adapt
    OC_Adapt --> T
    OC_Adapt --> Th
    OC_Adapt --> Tc
    CC_Adapt --> T
    CC_Adapt --> Th
    CC_Adapt --> Tc
    CC_Adapt --> Tr
    T --> Card[飞书卡片流式刷新]
    Th --> Card
    Tc --> Card
    Tr --> Card
```

## 4. 组件依赖关系

```mermaid
graph TD
    CLI[deapbox-cli] --> CORE[deapbox-core]
    CLI --> LARK[deapbox-lark]
    CLI --> AGENT[deapbox-agent]
    CLI --> STORE[deapbox-store]
    LARK --> CORE
    AGENT --> CORE
    STORE --> CORE
```

## 5. 主循环事件调度

```mermaid
graph TB
    subgraph tokio::select!
        F1["分支 1:\nlark_rx.recv()\n飞书事件"]
        F2["分支 2:\nagent_manager.watch_events()\n进程状态变更"]
        F3["分支 3:\ntokio::time::sleep(30s)\n健康检查"]
        F4["分支 4:\nshutdown_signal()\n优雅关闭"]
    end

    F1 -->|事件到达| H1[handle_lark_event]
    F2 -->|进程变更| H2[handle_agent_status]
    F3 -->|30s 定时| H3[health_check_all]
    F4 -->|Ctrl+C| H4[shutdown_all_processes]

    H1 --> R[Router.route_user_message]
    R --> AM[AgentManager.get_or_spawn]
    AM -->|失败重试| AM
```

## 6. PTY 通信协议

```mermaid
sequenceDiagram
    participant D as deapbox
    participant P as Agent 进程 (PTY)

    D->>P: spawn(config, workspace)
    P-->>D: 进程启动
    Note over D,P: 初始握手（可选）

    D->>P: stdin << 用户消息文本
    Note over P: agent 处理中...

    loop 逐行读取
        P-->>D: stdout >> 原始输出行
        D->>D: ProtocolAdapter.process_line()
        Note over D: → NormalizedEvent
    end

    P-->>D: stdout >> turn 结束信号
    D->>D: ProtocolAdapter.flush()

    Note over D,P: 等待用户下一条消息...

    alt 中断
        D->>P: signal(SIGINT)
    end

    alt 关闭
        D->>P: signal(SIGTERM)
        P-->>D: 进程退出
    end
```

## 7. Crate 文件结构

```
deapbox/
├── Cargo.toml                         # [workspace]
├── ARCHITECTURE.md                    # 本文档
│
├── deapbox-core/                      # 领域核心（零依赖外部）
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── types.rs                   # 类型定义（28 loc）
│       ├── traits.rs                  # trait 定义（36 loc）
│       ├── router.rs                  # 消息路由（stub）
│       └── agent_manager.rs           # 进程管理（stub）
│
├── deapbox-lark/                      # 飞书适配
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── event.rs
│       ├── api.rs
│       ├── card.rs
│       └── types.rs
│
├── deapbox-agent/                     # Agent 协议适配
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── protocol.rs               # PTY stdio 通信
│       ├── adapter.rs                # ProtocolAdapter trait + NormalizedEvent
│       ├── opencode.rs               # Opencode 适配
│       ├── codex.rs                  # Codex 适配
│       ├── claude_code.rs            # ClaudeCode 适配
│       └── kimi_code.rs             # KimiCode 适配
│
├── deapbox-store/                     # 持久化
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── store.rs                  # sled 实现
│       └── config.rs                 # TOML 解析
│
└── deapbox-cli/                       # 二进制入口
    ├── Cargo.toml
    └── src/
        ├── main.rs
        └── config.toml
```
