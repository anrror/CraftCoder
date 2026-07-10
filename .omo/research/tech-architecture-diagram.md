# AI Coding Agent 技术架构图

> **版本**: v1.0  
> **日期**: 2026-07-09  
> **模型家族**: Qwen3 Series (self-hosted via OpenAI-compatible API)  
> **后端存储**: MySQL + Redis  
> **Agent 运行时**: Rust (code-agent-rs workspace, 7 crates)

---

## 目录

1. [系统分层架构](#1-系统分层架构)
2. [OpenAI 兼容 API 网关集成](#2-openai-兼容-api-网关集成)
3. [组件关系图](#3-组件关系图)
4. [数据流图](#4-数据流图)
5. [ReAct 循环详细时序](#5-react-循环详细时序)
6. [部署架构](#6-部署架构)
7. [技术栈全景](#7-技术栈全景)

---

## 1. 系统分层架构

系统采用六层分层架构，自下而上依次为数据基础设施层、AI 模型层、工具集成层、代码理解层、Agent 引擎层和产品表现层。每一层通过明确定义的接口与相邻层交互。

```mermaid
flowchart TB
    subgraph L1["🎯 Product Surface Layer (产品表现层)"]
        direction LR
        CLI_TUI["CLI (TUI / Exec)"]
        IDE_Plugin["IDE Plugin (App Server)"]
        Web_API["Web API (REST / SSE)"]
    end

    subgraph L2["🧠 Agent Engine Layer (Agent 引擎层)"]
        direction LR
        ReActLoop["ReAct Loop Controller"]
        SessionMgr["Session Manager"]
        ToolRouter["Tool Router"]
        ContextMgr["Context Manager"]
        PromptEngine["Prompt Engine"]
        PlannerNode["Task Planner"]
        ReviewerNode["Reviewer / Judge"]
    end

    subgraph L3["🔍 Code Understanding Layer (代码理解层)"]
        direction LR
        ASTIndexer["AST Indexer (tree-sitter)"]
        CallGraph["Call Graph Builder"]
        HybridRetriever["Hybrid Retriever<br/>(BM25 + Vector)"]
        DepAnalyzer["Dependency Analyzer"]
        SymResolver["Symbol Resolver"]
        CodeEmbedder["Code Embedder"]
    end

    subgraph L4["🔧 Tool Integration Layer (工具集成层)"]
        direction LR
        ShellTool["Shell (PTY / Linux)"]
        LSPTool["LSP Client"]
        GitTool["Git Operations"]
        MCPClient["MCP Client"]
        FSysTool["Filesystem"]
        StaticAnalysis["Static Analysis<br/>(Clippy / Ruff)"]
        BrowserTool["Browser (Playwright)"]
    end

    subgraph L5["🤖 AI Model Layer (AI 模型层)"]
        direction LR
        Qwen27B["Qwen3.6-27B<br/>AEON-Ultimate<br/>NVFP4<br/>(LLM Core)"]
        QwenEmb8B["Qwen3-Embed-8B<br/>(Embedding)"]
        QwenRerank8B["Qwen3-Reranker-8B<br/>(Reranker)"]
        QwenVL8B["Qwen3-VL-8B<br/>Instruct-FP8<br/>(Vision)"]
        QwenGuard8B["Qwen3Guard-Gen-8B<br/>(Safety Guard)"]
    end

    subgraph L6["💾 Data & Infrastructure Layer (数据 & 基础设施层)"]
        direction LR
        MySQL[("MySQL<br/>10.97.127.59:3306")]
        Redis[("Redis<br/>10.97.236.199:6379")]
        Docker["Docker Runtime"]
        LocalFS["Local Filesystem"]
        SQLite["SQLite (Index Cache)"]
    end

    L1 --> L2
    L2 --> L3
    L2 --> L4
    L3 --> L5
    L4 --> L5
    L3 --> L6
    L4 --> L6
    L2 --> L6
```

**层间通信协议**:

| 交互方向 | 协议 | 说明 |
|----------|------|------|
| L1 → L2 | JSON-RPC 2.0 | CLI / IDE / Web 通过统一协议与 Agent Engine 通信 |
| L2 → L3 | Internal Function Calls | Rust 同步/异步函数调用 (零序列化开销) |
| L2 → L4 | MCP (Model Context Protocol) | 工具调用与结果返回 |
| L3 → L5 | OpenAI-compatible HTTP API | Embedding & Reranker 请求 |
| L4 → L5 | OpenAI-compatible HTTP API | LLM / Vision / Guard 请求 |
| L2/L3/L4 → L6 | TCP / Native FS | 数据库连接 (sqlx) + Redis (fred) + 文件系统 (tokio::fs) |

---

## 2. OpenAI 兼容 API 网关集成

所有 Qwen3 模型通过统一的 API 网关 (`https://prod-ai.isigning.cn/v1/`) 对外暴露，使用 OpenAI 兼容的 API 格式。Chat / Completions 接口被 LLM、Vision 和 Guard 三个模型复用，Embedding 和 Reranker 使用专用端点。

```mermaid
flowchart LR
    subgraph AgentEngine["Agent Engine (Rust Runtime)"]
        direction TB
        LLMClient["LLM Client<br/>(chat/completions)"]
        EmbedClient["Embedding Client<br/>(embeddings)"]
        RerankClient["Reranker Client<br/>(rerank)"]
        VisionClient["Vision Client<br/>(chat/completions)"]
        GuardClient["Guard Client<br/>(chat/completions)"]
    end

    subgraph APIGateway["API Gateway: https://prod-ai.isigning.cn/v1/"]
        subgraph ChatEndpoint["POST /v1/chat/completions"]
            ChatRouter["Path Router<br/>(model name dispatch)"]
        end
        EmbedEndpoint["POST /v1/embeddings"]
        RerankEndpoint["POST /v1/rerank"]
        APIAuth["API Key Manager<br/>(Unified Key → Rate Limit)"]
    end

    subgraph Qwen3Family["Qwen3 Model Family (Self-Hosted)"]
        QwenLLM["Qwen3.6-27B-AEON-Ultimate-NVFP4<br/>→ Reasoning + Code Generation"]
        QwenEmbed["Qwen3-Embedding-8B<br/>→ Code Vectorization"]
        QwenRerank["Qwen3-Reranker-8B<br/>→ Relevance Scoring"]
        QwenVL["Qwen3-VL-8B-Instruct-FP8<br/>→ Screenshot / UI Analysis"]
        QwenGuard["Qwen3Guard-Gen-8B<br/>→ Safety Review"]
    end

    LLMClient -->|"model: qwen3.6-27b-aeon"| ChatEndpoint
    VisionClient -->|"model: qwen3-vl-8b-instruct"| ChatEndpoint
    GuardClient -->|"model: qwen3guard-gen-8b"| ChatEndpoint
    EmbedClient -->|"input: code_snippets[]"| EmbedEndpoint
    RerankClient -->|"query + documents[]"| RerankEndpoint

    ChatEndpoint --> ChatRouter
    ChatRouter --> QwenLLM
    ChatRouter --> QwenVL
    ChatRouter --> QwenGuard
    EmbedEndpoint --> QwenEmbed
    RerankEndpoint --> QwenRerank

    APIAuth -.-> ChatEndpoint
    APIAuth -.-> EmbedEndpoint
    APIAuth -.-> RerankEndpoint
```

**API 端点矩阵**:

| 端点 | 方法 | 模型参数 | 用途 | 超时 |
|------|------|----------|------|------|
| `/v1/chat/completions` | POST | `qwen3.6-27b-aeon` | ReAct 推理、代码生成、规划 | 120s |
| `/v1/chat/completions` | POST | `qwen3-vl-8b-instruct` | 截图分析、UI 验证 | 60s |
| `/v1/chat/completions` | POST | `qwen3guard-gen-8b` | 生成代码安全审查 | 30s |
| `/v1/embeddings` | POST | `qwen3-embedding-8b` | 代码向量化检索 | 30s |
| `/v1/rerank` | POST | `qwen3-reranker-8b` | 检索结果重排序 | 15s |

**API 密钥管理策略**:
- 统一 API Key 全局鉴权，通过 `Authorization: Bearer <key>` Header 传递
- 基于模型的独立 Rate Limit (LLM: 60 RPM, Embedding: 200 RPM, Reranker: 300 RPM)
- 请求级 Trace ID (`x-trace-id`) 用于全链路 observability
- 失败自动重试: exponential backoff + jitter (max 3 retries)

---

## 3. 组件关系图

本图展示 Rust workspace 内部 7 个 crate 之间的依赖关系，以及它们对外部 Qwen3 模型家族和基础设施组件的调用关系。共 30+ 组件。

```mermaid
graph TB
    subgraph RustWorkspace["code-agent-rs Workspace (7 Crates)"]
        subgraph CoreCrate["core — Agent Core Engine"]
            ReActLoop["ReActLoop"]
            SessionMgr["SessionManager"]
            ContextMgr["ContextManager"]
            ToolRouter["ToolRouter"]
            PromptTempl["PromptTemplates"]
            PlanNode["PlannerNode"]
            ReviewNode["ReviewerNode"]
        end

        subgraph ToolsCrate["tools — Tool Implementations"]
            LSPClient["LSPClient"]
            ShellExec["ShellExecutor (PTY)"]
            GitOps["GitOperations"]
            FSysOps["FilesystemOps"]
            MCPAdapter["MCPAdapter"]
            LintRunner["LintRunner<br/>(Clippy / Ruff)"]
            BrowserOps["BrowserOps<br/>(Playwright)"]
        end

        subgraph CodexCrate["codex — Code Understanding"]
            ASTParser["ASTParser<br/>(tree-sitter)"]
            CallGraphB["CallGraphBuilder"]
            HybridRet["HybridRetriever"]
            DepAnalyze["DependencyAnalyzer"]
            SymResolve["SymbolResolver"]
            EmbedBridge["EmbeddingBridge"]
        end

        subgraph ProtocolCrate["protocol — Shared Types"]
            AgentMsg["AgentMessage"]
            ToolDef["ToolDefinition"]
            SessionState["SessionState"]
            TurnRecord["TurnRecord"]
            CodeContext["CodeContext"]
            EvalReport["EvaluationReport"]
        end

        subgraph CLICrate["cli — CLI Binary"]
            TUIApp["TUI Application"]
            ExecMode["Exec Mode"]
            Config["Configuration Loader"]
        end

        subgraph AppServerCrate["app-server — IDE Backend"]
            JSONRPCSrv["JSON-RPC Server"]
            WSServer["WebSocket Server"]
            EventBus["Event Bus"]
        end

        subgraph EvalCrate["eval — Evaluation Framework"]
            BenchmarkRunner["Benchmark Runner"]
            MetricCollector["Metric Collector"]
            SWEAdapter["SWE-bench Adapter"]
            HumanEvalAdapter["HumanEval Adapter"]
        end
    end

    subgraph Qwen3Models["Qwen3 Model Family @ prod-ai.isigning.cn"]
        QwenLLM_27B["Qwen3.6-27B-AEON<br/>NVFP4"]
        QwenEmb_8B["Qwen3-Embed-8B"]
        QwenRerank_8B["Qwen3-Reranker-8B"]
        QwenVL_8B["Qwen3-VL-8B-FP8"]
        QwenGuard_8B["Qwen3Guard-Gen-8B"]
    end

    subgraph Infrastructure["Infrastructure"]
        MySQL_DB[("MySQL<br/>10.97.127.59:3306")]
        Redis_Cache[("Redis<br/>10.97.236.199:6379")]
        LocalFS_Disk["Local FS"]
        SQLite_Cache["SQLite (Index)"]
    end

    subgraph ExternalTools["External Tools (via MCP)"]
        LangServers["Language Servers<br/>(gopls / rust-analyzer / pylsp)"]
        GitProcess["git CLI"]
        ShellProc["Shell / PTY"]
        ChromeBrowser["Chrome DevTools"]
    end

    %% Crate dependencies
    CLICrate --> CoreCrate
    CLICrate --> ProtocolCrate
    AppServerCrate --> CoreCrate
    AppServerCrate --> ProtocolCrate
    CoreCrate --> ProtocolCrate
    CoreCrate --> ToolsCrate
    CoreCrate --> CodexCrate
    ToolsCrate --> ProtocolCrate
    ToolsCrate --> MCPAdapter
    CodexCrate --> ProtocolCrate
    EvalCrate --> CoreCrate
    EvalCrate --> ProtocolCrate
    EvalCrate --> CodexCrate

    %% External tool connections
    LSPClient --> LangServers
    ShellExec --> ShellProc
    GitOps --> GitProcess
    BrowserOps --> ChromeBrowser

    %% Qwen3 connections
    ReActLoop -->|"chat/completions"| QwenLLM_27B
    PlanNode -->|"chat/completions"| QwenLLM_27B
    ReviewNode -->|"chat/completions"| QwenLLM_27B
    EmbedBridge -->|"embeddings"| QwenEmb_8B
    HybridRet -->|"rerank"| QwenRerank_8B
    BrowserOps -->|"chat/completions<br/>(vision)"| QwenVL_8B
    ReviewNode -->|"chat/completions<br/>(safety)"| QwenGuard_8B

    %% Infrastructure connections
    SessionMgr --> MySQL_DB
    SessionMgr --> Redis_Cache
    SessionMgr --> SQLite_Cache
    ContextMgr --> MySQL_DB
    ContextMgr --> Redis_Cache
    FSysOps --> LocalFS_Disk
    ASTParser --> LocalFS_Disk
    CallGraphB --> SQLite_Cache
    HybridRet --> SQLite_Cache
    MetricCollector --> MySQL_DB

    %% Style
    classDef qwen fill:#1a1a2e,stroke:#e94560,color:#fff
    classDef infra fill:#0f3460,stroke:#16c79a,color:#fff
    classDef tools fill:#16213e,stroke:#ffd369,color:#fff
    class QwenLLM_27B,QwenEmb_8B,QwenRerank_8B,QwenVL_8B,QwenGuard_8B qwen
    class MySQL_DB,Redis_Cache,LocalFS_Disk,SQLite_Cache infra
    class LangServers,GitProcess,ShellProc,ChromeBrowser tools
```

**Qwen3 模型调用矩阵**:

| 调用方 (Crate / Module) | 目标模型 | 调用接口 | 典型调用频率 | Token 预算 |
|--------------------------|----------|----------|-------------|-----------|
| `ReActLoop` | Qwen3.6-27B | `chat/completions` | 3-30 次/任务 | 8K-32K per call |
| `PlanNode` | Qwen3.6-27B | `chat/completions` | 1 次/任务 | 4K-8K |
| `ReviewNode` | Qwen3.6-27B | `chat/completions` | 1-2 次/任务 | 4K-16K |
| `ReviewNode` (Safety) | Qwen3Guard-8B | `chat/completions` | 1 次/代码块 | 2K-4K |
| `EmbedBridge` | Qwen3-Embed-8B | `embeddings` | 批量 (N 个代码块) | N/A |
| `HybridRetriever` | Qwen3-Reranker-8B | `rerank` | 1 次/检索 | N/A |
| `BrowserOps` | Qwen3-VL-8B | `chat/completions` | 按需 (截图分析) | 2K + image |

---

## 4. 数据流图

完整的 8 步数据流，展示从用户输入到最终输出的全过程。每一步涉及的具体组件、模型调用和数据持久化均已标注。

```mermaid
sequenceDiagram
    actor User as 👤 用户
    participant CLI as CLI / IDE / Web
    participant Session as Session Manager
    participant Plan as Task Planner
    participant React as ReAct Loop
    participant Context as Context Manager
    participant CodeU as Code Understanding
    participant Tools as Tool Executor
    participant Embed as Qwen3-Embed-8B
    participant Rerank as Qwen3-Reranker-8B
    participant LLM as Qwen3.6-27B
    participant Guard as Qwen3Guard-8B
    participant MySQL as MySQL
    participant Redis as Redis

    Note over User,Redis: === Step 1: User Input & Session Init ===
    User->>CLI: "修复 src/auth/login.rs 中的认证 Bug"
    CLI->>Session: create_session(user_id, task)
    Session->>MySQL: INSERT INTO sessions (...)
    Session->>Redis: SET session:{id}:state = "active"
    Session-->>CLI: session_id = "ses_abc123"

    Note over User,Redis: === Step 2: Task Planning (Qwen3.6-27B) ===
    CLI->>Plan: plan_task(session_id, task_desc)
    Plan->>LLM: chat/completions<br/>(system: planner prompt + task)
    LLM-->>Plan: Task Plan JSON<br/>[Step1: locate auth code, Step2: trace call chain, ...]
    Plan->>Session: attach_plan(plan_json)
    Session->>MySQL: UPDATE sessions SET plan = ...

    Note over User,Redis: === Step 3: Code Retrieval (Qwen3-Embed + Qwen3-Rerank) ===
    Plan->>CodeU: retrieve_context(query="auth login bug")
    CodeU->>Embed: POST /v1/embeddings<br/>(input: "auth login bug")
    Embed-->>CodeU: query_vector[1024]
    CodeU->>CodeU: BM25 + Vector Hybrid Search<br/>(against SQLite code index)
    CodeU->>Rerank: POST /v1/rerank<br/>(query + top-20 candidates)
    Rerank-->>CodeU: reranked_top5[score>0.7]
    CodeU->>Context: inject_context(session_id, files[], symbols[])
    Context->>Redis: SET session:{id}:context = serialized

    Note over User,Redis: === Step 4: ReAct Loop — Reasoning (Qwen3.6-27B) ===
    React->>Context: get_active_context(session_id)
    Context->>Redis: GET session:{id}:context
    Context-->>React: messages[] + code_context[]
    React->>LLM: chat/completions<br/>(messages + tools_schema + context)
    LLM-->>React: Thought: "Need to read auth/login.rs"<br/>ToolCall: read_file(path="src/auth/login.rs")

    Note over User,Redis: === Step 5: Tool Execution ===
    React->>Tools: execute(tool_call)
    Tools->>Tools: fs.read("src/auth/login.rs")
    Tools-->>React: ToolResult{ output: file_contents, 120 lines }
    React->>Context: append_turn(thought, tool_call, result)
    React->>Session: increment_turn_count()

    Note over User,Redis: === Step 6: ReAct Loop — Iteration (Qwen3.6-27B) ===
    React->>LLM: chat/completions<br/>(updated messages + new context)
    LLM-->>React: Thought: "Bug in validate_token()"<br/>ToolCall: edit_file(...)
    React->>Tools: execute(edit_file)
    Tools-->>React: ToolResult{ applied: true, diff: "..." }

    Note over User,Redis: === Step 7: Safety Review (Qwen3Guard-8B) ===
    React->>Guard: chat/completions<br/>(system: safety check, input: generated_code_diff)
    Guard-->>React: SafetyReport{ safe: true, risk_score: 0.02 }
    Note right of Guard: risk_score < 0.3 → pass<br/>risk_score 0.3-0.7 → warn<br/>risk_score > 0.7 → block

    Note over User,Redis: === Step 8: Persistence & Output ===
    React->>Session: complete_turn(final_output)
    Session->>MySQL: INSERT INTO turns (session_id, turn_n, input, output, tokens_used, ...)
    Session->>Redis: EXPIRE session:{id}:state 3600
    React-->>CLI: Final Response: "修复完成: validate_token() 中..." 
    CLI-->>User: ✅ 代码修复结果 + 变更摘要
```

**数据流步骤总结**:

| 步骤 | 阶段 | 核心模型 | 数据落盘 |
|------|------|----------|----------|
| 1 | 会话初始化 | — | MySQL (`sessions`), Redis (`session:{id}:state`) |
| 2 | 任务规划 | Qwen3.6-27B | MySQL (`sessions.plan`) |
| 3 | 代码检索 | Qwen3-Embed-8B + Qwen3-Reranker-8B | Redis (`session:{id}:context`) |
| 4 | ReAct 推理 | Qwen3.6-27B | — (context in Redis) |
| 5 | 工具执行 | — | Filesystem (actual edits) |
| 6 | ReAct 迭代 | Qwen3.6-27B | Redis (updated context) |
| 7 | 安全审查 | Qwen3Guard-8B | — (inline check) |
| 8 | 持久化输出 | — | MySQL (`turns`), Redis (expire) |

---

## 5. ReAct 循环详细时序

ReAct 循环是系统核心的控制原语。每次循环包含三个阶段：**Thought** (推理) → **Act** (工具调用) → **Observe** (结果观察)。循环终止于模型输出最终答案而非工具调用。

```mermaid
sequenceDiagram
    actor User as 👤 用户
    participant CLI as CLI Interface
    participant React as ReAct Controller
    participant Context as Context Manager
    participant LLM as Qwen3.6-27B-AEON
    participant Tools as Tool Router
    participant Shell as Shell Executor
    participant FS as Filesystem
    participant LSP as LSP Client
    participant Git as Git Ops
    participant Guard as Qwen3Guard-8B
    participant MySQL as MySQL

    User->>CLI: "实现 UserService.create() 方法"
    CLI->>React: start_loop(task, plan)

    loop ReAct Loop (Turn 1..N, max_turns=30)
        Note over React,LLM: === Phase 1: Context Assembly ===
        React->>Context: build_prompt(session_id)
        Context->>Context: budget_allocation:<br/>system(4K) + plan(2K) + tools(5K)<br/>+ context(10K) + history(8K) + reserve(8K)
        Context-->>React: assembled_messages[] + tools_schema[]

        Note over React,LLM: === Phase 2: Model Inference (Qwen3.6-27B) ===
        React->>LLM: POST /v1/chat/completions<br/>{ model: "qwen3.6-27b-aeon",<br/>  messages: [...],<br/>  tools: [read_file, write_file, bash, ...],<br/>  max_tokens: 8192 }
        LLM-->>React: Response{<br/>  content: "Thought: 首先需要理解项目结构...",<br/>  tool_calls: [{ name: "read_file", args: {path: "src/service/user.rs"} }]<br/>}

        alt has_tool_calls = true
            Note over React,Tools: === Phase 3: Tool Execution ===
            React->>Tools: dispatch(tool_calls[])

            par Parallel Tool Calls (independent)
                Tools->>FS: read_file("src/service/user.rs")
                FS-->>Tools: file_contents (120 lines)
            and
                Tools->>Shell: bash("cargo check 2>&1")
                Shell-->>Tools: compiler_output
            and
                Tools->>LSP: find_references("User")
                LSP-->>Tools: references_list[]
            end

            Tools-->>React: tool_results[]

            Note over React,Context: === Phase 4: Observation & Update ===
            React->>Context: append_observation(turn_results)
            Context->>Context: check_budget_threshold<br/>70%: monitor, 85%: compact, 95%: evict
            Context->>Context: maybe_auto_compact() if budget > 85%

            Note over React,Guard: === Phase 5: Optional Safety Check ===
            alt tool involves code generation
                React->>Guard: POST /v1/chat/completions<br/>{ model: "qwen3guard-gen-8b",<br/>  messages: [{role:"system", content:"safety check..."}]}
                Guard-->>React: SafetyReport{ safe: true }
            end

            React->>React: turn_count += 1
            Note right of React: continue loop

        else has_tool_calls = false (final answer)
            Note over React,CLI: === Phase 6: Loop Termination ===
            React->>React: emit final_answer
            React->>MySQL: INSERT INTO turns<br/>(session_id, turn_n, thought, action, observation,<br/> tokens_used, latency_ms, success)
            React->>CLI: final_response
            CLI-->>User: ✅ 实现结果 + 变更摘要
        end
    end
```

**ReAct 循环关键参数**:

| 参数 | 值 | 说明 |
|------|-----|------|
| `max_turns` | 30 | 最大循环迭代次数 (硬限制) |
| `turn_timeout` | 120s | 单轮推理 + 工具执行超时 |
| `token_budget` | 128K | Qwen3.6-27B 上下文窗口 (NVFP4) |
| `budget_warn` | 70% (~90K) | 触发监控告警 |
| `budget_compact` | 85% (~109K) | 触发上下文压缩 |
| `budget_evict` | 95% (~122K) | 触发激进淘汰 |
| `max_parallel_tools` | 6 | 单轮最大并行工具调用数 |
| `guard_check_freq` | every_code_gen | 每次代码生成后执行安全审查 |

**上下文预算分配 (128K total)**:

```
┌────────────────────────────────────────────────────────────┐
│  System Prompt + Tool Definitions  │  ~9K  │  Static (cached) │
├────────────────────────────────────┼───────┼──────────────────┤
│  Task Plan + Hypothesis            │  ~2K  │  Always resident  │
├────────────────────────────────────┼───────┼──────────────────┤
│  Retrieved Code Context            │  ~10K │  Top-5 reranked    │
├────────────────────────────────────┼───────┼──────────────────┤
│  Tool Results (current turn)       │  ~10K │  JIT, evicted next │
├────────────────────────────────────┼───────┼──────────────────┤
│  Conversation History (compacted)  │  ~8K  │  Compacted >85%   │
├────────────────────────────────────┼───────┼──────────────────┤
│  Model Reply Headroom              │  ~8K  │  Reserved          │
├────────────────────────────────────┼───────┼──────────────────┤
│  Safety Margin                     │  ~5K  │  Buffer            │
└────────────────────────────────────┴───────┴──────────────────┘
```

---

## 6. 部署架构

系统支持三种部署模式，共享同一套 Rust Agent Core 和 Qwen3 模型后端。

```mermaid
flowchart TB
    subgraph Mode1["Mode 1: 本地 CLI 模式 (直连 Qwen3 API)"]
        direction TB
        CLI1["code-agent-cli (Rust Binary)"]
        CLI1 -->|"HTTPS"| QwenAPI1["Qwen3 API Gateway<br/>prod-ai.isigning.cn"]
        CLI1 -->|"local"| LocalFS1["Local Filesystem"]
        Note1["适用: 个人开发者<br/>特点: 零依赖 (单二进制), 本地文件直接操作"]
    end

    subgraph Mode2["Mode 2: IDE 插件模式 (App Server + Qwen3 API)"]
        direction TB
        VSCodeExt["VS Code Extension<br/>(TypeScript)"]
        VSCodeExt -->|"JSON-RPC / WS"| AppSrv["code-agent-app-server<br/>(Rust, port 24680)"]
        AppSrv -->|"HTTPS"| QwenAPI2["Qwen3 API Gateway<br/>prod-ai.isigning.cn"]
        AppSrv -->|"local"| LocalFS2["Local Filesystem"]
        Note2["适用: IDE 集成开发<br/>特点: App Server 管理 Agent 生命周期, IDE 轻量通信"]
    end

    subgraph Mode3["Mode 3: Web 服务模式 (后端 + DB + Redis + Qwen3 API)"]
        direction TB
        WebUI["Web Frontend<br/>(React / Next.js)"]
        WebUI -->|"REST / SSE"| Backend["code-agent-app-server<br/>(Rust, Docker)"]
        Backend -->|"HTTPS"| QwenAPI3["Qwen3 API Gateway<br/>prod-ai.isigning.cn"]
        Backend -->|"TCP:3306"| MySQL3[("MySQL<br/>10.97.127.59:3306")]
        Backend -->|"TCP:6379"| Redis3[("Redis<br/>10.97.236.199:6379")]
        Backend -->|"local"| DockerFS["Docker Volume<br/>(Workspace FS)"]
        Note3["适用: 团队协作 / SaaS<br/>特点: 多用户隔离, 持久化存储, 共享 Qwen3 推理资源"]
    end

    subgraph Shared["共享基础设施"]
        QwenAPI["Qwen3 API Gateway<br/>https://prod-ai.isigning.cn/v1/"]
        QwenLLM_S["Qwen3.6-27B-AEON-NVFP4"]
        QwenEmb_S["Qwen3-Embed-8B"]
        QwenRerank_S["Qwen3-Reranker-8B"]
        QwenVL_S["Qwen3-VL-8B-FP8"]
        QwenGuard_S["Qwen3Guard-8B"]
        QwenAPI --> QwenLLM_S
        QwenAPI --> QwenEmb_S
        QwenAPI --> QwenRerank_S
        QwenAPI --> QwenVL_S
        QwenAPI --> QwenGuard_S
    end

    QwenAPI1 -.-> QwenAPI
    QwenAPI2 -.-> QwenAPI
    QwenAPI3 -.-> QwenAPI
```

**部署模式对比**:

| 维度 | Mode 1: CLI | Mode 2: IDE Plugin | Mode 3: Web Service |
|------|-------------|-------------------|---------------------|
| **目标用户** | 个人开发者 | IDE 用户 | 团队 / SaaS |
| **Rust Binary 数** | 1 (cli) | 2 (cli + app-server) | 2 (app-server + 可选 cli) |
| **端口占用** | 无 | localhost:24680 | 0.0.0.0:8080 |
| **数据库** | 无 (可选 SQLite) | MySQL (远程) + Redis (远程) | MySQL + Redis |
| **多用户隔离** | N/A (单用户) | 系统用户隔离 | session-based 隔离 |
| **Web UI** | 无 | VS Code Extension | React SPA |
| **部署复杂度** | 最小 (单二进制) | 中等 (Extension + Server) | 最高 (Docker Compose) |
| **Qwen3 连接** | 直连 API | 直连 API | 直连 API (pooled) |
| **冷启动时间** | <100ms | <500ms (App Server) | <5s (Docker) |

---

## 7. 技术栈全景

### 7.1 分层技术栈

| 层级 | 组件 | 技术选型 | 版本 / 配置 |
|------|------|----------|-------------|
| **产品表现层** | CLI | Ratatui (Rust TUI) | 0.28+ |
| | IDE Plugin | TypeScript + VS Code Extension API | VS Code 1.95+ |
| | Web 前端 | React 19 + Next.js 15 + Tailwind CSS 4 | Node.js 22 |
| | 通信协议 | JSON-RPC 2.0 / WebSocket / SSE | — |
| **Agent 引擎层** | 运行时 | Rust (tokio async runtime) | Rust 1.80+, tokio 1.x |
| | 控制循环 | Self-built ReAct Loop | max 30 turns |
| | 任务规划 | Chain-of-Thought + Plan-and-Execute | Qwen3.6-27B 驱动 |
| | 代码审查 | Reviewer Node (独立 LLM 调用) | Qwen3Guard-8B 安全审查 |
| | 上下文管理 | 5-layer compaction (inspired by Claude Code) | 70% / 85% / 95% 阈值 |
| | 提示工程 | Tera template engine | 动态模板注入 |
| **代码理解层** | AST 解析 | tree-sitter (WASM Runtime) | 100+ 语言语法 |
| | 调用图 | Static Call Graph (scope-aware resolution) | SQLite 存储 |
| | 向量检索 | Hybrid Retriever (BM25 + Dense Vector) | RRF fusion |
| | 依赖分析 | Dependency Graph + Tarjan's SCC | 循环检测 |
| | 符号解析 | Symbol-name-based resolution | 跨 turn 稳定 ID |
| | 语义搜索 | Hybrid (lexical + semantic) | FTS5 + Embedding |
| **工具集成层** | Shell | PTY-based executor (async process) | Linux / WSL / macOS |
| | LSP | agent-lsp inspired MCP bridge | 30+ 语言 LS 支持 |
| | Git | libgit2 binding (git2 crate) | 原子操作 |
| | MCP Client | Custom Rust MCP client (stdlib / SSE) | MCP 2024-11-05 |
| | Filesystem | tokio::fs (async I/O) | watch / notify |
| | 静态分析 | Clippy (Rust), Ruff (Python), ESLint (TS) | CLI 子进程 |
| | 浏览器 | Playwright (Chromium / Firefox / WebKit) | MCP bridge |
| **AI 模型层** | LLM Core | **Qwen3.6-27B-AEON-Ultimate-NVFP4** | 128K context, FP4 量化 |
| | Embedding | **Qwen3-Embedding-8B** | 1024 dims, chunk c1500-o200 |
| | Reranker | **Qwen3-Reranker-8B** | top-20 → top-5 重排 |
| | Vision | **Qwen3-VL-8B-Instruct-FP8** | 8B params, FP8 |
| | Safety Guard | **Qwen3Guard-Gen-8B** | 8B params, risk_score 0-1 |
| | API 网关 | OpenAI-compatible REST API | `https://prod-ai.isigning.cn/v1/` |
| **数据 & 基础设施层** | 关系数据库 | MySQL 8.0 | 10.97.127.59:3306 |
| | 缓存 / 状态 | Redis 7.x | 10.97.236.199:6379 |
| | 索引缓存 | SQLite 3.x (WAL mode) | 本地嵌入式 |
| | 容器化 | Docker + Docker Compose | 部署编排 |
| | 文件系统 | Native FS (workspace root) | 用户工作区 |
| **评估 & 观测** | 基准测试 | SWE-bench-Live, HumanEval, MBPP | 自定义 runner |
| | 指标收集 | Metrics (turn_count, tokens_used, latency, success_rate) | MySQL |
| | 链路追踪 | W3C Trace Context (x-trace-id) | MCP 兼容 |
| | 日志 | tracing crate (structured logging) | JSON / Console |

### 7.2 Qwen3 模型家族核心位置

Qwen3 模型家族是系统的**唯一推理引擎**，所有 AI 能力均由 self-hosted 的 Qwen3 模型提供。以下是 Qwen3 模型在本系统中的关键角色：

| 模型 | 参数量 | 核心职责 | 调用入口 | 典型延迟 |
|------|--------|----------|----------|----------|
| **Qwen3.6-27B-AEON-Ultimate-NVFP4** | 27B | 代码推理与生成 (ReAct 循环核心) | `ReActLoop`, `PlannerNode`, `ReviewerNode` | 2-8s |
| **Qwen3-Embedding-8B** | 8B | 代码语义向量化 (BM25+Vector 混合检索) | `EmbeddingBridge` → `HybridRetriever` | 200-500ms |
| **Qwen3-Reranker-8B** | 8B | 检索结果相关性重排序 (top-20 → top-5) | `HybridRetriever` | 100-300ms |
| **Qwen3-VL-8B-Instruct-FP8** | 8B | 截图分析 / UI 验证 / 可视化 QA | `BrowserOps` (vision mode) | 1-3s |
| **Qwen3Guard-Gen-8B** | 8B | 生成代码安全审查 (risk scoring 0-1) | `ReviewerNode` (safety path) | 500ms-1s |

### 7.3 Rust Workspace Crate 职责矩阵

| Crate | 包名 | 职责 | 关键依赖 |
|-------|------|------|----------|
| **protocol** | `code-agent-protocol` | 共享数据类型 (AgentMessage, ToolDefinition, SessionState, TurnRecord, CodeContext, EvaluationReport) | serde, serde_json, derive_more |
| **core** | `code-agent-core` | Agent 核心引擎 (ReActLoop, SessionManager, ContextManager, ToolRouter, PlannerNode, ReviewerNode, PromptTemplates) | protocol, tools, codex |
| **tools** | `code-agent-tools` | 工具实现 (LSPClient, ShellExecutor, GitOperations, FilesystemOps, MCPAdapter, LintRunner, BrowserOps) | protocol |
| **codex** | `code-agent-codex` | 代码理解 (ASTParser, CallGraphBuilder, HybridRetriever, DependencyAnalyzer, SymbolResolver, EmbeddingBridge) | protocol, tree-sitter |
| **cli** | `code-agent-cli` | CLI 二进制 (TUI Application, Exec Mode, Configuration Loader) | core, protocol |
| **app-server** | `code-agent-app-server` | IDE 后端服务 (JSON-RPC Server, WebSocket Server, Event Bus) | core, protocol |
| **eval** | `code-agent-eval` | 评估框架 (BenchmarkRunner, MetricCollector, SWEAdapter, HumanEvalAdapter) | core, codex, protocol |

### 7.4 外部依赖与基础设施

| 组件 | 地址 | 协议 | 用途 |
|------|------|------|------|
| Qwen3 API Gateway | `https://prod-ai.isigning.cn/v1/` | HTTPS / OpenAI-compatible | 所有 AI 推理请求 |
| MySQL | `10.97.127.59:3306` | TCP / MySQL Protocol | 会话持久化、评估数据、配置 |
| Redis | `10.97.236.199:6379` | TCP / Redis Protocol | 会话状态缓存、上下文缓存、限流 |
| SQLite | `<workspace>/.code-agent/index.db` | 本地文件 | 代码索引缓存 (tree-sitter AST + FTS5) |
| Language Servers | 系统 PATH | stdio / TCP via MCP | LSP 代码智能 (gopls, rust-analyzer, pylsp, etc.) |
| Docker | 系统 daemon | Unix Socket / TCP | 容器化部署与资源隔离 |

---

> *文档由 AI Coding Agent 技术架构研究组编制，基于 code-agent-rs workspace 实际结构与 Qwen3 模型家族部署配置。所有 API 地址、数据库连接信息均为实际环境约束。*
