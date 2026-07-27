# code-agent-rs — 定制码匠核心引擎

**code-agent-rs** 是一个 Rust 工作区，为 AI 编码代理提供模块化、可扩展的核心引擎。它包含 7 个主要 crate 和 2 个辅助 crate，覆盖从协议定义、Agent 推理循环、工具链集成到代码理解、评估框架和 IDE 接入的完整链路。

工作区采用领域驱动设计（DDD）组织，每个 crate 对应一个独立的限界上下文（Bounded Context），通过 trait 接口解耦，支持多供应商替换和渐进式组合。

### 设计目标

- **模块化** — 每个 crate 职责单一，可独立编译、测试和替换
- **可扩展** — 核心 trait 驱动架构，新增模型供应商或工具无需修改核心逻辑
- **安全优先** — 工具执行内建沙箱和安全审查，权限控制贯穿全链路
- **可观测** — 结构化日志和分布式追踪默认开启，生产环境可切换 OTLP 导出
- **流式交互** — 所有模型交互采用 SSE 流式协议，支持实时事件推送

---

## 工作区结构

```
code-agent-rs/
├── protocol/          # 共享协议类型与核心数据模型
├── core/              # Agent 引擎核心（ReAct Loop, ModelClient, ToolRegistry, Safety, Observability, Persistence）
├── tools/             # 工具链（Shell, LSP, Git, MCP）
├── codex/             # 代码理解（Indexer, Graph, Retrieval, Context, Watcher）
├── eval/              # 评估框架（Runner, Adapters, Metrics）
├── eval-runner/       # 评估 CLI 入口（CI 流水线专用）
├── cli/               # CLI 入口（TUI, Exec, Config）
├── app-server/        # JSON-RPC 2.0 App Server（IDE 集成）
└── code-agent-web/    # Axum REST + SSE Web API（Web 集成）
```

---

## Crate 详解

### protocol — 共享协议类型

**路径**: `protocol/`
**包名**: `code-agent-protocol`
**作用**: 定义工作区中所有 crate 共同依赖的基础类型。所有公开类型均实现 `Serialize`、`Deserialize`、`Clone` 和 `Debug`。

按 DDD 四层组织：

| 层级 | 模块 | 核心类型 | 职责 |
|------|------|---------|------|
| 标识层 | `identity` | `SessionId`, `ThreadId`, `TurnId` | 领域标识值对象 |
| 消息层 | `message` | `Message`, `ToolCall`, `ToolResultMessage` | 对话领域事件 |
| 执行层 | `execution` | `TurnInput`, `ResponseEvent` | 轮次生命周期事件流 |
| 配置层 | `config` | `SessionStatus`, `PermissionMode`, `CapabilityLevel` | 状态与安全策略值对象 |

**关键类型**:
- `SessionId` / `ThreadId` / `TurnId` — 不可变标识值对象，支持 `From<String>` 和 `Display`
- `Message` — 枚举，覆盖 `UserMessage`、`AssistantMessage`、`ToolCall`、`ToolResult` 四种对话形式
- `ResponseEvent` — 枚举，以事件溯源模式描述 Agent 完整执行过程：`TurnStarted`、`AgentMessageDelta`、`ToolCallBegin`、`ToolCallEnd`、`TurnComplete`、`Error`、`TokenUsage`
- `TurnInput` — 轮次输入命令，包含 `thread_id` 和消息列表
- `SessionStatus` — `Active` / `Paused` / `Completed` / `Archived`
- `PermissionMode` — `Auto` / `Permit` / `Block`
- `CapabilityLevel` — `Read` / `Edit` / `Exec`（实现 `PartialOrd`）

**依赖**: serde, serde_json, derive_more

**测试覆盖**:
- 所有类型的 JSON 序列化/反序列化往返测试
- 完整轮次生命周期事件流测试（从 TurnStarted 到 TurnComplete）
- 边界情况测试（空消息列表、null 字段、畸形 JSON）
- 枚举值 JSON 格式验证（snake_case 命名约定）

---

### core — Agent 引擎核心

**路径**: `core/`
**包名**: `code-agent-core`
**作用**: 整个系统的核心引擎，按 DDD 限界上下文组织为 9 个模块。

**模块与关键类型**:

| 模块 | 限界上下文 | 核心类型 / Trait | 职责 |
|------|-----------|-----------------|------|
| `agent` | Agent 核心上下文 | `Session`, `ThreadManager`, `ReActLoop` | ReAct 推理循环、会话生命周期、线程管理、多智能体协作 |
| `context` | 上下文管理上下文 | `ContextCompactor`, `TokenBudget` | 5 层渐进式上下文压缩管道、Token 预算管理 |
| `model` | 模型抽象上下文 | `ModelClient` trait, `ToolDefinition`, `TokenUsage` | 供应商无关的模型客户端接口 + Qwen3 实现 |
| `tools` | 工具体系上下文 | `Tool` trait, `ToolRegistry` | 工具接口、注册中心、路由与权限控制 |
| `safety` | 安全防护上下文 | `SafetyClient`, `ContentGuard` | 提示注入防御、内容安全审查、Qwen3Guard 集成 |
| `observability` | 可观测性上下文 | `Telemetry`, `StructuredLogger` | OpenTelemetry 追踪、结构化 JSON 日志（feature gate） |
| `feedback` | 反馈收集上下文 | `FeedbackCollector` | 用户反馈采集与分析 |
| `flywheel` | 飞轮分析上下文 | `FlywheelCollector`, `FailureAnalyzer`, `ImprovementSuggester`, `NightlyPipeline` | 错误追踪收集、失败聚类分析、改进建议生成、夜间管道 |
| `tools/hook` | 工具生命周期上下文 | `ToolHook` trait, `HookRegistry`, `ToolEvent`（8 种事件） | Phase F 钩子系统：工具执行前后事件拦截 |
| `tools/plugin` | 插件上下文 | `Plugin` trait, `PluginManager`, `McpConnector` | Phase F 动态工具加载、MCP 连接器抽象 |
| `tools/command` | 命令上下文 | `Command`, `CommandRegistry`, `/help`, `/status` | Phase F 斜杠命令系统 |
| `persistence` | 持久化上下文 | `SessionStore`, `SnapshotManager` | 会话状态保存、恢复与快照（基于 rusqlite） |

**关键 Trait**:
- `ModelClient` — 异步流式模型客户端接口，`complete_stream()` 返回 `Stream<Item = ResponseEvent>`
- `Tool` — 工具接口，统一 `execute()` 签名

**Feature flags**:
- `observability`（默认开启）— 启用 tracing-subscriber 和 tracing-appender
- `otlp` — 启用 OpenTelemetry OTLP 导出（opentelemetry SDK）

**依赖**: code-agent-protocol, reqwest, tokio, async-trait, tracing, rusqlite, uuid, chrono

**测试策略**:
- 使用 `httpmock` 模拟外部 API 响应
- 持久化层使用 `tempfile` 创建临时 SQLite 数据库
- 模型客户端测试覆盖正常流、错误流和超时场景

---

### tools — 工具链

**路径**: `tools/`
**包名**: `code-agent-tools`
**作用**: 封装与外部系统交互的四个核心工具领域，为上层 Agent 提供统一、安全、可审计的工具调用接口。

**模块与关键类型**:

| 模块 | 核心类型 | 职责 |
|------|---------|------|
| `shell` | `ShellExecutor`, `SandboxConfig` | 沙箱化 Shell 命令执行（bubblewrap / Docker / 无限制回退） |
| `lsp` | `LspClient`, `LspDiagnostics` | 语言服务器协议集成，提供代码智能（跳转定义、引用查找、悬停提示、诊断） |
| `git` | `GitClient`, `GitDiff`, `GitCommit` | Git 源码控制操作，带安全策略的增删改查 |
| `mcp` | `McpClient`, `McpServer`, `McpRegistry` | MCP（模型上下文协议）客户端与服务器实现，用于工具注册与远程调用 |

**Feature flags**:
- `lsp`（默认开启）— 启用 LSP 模块
- `mcp`（默认开启）— 启用 MCP 模块

**依赖**: code-agent-core, code-agent-protocol, git2, tokio, reqwest, which

**Shell 模块详解**:
- 支持三种执行模式：无限制（本地直接执行）、bubblewrap（Linux 沙箱）、Docker 容器隔离
- 自动检测系统可用沙箱能力，按优先级降级；`SandboxBackend::None`（无沙箱可用时）拒绝执行，需设置 `CODE_AGENT_ALLOW_UNRESTRICTED=true` 才能回退到无限制模式（CWE-250 门控）
- Shell 元字符检测（`$ | ; & \` `` 等）阻止命令注入攻击（CWE-78 防护）
- 命令执行超时控制、输出大小限制、环境变量过滤

**LSP 模块详解**:
- 基于 JSON-RPC 2.0 协议与语言服务器通信
- 支持诊断收集、跳转定义、查找引用、悬停提示
- 内置 mock-lsp-server 二进制用于集成测试

**Git 模块详解**:
- 基于 git2（libgit2 绑定）实现，无需安装独立 Git CLI
- 支持 diff 生成、提交创建、分支操作、状态查询
- 操作前进行安全策略检查（禁止操作范围外的仓库）
- `build_signature()` 拒绝硬编码身份，要求通过 git config 或 `CODE_AGENT_GIT_AUTHOR_NAME` / `CODE_AGENT_GIT_AUTHOR_EMAIL` 环境变量提供作者信息（CWE-290 身份验证）

**MCP 模块详解**:
- 实现模型上下文协议（Model Context Protocol）客户端
- 支持远程工具发现、注册和调用
- 与 MCP 服务器通过 JSON-RPC 2.0 over stdio 通信
- 通过环境变量注册 MCP 服务器需设置 `CODE_AGENT_ALLOW_MCP_ENV=true` 门控
- 单服务器工具上限 `MAX_TOOLS_PER_SERVER = 100`，行长度上限 `MAX_LINE_BYTES = 1 MiB`（DoS 防护）

---

### codex — 代码理解

**路径**: `codex/`
**包名**: `code-agent-codex`
**作用**: 代码理解与分析引擎，基于 tree-sitter AST 解析实现深度代码索引、图谱构建和混合检索。

**模块与关键类型**:

| 模块 | 核心类型 | 职责 |
|------|---------|------|
| `indexer` | `CodeIndexer`, `SymbolEntry`, `SymbolKind` | AST 驱动的代码符号索引引擎，将源代码解析为结构化符号条目并持久化到 SQLite |
| `graph` | `CallGraph`, `DependencyGraph`, `CallEdge`, `ImpactResult` | 基于 AST 索引构建的函数调用关系和文件依赖关系图，支持影响分析 |
| `retrieval` | `Retriever`, `SearchOptions`, `ScoredResult`, `RetrievalResult` | 混合检索管道，融合 BM25 词法搜索、向量语义搜索和图谱重排序 |
| `context` | `ContextBuilder`, `ContextChunk` | Agent 引擎的代码上下文检索管道，将任务描述转化为结构化 XML 提示块 |
| `watcher` | `FileWatcher`, `IndexSync`, `FileEvent`, `FileChangeKind` | 增量式文件系统监听器，检测源代码变更并自动更新符号索引和调用图 |

**支持语言**: Python, TypeScript, Rust, Go, Java（通过 tree-sitter 语法解析器）

**依赖**: code-agent-core, tree-sitter + 各语言 parser, rusqlite, walkdir, ignore, notify, blake3, lru

**索引器工作流程**:
1. 使用 tree-sitter 解析源文件为 AST
2. 遍历 AST 提取符号定义（函数、类、结构体、接口等）
3. 计算文件内容 blake3 哈希用于变更检测
4. 将符号条目持久化到 SQLite 数据库
5. 支持增量更新：仅重新索引变更文件

**检索管道**:
- BM25 词法搜索 — 基于 SQLite FTS5 全文索引
- 向量语义搜索 — 通过外部嵌入 API 获取向量表示
- 图谱重排序 — 利用调用图关系提升相关结果排名
- LRU 缓存 — 最近查询结果缓存，减少重复计算

**监听器架构**:
- 基于 notify crate 的跨平台文件系统监听
- 防抖机制避免高频变更触发频繁重建
- 哈希去重确保仅实际变更触发索引更新
- 支持 `.gitignore` 规则自动过滤

---

### eval — 评估框架

**路径**: `eval/`
**包名**: `code-agent-eval`
**作用**: 评估框架，通过 JSON-over-subprocess 委托 Python 评估器执行基准测试，支持 HumanEval、SWE-bench-Live 等标准 benchmark。

**模块与关键类型**:

| 模块 | 核心类型 | 职责 |
|------|---------|------|
| `types` | `EvalTask`, `EvalResult`, `EvalStatus`, `EvalMetrics` | 评估任务与结果类型定义 |
| `sandbox` | `DockerSandbox` | Docker 可用性检查与沙箱环境管理 |
| `runner` | `EvalRunner` | 评估编排器，管理 Python 子进程执行 |
| `adapters` | `BenchmarkAdapter`, `AgentOutput`, `AdapterError` | Benchmark 适配器（HumanEval, SWE-bench-Live） |
| `metrics` | `MetricsCalculator`, `ReportGenerator`, `RegressionDetector` | 指标计算、报告生成、回归检测 |

**Feature flags**:
- `remote-datasets`（默认开启）— 启用远程数据集下载（reqwest）

**依赖**: serde, tokio, async-trait, sha2, tempfile

**评估流程**:
1. `EvalRunner` 接收 `EvalTask`（包含数据集、任务描述、超时配置）
2. 通过 `DockerSandbox` 检查 Docker 可用性，准备隔离环境
3. 委托 Python 子进程执行评估（JSON-over-subprocess）
4. 收集 `EvalResult` 并计算 `EvalMetrics`
5. `MetricsCalculator` 聚合结果，`RegressionDetector` 检测性能回退
6. `ReportGenerator` 输出结构化评估报告

**支持的 Benchmark**:
- HumanEval — 函数级代码生成评估
- SWE-bench-Live — 真实 GitHub Issue 修复评估
- 自定义数据集 — 通过 `BenchmarkAdapter` trait 扩展

---

### cli — CLI 入口

**路径**: `cli/`
**包名**: `code-agent-cli`
**作用**: 提供全功能终端界面和 CI 友好的无头执行模式。

**模块与关键类型**:

| 模块 | 核心类型 | 职责 |
|------|---------|------|
| `tui` | `App`, `ChatPanel`, `StatusBar` | 基于 ratatui + crossterm 的交互式终端 UI |
| `exec` | `ExecMode`, `ExecPipeline` | 无头执行模式，适用于 CI 自动化流水线 |
| `config` | `CliConfig`, `ConfigLoader` | TOML 配置文件加载与合并 |
| `diff` | `DiffRenderer`, `DiffLine` | 基于 similar crate 的差异渲染 |

**依赖**: code-agent-protocol, code-agent-core, code-agent-eval, code-agent-app-server, ratatui, crossterm, clap, toml, similar

**TUI 界面布局**:
- 主聊天面板 — 显示对话历史，支持 Markdown 渲染和语法高亮
- 输入栏 — 底部多行输入，支持快捷键（Ctrl+Enter 发送，Tab 补全）
- 状态栏 — 显示当前会话状态、Token 用量、活跃工具调用
- 侧边栏 — 会话列表、配置面板、工具调用历史

**无头执行模式**:
- 适用于 CI/CD 流水线和自动化脚本
- 通过 `--exec` 标志启用，接收 JSON 格式的指令文件
- 输出结构化 JSON 结果，便于下游工具解析

**配置系统**:
- TOML 格式配置文件，支持多级合并
- 配置项包括：LLM 连接参数、工具权限策略、UI 主题、日志级别
- 支持环境变量覆盖（`CODECTL_*` 前缀）

---

### app-server — JSON-RPC App Server

**路径**: `app-server/`
**包名**: `code-agent-app-server`
**作用**: 基于 stdio 的 JSON-RPC 2.0 应用服务器，为 IDE 插件提供 Agent 编码线程管理接口。

**架构**:
```
IDE Plugin ──stdin──▶ AppServer ──▶ ThreadManager ──▶ Session (ReAct loop)
             ◀─stdout──          ◀── event stream  ◀──
```

**关键类型**:
- `AppServer` — 服务器聚合根，管理 ThreadManager、ModelClient、ToolRegistry
- `JsonRpcRequest` / `JsonRpcResponse` — JSON-RPC 2.0 请求/响应类型
- `handler` — 请求分发处理器，支持 `initialize`、`threads/create`、`threads/submitTurn`、`threads/fork`、`threads/archive`、`threads/list`、`threads/get` 等方法

**支持的 RPC 方法**:
- `initialize` — 握手初始化，返回协议版本、服务器信息、能力声明
- `notifications/initialized` — 客户端初始化完成通知
- `threads/create` — 创建 Agent 线程
- `threads/submitTurn` — 提交轮次输入，通过事件通知流式返回 Agent 执行事件
- `threads/fork` — 从现有线程 fork 出独立副本
- `threads/archive` — 归档线程
- `threads/list` — 列出所有活跃线程
- `threads/get` — 获取线程详情

**依赖**: code-agent-core, code-agent-protocol, tokio, clap, reqwest (rustls-tls)

**支持的 RPC 方法详解**:

| 方法 | 方向 | 说明 |
|------|------|------|
| `initialize` | 请求→响应 | 握手初始化，返回协议版本、服务器信息、能力声明 |
| `notifications/initialized` | 通知 | 客户端通知服务器初始化完成 |
| `threads/create` | 请求→响应 | 创建新 Agent 线程，返回 thread_id 和 session_id |
| `threads/submitTurn` | 请求→响应+通知 | 提交用户输入，Agent 执行期间通过事件通知流式推送 |
| `threads/fork` | 请求→响应 | 从现有线程 fork 出独立副本，继承系统指令和历史 |
| `threads/archive` | 请求→响应 | 归档线程，释放资源 |
| `threads/list` | 请求→响应 | 列出所有活跃线程及其状态 |
| `threads/get` | 请求→响应 | 获取指定线程的详细信息（状态、轮次计数、系统指令） |

**事件通知类型**（通过 `threads/submitTurn` 推送）:
- `turn_started` — 轮次开始，包含 turn_id
- `agent_message_delta` — Agent 消息增量（流式文本）
- `tool_call_begin` — 工具调用开始，包含工具名称和参数
- `tool_call_end` — 工具调用结束，包含执行结果
- `token_usage` — Token 用量统计
- `turn_complete` — 轮次完成，包含最终消息
- `error` — 执行错误

---

### 辅助 Crate

#### eval-runner

**路径**: `eval-runner/`
**包名**: `eval-runner`
**作用**: 评估流水线的 CLI 入口，专为 CI 环境设计。封装 `code-agent-eval`，提供统一的命令行接口来运行 benchmark 和比较结果。

**依赖**: code-agent-eval, clap, anyhow

#### code-agent-web

**路径**: `code-agent-web/`
**包名**: `code-agent-web`
**作用**: 基于 Axum 的 REST + SSE Web API 服务，为 Web 前端提供 Agent 交互接口。支持 CORS、文件服务、SSE 事件流推送。

**依赖**: code-agent-core, code-agent-protocol, axum, tower, tower-http, tokio-stream

**API 端点**:

| 方法 | 路径 | 说明 |
|------|------|------|
| POST | `/threads` | 创建新线程 |
| GET | `/threads` | 列出所有线程 |
| POST | `/threads/:id/turns` | 提交轮次输入，返回 SSE 事件流 |
| GET | `/threads/:id/events` | 获取线程的 SSE 事件流 |
| DELETE | `/threads/:id` | 删除线程 |
| GET | `/health` | 健康检查端点 |
| GET | `/` | 静态文件服务（Web UI） |

**SSE 事件类型**:

| 事件类型 | 说明 |
|----------|------|
| `turn_started` | 轮次开始，包含 turn_id |
| `agent_message_delta` | Agent 消息增量（流式文本） |
| `tool_call_begin` | 工具调用开始，包含工具名称和参数 |
| `tool_call_end` | 工具调用结束，包含执行结果 |
| `token_usage` | Token 用量统计 |
| `turn_complete` | 轮次完成，包含最终消息 |
| `error` | 执行错误 |

```
event: agent_message_delta
data: {"content": "正在分析代码..."}

event: tool_call_begin
data: {"name": "read_file", "arguments": {"path": "src/main.rs"}, "call_id": "call_xxx"}

event: turn_complete
data: {"turn_id": "turn_xxx", "final_message": "分析完成。"}
```

---

## 编译要求

### Rust 版本

- **最低**: Rust 1.80+
- **工具链组件**: rustfmt, clippy, rust-analyzer
- **目标平台**: `x86_64-pc-windows-msvc`（当前配置，可扩展）

工具链配置见 `rust-toolchain.toml`。

### 系统依赖

| 平台 | 依赖 | 说明 |
|------|------|------|
| Windows | — | 无需额外系统依赖（rusqlite 使用 bundled 模式） |
| Linux | libc, libsqlite3-dev（可选） | libc 自动链接；SQLite 默认 bundled |
| macOS | — | 无需额外系统依赖 |

### 编译命令

```bash
# 完整工作区编译
cargo build --workspace

# 发布模式编译（推荐生产使用）
cargo build --workspace --release

# 仅编译特定 crate
cargo build -p code-agent-core
cargo build -p code-agent-cli
cargo build -p code-agent-app-server

# 编译并运行 clippy lint
cargo clippy --workspace -- -D warnings

# 编译并格式化代码
cargo fmt --all --check
```

### 常见编译问题

| 问题 | 原因 | 解决 |
|------|------|------|
| `git2` 编译失败 | 缺少 libgit2 系统依赖 | 安装 cmake / pkg-config，或使用 vendored 特性 |
| `tree-sitter` 编译慢 | 各语言 parser 需要编译 C 源码 | 首次编译较慢，后续增量编译会快很多 |
| `rusqlite` 链接错误 | SQLite 库冲突 | 使用 bundled 特性（默认已启用） |
| Windows 上 `notify` 警告 | Windows 文件监听限制 | 不影响功能，仅影响监听精度 |

---

## 测试运行

```bash
# 运行工作区所有测试
cargo test --workspace

# 运行特定 crate 测试
cargo test -p code-agent-protocol
cargo test -p code-agent-core
cargo test -p code-agent-app-server

# 运行特定测试（按名称过滤）
cargo test -p code-agent-app-server initialize_handshake

# 包含忽略的测试
cargo test --workspace -- --ignored
```

### 测试策略

- **单元测试**: 每个 crate 的模块内内联 `#[cfg(test)] mod tests`，覆盖核心逻辑和边界情况
- **集成测试**: app-server 使用 mock ModelClient 进行完整的 JSON-RPC 请求/响应测试，覆盖 initialize、threads/create、threads/submitTurn、threads/fork、threads/archive 等全部 RPC 方法
- **持久化测试**: core 和 codex 使用 `tempfile` 创建临时 SQLite 数据库，测试后自动清理，确保无状态泄漏
- **HTTP mock**: 使用 `httpmock` 模拟外部 API 调用，测试网络错误处理和重试逻辑
- **并发测试**: 验证多线程环境下的状态一致性（如 ThreadManager 的并发访问）
- **序列化测试**: 所有 protocol 类型经过 JSON 往返测试，确保前后兼容

### 测试覆盖率目标

| 模块 | 目标覆盖率 | 关键测试场景 |
|------|-----------|-------------|
| protocol | 90%+ | 所有类型的序列化/反序列化、边界值、畸形输入 |
| core (agent) | 80%+ | 会话生命周期、轮次流转、错误恢复 |
| core (model) | 80%+ | 流式响应、超时、重试、Token 统计 |
| tools (shell) | 75%+ | 命令执行、超时终止、输出截断 |
| tools (git) | 75%+ | diff 生成、提交、分支操作、错误处理 |
| codex (indexer) | 80%+ | 多语言解析、增量索引、哈希去重 |
| codex (retrieval) | 75%+ | BM25 搜索、混合检索、空结果处理 |
| app-server | 85%+ | 全部 RPC 方法、错误码、并发线程管理 |

---

## DDD 领域划分说明

工作区按领域驱动设计（DDD）组织，每个 crate 对应一个限界上下文：

| 限界上下文 | Crate | 聚合根 | 核心概念 |
|-----------|-------|--------|---------|
| **共享内核** | protocol | — | 值对象（SessionId, ThreadId）、领域事件（Message, ResponseEvent）、枚举（SessionStatus, PermissionMode） |
| **Agent 核心** | core | Session | ReAct 推理循环、模型客户端、工具注册表、安全防护、可观测性、持久化 |
| **工具集成** | tools | ToolRegistry | Shell 沙箱、LSP 客户端、Git 操作、MCP 协议 |
| **代码理解** | codex | CodeIndexer | AST 索引、调用图、混合检索、增量监听 |
| **评估** | eval | EvalRunner | Benchmark 适配器、指标计算、回归检测 |
| **终端交互** | cli | App | TUI 面板、无头执行、配置管理 |
| **IDE 集成** | app-server | AppServer | JSON-RPC 2.0、线程管理、事件流推送 |
| **Web 集成** | code-agent-web | WebServer | REST API、SSE 流、CORS |

### 依赖方向

```
protocol  ←  core  ←  tools
                    ←  codex
                    ←  app-server
                    ←  code-agent-web
                    ←  cli
                    ←  eval  ←  eval-runner
```

`protocol` 位于最底层，所有 crate 依赖它。`core` 是第二层，提供 Agent 引擎抽象。`tools`、`codex`、`eval` 在第三层，各自扩展核心能力。`cli`、`app-server`、`code-agent-web`、`eval-runner` 在最上层，作为入口点组合下层能力。

### 架构原则

1. **面向接口编程** — Agent 核心只依赖 trait，不依赖具体实现。`ModelClient`、`Tool`、`SafetyClient` 等核心接口均可替换
2. **聚合根驱动** — `Session` 是核心聚合根，管理完整的会话生命周期，确保状态一致性
3. **流式优先** — 所有模型交互采用流式（SSE）协议，支持实时事件推送和增量渲染
4. **安全内建** — 安全审查嵌入工具执行和用户输入管道，权限控制贯穿全链路
5. **可观测性默认** — 结构化日志和分布式追踪默认开启，生产环境可切换 OTLP 导出
6. **失败隔离** — 单个工具执行失败不影响 Agent 整体状态，支持重试和降级

### 领域事件流

Agent 的一次完整轮次（Turn）产生以下领域事件序列：

```
TurnStarted
  → AgentMessageDelta (0..N)
  → ToolCallBegin → ToolCallEnd (0..N, 可循环)
  → AgentMessageDelta (0..N)
  → TokenUsage
  → TurnComplete | Error
```

这些事件通过 `ResponseEvent` 枚举在系统各层之间传递：
- **core** 层产生事件流（来自 ModelClient 的 SSE 流）
- **app-server** 层将事件转换为 JSON-RPC 通知推送给 IDE
- **code-agent-web** 层将事件转换为 SSE 事件推送给 Web 客户端
- **cli** 层消费事件并实时更新 TUI 界面

### 安全架构

安全机制贯穿系统全链路：

| 层级 | 安全措施 | 实现位置 | CWE |
|------|---------|---------|-----|
| 输入层 | 提示注入检测、敏感信息过滤 | core::safety | — |
| 输入层 | 工作区边界强制、路径穿越防御 | core::tools | CWE-22 |
| 工具层 | 命令沙箱执行、路径白名单、Git 操作范围限制 | tools::shell, tools::git | — |
| 工具层 | Shell 元字符检测、命令注入防护 | tools::shell | CWE-78 |
| 工具层 | 沙箱门控（需显式 opt-in 才能无沙箱执行） | tools::shell | CWE-250 |
| 工具层 | Git 身份强制验证（拒绝硬编码身份） | tools::git | CWE-290 |
| 权限层 | PermissionMode（Auto/Permit/Block）、CapabilityLevel（Read/Edit/Exec） | protocol::config | — |
| 输出层 | 内容安全审查、正则预过滤 + 守卫模型 | core::safety | CWE-184 |
| 输出层 | CORS 来源限制 + API 认证 | code-agent-web | CWE-942 |
| 审计层 | 全量操作日志、Token 用量追踪 | core::observability | — |
