# 定制码匠（CraftCoder）

> 基于 LLM 开源模型的 AI Coding Agent，支持 CLI / IDE / Web 三种交互形态，可私有化部署。

定制码匠（CraftCoder）是一个面向开发者的 AI 编程助手平台。它通过大语言模型驱动，能够理解自然语言指令，在本地环境中执行代码生成、文件操作、Git 集成、终端命令、代码搜索与重构等任务。项目采用 Rust 作为主力语言，核心模块覆盖 LLM 模型接入、工具调用、代码理解（Codex）、评估基准、终端交互界面和 Web API 服务，同时提供 Python 评估套件和 VS Code 扩展，形成完整的 AI 编码助手解决方案。

## 项目架构

```
┌─────────────────────────────────────────────────────────┐
│                    交互层 (Interfaces)                    │
│  ┌──────────┐  ┌──────────────┐  ┌───────────────────┐  │
│  │  CLI TUI  │  │  Web API     │  │  VS Code 扩展     │  │
│  │ (ratatui) │  │ (Axum+REST)  │  │ (TypeScript)     │  │
│  └─────┬─────┘  └──────┬───────┘  └────────┬──────────┘  │
│        │               │                    │             │
├────────┼───────────────┼────────────────────┼─────────────┤
│        ▼               ▼                    ▼             │
│  ┌──────────────────────────────────────────────────┐    │
│  │              核心引擎 (Core Engine)               │    │
│  │  ┌──────────┐  ┌──────────┐  ┌───────────────┐  │    │
│  │  │  Protocol │  │  Core    │  │  Tools 工具集  │  │    │
│  │  │  数据模型  │  │  LLM接入  │  │ 文件/Git/Shell│  │    │
│  │  └──────────┘  └──────────┘  └───────────────┘  │    │
│  │  ┌──────────────────────────────────────────┐    │    │
│  │  │  Codex 代码理解引擎                        │    │    │
│  │  │  (Tree-sitter AST / 索引 / 搜索 / 重构)    │    │    │
│  │  └──────────────────────────────────────────┘    │    │
│  └──────────────────────────────────────────────────┘    │
│                          │                               │
├──────────────────────────┼───────────────────────────────┤
│                          ▼                               │
│  ┌──────────────────────────────────────────────────┐    │
│  │              评估与质量 (Evaluation)               │    │
│  │  ┌──────────────┐  ┌──────────────┐  ┌────────┐  │    │
│  │  │  Rust Eval   │  │  Python Eval │  │  Eval  │  │    │
│  │  │  基准框架     │  │ SWE-bench等  │  │ Bridge │  │    │
│  │  └──────────────┘  └──────────────┘  └────────┘  │    │
│  └──────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────┘
```

项目分为三个层次：

**交互层**提供三种使用形态。CLI TUI 基于 ratatui 构建，适合终端重度用户；Web API 基于 Axum 提供 REST + SSE 接口，可嵌入 Web 应用；VS Code 扩展通过 JSON-RPC 2.0 协议连接应用服务器，在编辑器内提供内联 AI 辅助。

**核心引擎**是项目的灵魂。Protocol 定义所有跨模块的数据契约；Core 封装 LLM 模型客户端，支持多模型切换和可观测性；Tools 提供文件、Git、Shell、LSP、MCP 等工具，每个工具实现标准化的 Tool trait；Codex 基于 Tree-sitter 实现多语言代码理解，支持 AST 解析、索引搜索和增量监视。

**评估与质量**层保障项目可靠性。Rust Eval 提供基准框架抽象，Python Eval 对接 SWE-bench 等业界标准，Eval Bridge 通过 PyO3 实现跨语言调用。

### 9 个 Rust Crate（Cargo Workspace）

所有 Rust 代码位于 `code-agent-rs/` 目录，通过 Cargo Workspace 管理：

| Crate | 路径 | 说明 |
|-------|------|------|
| **code-agent-protocol** | `protocol/` | 共享协议类型与核心数据模型。定义 Agent 消息、工具调用、配置等所有跨 crate 类型。零依赖核心，被所有其他 crate 引用。 |
| **code-agent-core** | `core/` | LLM 模型客户端抽象与 Provider 实现。支持 OpenAI 兼容 API、可观测性（tracing / OpenTelemetry）、对话管理、SQLite 持久化。 |
| **code-agent-tools** | `tools/` | Agent 工具集：文件读写、Git 操作（git2）、Shell 命令执行、LSP 集成、MCP 协议支持。每个工具实现标准化的 Tool trait。 |
| **code-agent-codex** | `codex/` | 代码理解引擎。基于 Tree-sitter 的多语言 AST 解析（Python/TypeScript/Rust/Go/Java），代码索引与搜索（SQLite + blake3 指纹）、文件监视、LRU 缓存。 |
| **code-agent-eval** | `eval/` | 评估基准框架。定义 Dataset / Task / Runner 抽象，支持 SWE-bench 等基准的本地运行与结果对比。 |
| **code-agent-cli** | `cli/` | 交互式终端 UI。基于 ratatui + crossterm 构建，提供 TUI 聊天界面、配置管理、评估运行入口。 |
| **code-agent-app-server** | `app-server/` | JSON-RPC 2.0 应用服务器。为 IDE 集成提供标准协议接口，VS Code 扩展通过此服务与 Agent 通信。 |
| **code-agent-web** | `code-agent-web/` | Web API 服务。基于 Axum 框架，提供 REST + SSE 接口，支持浏览器端交互。 |
| **eval-runner** | `eval-runner/` | 评估运行器 CLI。CI 流水线中执行基准测试并输出对比结果。 |

## 功能特性

### 多模型 LLM 接入

支持多种 LLM Provider，包括 OpenAI 兼容 API（Qwen3）、Anthropic Claude 原生协议、Google Gemini 原生协议。通过统一的 Provider 抽象层，切换模型无需修改代码。内置请求重试、流式响应、Token 用量追踪和超时控制。

### 代码理解与搜索

Codex 引擎基于 Tree-sitter 对 Python、TypeScript、Rust、Go、Java 五种语言做精确的 AST 解析。支持语义级代码搜索（查找函数定义、引用、类继承关系）、增量索引更新和文件变更监视。所有索引数据存储在 SQLite 中，通过 blake3 指纹实现去重。

### 工具调用系统

Agent 可以调用一系列工具来完成复杂任务：

- **文件操作**：读写、编辑、创建、删除文件和目录
- **Git 集成**：提交、分支、diff、log 等常用 Git 操作
- **Shell 执行**：在受控工作区中运行终端命令
- **LSP 集成**：通过语言服务器获取诊断、跳转定义、查找引用
- **MCP 协议**：支持 Model Context Protocol，可扩展第三方工具

工具执行时进行路径验证和 workspace 范围限定，防止越权访问。

### 三种交互形态

| 形态 | 入口 | 适用场景 |
|------|------|----------|
| CLI TUI | `code-agent-cli tui` | 终端重度用户、SSH 远程开发 |
| Web API | `code-agent-web` | Web 应用集成、浏览器访问 |
| VS Code 扩展 | `code-agent-vscode/` | 编辑器内联 AI 辅助 |

### 评估与基准

内置完整的评估框架，支持 SWE-bench、HumanEval 等业界标准基准。Rust 和 Python 双实现，通过 PyO3 桥接实现跨语言调用。CI 流水线自动运行评估，追踪每次提交的性能变化。

### 可观测性

基于 tracing 的结构化日志系统，支持 JSON 格式输出和环境过滤。OpenTelemetry 集成作为可选功能，通过 `otlp` cargo feature 启用，可将链路追踪数据导出到 Jaeger、Grafana 等后端，方便生产环境监控。

### 私有化部署

所有组件均可私有化部署。LLM 模型可选用本地推理服务（vLLM、Ollama、llama.cpp），数据存储在本地 SQLite 中，无需依赖任何外部云服务。

### Python 评估套件

`code-agent-eval-py/` — 基于 Python 的评估实现，支持 SWE-bench-Live、HumanEval 等业界标准基准。通过 Docker 隔离运行环境，确保评估结果的可复现性。

### eval-bridge（PyO3）

`eval-bridge/` — Rust 与 Python 评估套件之间的桥梁。使用 PyO3 实现 Rust 直接调用 Python 评估逻辑，避免进程间通信开销。独立于主 workspace 编译（PyO3 需要 Python 开发头文件）。

### VS Code 扩展

`code-agent-vscode/` — TypeScript 编写的 VS Code 扩展。通过 JSON-RPC 2.0 协议连接 `code-agent-app-server`，在编辑器内提供 AI 编码辅助功能。

## 核心设计原则

### 分层解耦

项目采用严格的分层架构。Protocol 层定义数据契约，不依赖任何运行时逻辑。Core 层只依赖 Protocol，提供 LLM 接入抽象，并在 `core/src/tools/` 中定义标准化的 `Tool` trait。Tools 层依赖 Core 和 Protocol，实现具体工具。交互层（CLI / Web / IDE）在最上层，组合核心能力暴露给用户。每一层只依赖下层，不产生循环依赖。

### Provider 模式

LLM 模型接入采用 Provider 模式。`ModelClient` trait 定义了统一的 `complete_stream` / `complete` 接口，不同模型服务实现各自的 Provider。当前已支持 OpenAI 兼容 API（Qwen3）、Anthropic Claude 原生协议、Google Gemini 原生协议三种 Provider。

### Tool trait 标准化

所有 Agent 工具实现统一的 `Tool` trait：

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> serde_json::Value;
    fn capability(&self) -> CapabilityLevel;
    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError>;
}
```

这种设计让 Agent 可以动态发现可用工具，也方便第三方开发者贡献新工具。

### 安全优先

工具执行层内置基本安全机制：路径穿越检测防止越权访问文件；Shell 命令执行在受控工作区中运行；Git 操作限制在 workspace 范围内；工具调用记录操作日志。生产环境中可通过配置禁用高风险工具。

### 可观测性内置

从项目第一天起就内置了结构化日志。每个 LLM 请求、工具调用、Agent 决策都记录结构化事件。开发阶段通过 tracing-subscriber 输出彩色日志，生产环境可切换 JSON 格式。OpenTelemetry 链路追踪为可选功能，需启用 `otlp` feature 并配置 Collector 端点。

## 目录结构

```
y-ai-coding/
├── .ai/                        # AI-OS 开发状态机状态文件
├── .github/                    # GitHub CI/CD 配置
│   └── workflows/
│       ├── ci.yml              # 持续集成（编译 + 测试 + lint）
│       ├── audit.yml           # 安全审计（cargo audit / trivy）
│       ├── eval.yml            # 评估流水线（自动运行基准）
│       └── release.yml         # 发布流水线
├── .omo/                       # OpenCode 工作记录
│   ├── evidence/               # 开发证据
│   ├── notepads/               # 开发笔记
│   ├── plans/                  # 开发计划
│   ├── research/               # 调研记录
│   └── run-continuation/       # 运行延续状态
│
├── code-agent-rs/              # Rust 主项目（Cargo Workspace）
│   ├── Cargo.toml              # Workspace 定义
│   ├── rust-toolchain.toml     # Rust 工具链版本锁定
│   ├── clippy.toml             # Clippy lint 配置
│   ├── core/                   # code-agent-core crate
│   ├── tools/                  # code-agent-tools crate
│   ├── codex/                  # code-agent-codex crate
│   ├── eval/                   # code-agent-eval crate
│   ├── cli/                    # code-agent-cli crate（TUI 入口）
│   ├── app-server/             # code-agent-app-server crate
│   ├── protocol/               # code-agent-protocol crate
│   ├── eval-runner/            # eval-runner crate
│   └── code-agent-web/         # code-agent-web crate（Web API）
│
├── code-agent-eval-py/         # Python 评估套件
│   ├── README.md               # 使用说明
│   ├── pyproject.toml          # Python 项目配置
│   ├── code_agent_eval/        # 评估实现
│   ├── tests/                  # 测试
│   └── Dockerfile              # 评估运行容器
│
├── code-agent-vscode/          # VS Code 扩展
│   ├── package.json            # 扩展清单
│   ├── src/                    # TypeScript 源码
│   ├── out/                    # 编译输出
│   └── tsconfig.json           # TypeScript 配置
│
├── eval-bridge/                # Rust ↔ Python 评估桥接（PyO3）
│   ├── README.md               # 使用说明
│   ├── Cargo.toml
│   ├── build.rs
│   ├── src/
│   └── tests/
│
├── doc/                        # 项目文档
│   ├── 产品设计文档.md          # 产品需求与设计
│   ├── 技术设计文档.md          # 架构与技术方案
│   ├── 部署手册.md              # 部署与运维
│   └── 使用手册.md              # 用户使用指南
│
└── README.md                   # 本文件
```

## 快速开始

### 系统要求

- Rust 1.80+
- Python 3.10+（仅评估套件需要）
- Node.js 18+（仅 VS Code 扩展需要）
- 一个兼容 OpenAI API 的 LLM 服务（本地或远程）

### 编译

```bash
# 克隆项目后，进入 Rust 工作区
cd code-agent-rs

# 编译所有 crate
cargo build --release

# 仅编译 CLI
cargo build --release -p code-agent-cli

# 运行测试
cargo test --workspace
```

### 配置环境变量

定制码匠通过环境变量配置 LLM 连接：

```bash
# 必需：LLM API 地址（兼容 OpenAI 格式）
export LLM_API_BASE="https://api.openai.com/v1"

# 必需：API 密钥
export LLM_API_KEY="sk-your-api-key-here"

# 可选：模型名称（默认 gpt-4o）
export LLM_MODEL="gpt-4o"

# 可选：请求超时（秒，默认 120）
export LLM_TIMEOUT=120

# 可选：最大 token 数（默认 4096）
export LLM_MAX_TOKENS=4096
```

支持以下 LLM 服务：

- OpenAI / Azure OpenAI（兼容 API）
- **Anthropic**（Claude，原生 Messages API）
- **Google Gemini**（原生 generateContent API）
- Qwen3（兼容 OpenAI API / 原生）
- 本地部署的 vLLM / Ollama / llama.cpp
- 各类开源模型推理服务

### 运行 TUI

```bash
# 启动交互式终端界面
cargo run --release -p code-agent-cli -- tui
```

CLI 支持以下子命令：

```bash
# 查看帮助
cargo run --release -p code-agent-cli -- --help

# 单轮对话（非交互模式）
cargo run --release -p code-agent-cli -- chat "用 Rust 写一个斐波那契数列生成器"

# 启动 Web API 服务
cargo run --release -p code-agent-web

# 启动 IDE 应用服务器
cargo run --release -p code-agent-app-server
```

### 配置文件

除了环境变量，CLI 也支持通过配置文件设置参数。配置文件路径默认为 `~/.config/code-agent/config.toml`：

```toml
# 当前配置格式（扁平 TOML）
model = "qwen3.6-27b"
api_key = "sk-..."
api_base_url = "https://api.openai.com/v1"
timeout_secs = 300
max_iterations = 50
permission_mode = "auto"
work_dir = "."
system_instructions = "你是一个专业的编程助手。"
tool_allowlist = ["read_file", "bash"]

# === v2 可选扩展配置节 ===
# [planner]
# max_parallel = 6
# max_depth = 2
# 
# [knowledge]
# rules_dir = ".craftcoder/rules/"
# 
# [quality]
# gate_threshold = 80.0
```

### Docker 部署

Rust 核心提供 Dockerfile，支持编译为轻量生产镜像。Python 评估套件也提供独立的 Docker 环境：

```bash
# 构建并运行 Rust 核心容器
cd code-agent-rs
docker build -t craftcoder .
docker run --rm craftcoder --help

# 构建并运行 Python 评估容器
cd code-agent-eval-py
docker build -t craftcoder-eval .
docker run --rm craftcoder-eval --benchmark humaneval
```

### 运行评估

```bash
# 使用 eval-runner 运行基准测试
cargo run --release -p eval-runner -- --dataset swe-bench --task "..."

# 使用 Python 评估套件
cd code-agent-eval-py
pip install -e .
python -m code_agent_eval.run --benchmark humaneval
```

## 文档

项目文档位于 `doc/` 目录，包含 4 份文档：

| 文档 | 说明 |
|------|------|
| [产品设计文档.md](doc/%E4%BA%A7%E5%93%81%E8%AE%BE%E8%AE%A1%E6%96%87%E6%A1%A3.md) | 产品定位、用户场景、功能需求、交互设计 |
| [技术设计文档.md](doc/%E6%8A%80%E6%9C%AF%E8%AE%BE%E8%AE%A1%E6%96%87%E6%A1%A3.md) | 系统架构、模块设计、数据流、关键技术决策 |
| [部署手册.md](doc/%E9%83%A8%E7%BD%B2%E6%89%8B%E5%86%8C.md) | 环境要求、部署步骤、配置说明、运维指南 |
| [使用手册.md](doc/%E4%BD%BF%E7%94%A8%E6%89%8B%E5%86%8C.md) | 安装指南、功能操作、常见问题、最佳实践 |

这 4 份文档覆盖了项目的完整生命周期。产品设计文档从用户视角定义需求，技术设计文档深入系统内部，部署手册指导运维，使用手册帮助最终用户上手。建议新加入项目的开发者按此顺序阅读。

## 技术栈

| 层面 | 技术选型 |
|------|----------|
| 主力语言 | Rust 2021 Edition |
| LLM 接入 | 自定义 Provider 抽象层，支持 OpenAI 兼容 API |
| 代码理解 | Tree-sitter AST 解析（Python/TypeScript/Rust/Go/Java） |
| 终端 UI | ratatui + crossterm |
| Web 框架 | Axum + Tower + SSE |
| IDE 协议 | JSON-RPC 2.0 |
| 持久化 | SQLite（rusqlite） |
| Git 集成 | git2（libgit2 绑定） |
| 文件监视 | notify 7 |
| 缓存 | lru 0.12 |
| 内容哈希 | blake3 1 |
| CLI 框架 | clap 4 |
| Web 中间件 | tower 0.4 + tower-http 0.5 |
| Diff 显示 | similar 2 |
| 配置解析 | toml 0.8 |
| SSE 流式传输 | tokio-stream 0.1 |
| 可观测性 | tracing + OpenTelemetry（可选，需启用 `otlp` feature） |
| 评估桥接 | PyO3（Rust ↔ Python） |
| 扩展语言 | TypeScript（VS Code 扩展） |
| CI/CD | GitHub Actions |

## 贡献指南

欢迎贡献代码、报告问题或提出改进建议。

### 报告问题

在 GitHub Issues 中提交问题时，请包含：

- 运行环境（操作系统、Rust 版本、LLM 服务类型）
- 复现步骤
- 期望行为与实际行为
- 相关日志输出

### 提交代码

1. Fork 本仓库并创建特性分支
2. 确保所有测试通过：`cargo test --workspace`
3. 确保 lint 检查通过：`cargo clippy --workspace -- -D warnings`
4. 确保代码格式化：`cargo fmt --all`
5. 提交 Pull Request，描述变更内容和动机

### 开发约定

- 所有新增公共类型必须添加文档注释
- 工具实现必须包含单元测试
- 重大变更需在 PR 描述中说明
- 遵循 Rust 2021 Edition 惯用写法

## 项目路线图

### 基础框架
- [x] Rust Cargo Workspace 搭建
- [x] Protocol 数据模型定义
- [x] Core LLM Provider 抽象层
- [x] Tools 工具集（文件 / Git / Shell）
- [x] CLI TUI 交互界面
- [x] Codex 代码理解引擎（Tree-sitter 多语言支持）
- [x] Eval 评估框架
- [x] CI/CD 流水线

### 交互完善
- [x] Web API 服务（Axum REST + SSE）
- [x] VS Code 扩展（JSON-RPC 2.0）
- [x] MCP 协议支持
- [x] LSP 集成
- [x] 配置管理（文件 + 环境变量）

### 生产就绪（当前阶段）
- [x] OpenTelemetry 可观测性（可选功能，启用 `otlp` feature）
- [ ] Docker 容器化部署
- [ ] 多用户会话管理
- [ ] 权限与安全审计
- [ ] 性能优化与缓存

### 生态扩展
- [ ] 插件系统
- [ ] 自定义工具 SDK
- [x] 更多 LLM Provider（Anthropic / Google Gemini）— 已实现
- [ ] Web UI 管理面板
- [ ] 团队协作功能

### AI 原生开发工作台（v2 架构）
- [ ] Phase A: Delegator/Coder 双角色架构（基于现有 SubAgentManager）
- [ ] Phase B: Plan 引擎（三级分解 + DAG 依赖图）
- [ ] Phase C: 上下文增强（LLM 摘要 + 滑动窗口 + Token 预算）
- [ ] Phase D: Spec 驱动流水线 + 质量门禁
- [ ] Phase E: 知识体系（Rules/Skills/企业知识库）
- [ ] Phase F: 工具生态（插件/Hooks/Commands）
- [ ] Phase G: 数据飞轮闭环

## 开发指南

### 本地开发环境

```bash
# 克隆仓库
git clone <repo-url>
cd y-ai-coding

# 进入 Rust 工作区
cd code-agent-rs

# 安装开发依赖（可选）
cargo install cargo-watch  # 文件变更自动重编译
cargo install cargo-llvm-cov  # 代码覆盖率

# 运行所有测试
cargo test --workspace

# 运行 lint 检查
cargo clippy --workspace -- -D warnings

# 格式化代码
cargo fmt --all
```

### 工作流

1. 在 `code-agent-rs/` 下开发 Rust 代码
2. 修改 `protocol/` 中的类型定义后，运行 `cargo test` 确保跨 crate 兼容
3. 新增工具在 `tools/` 中实现 `Tool` trait
4. 新增 LLM Provider 在 `core/` 中实现 `ModelClient` trait
5. 提交前运行 `cargo clippy` 和 `cargo fmt`
6. CI 流水线会自动执行编译、测试、lint 和评估

### 评估开发

```bash
# 运行 Rust 评估
cargo test -p code-agent-eval

# 运行 Python 评估
cd code-agent-eval-py
pip install -e ".[dev]"
pytest tests/

# 构建 eval-bridge（需要 Python 开发头文件）
cd eval-bridge
cargo build
```

### VS Code 扩展开发

```bash
cd code-agent-vscode
npm install
npm run compile
# 在 VS Code 中按 F5 启动扩展开发模式
```

## CI/CD 流水线

项目使用 GitHub Actions 进行持续集成和交付，包含四条流水线：

| 流水线 | 触发条件 | 执行内容 |
|--------|----------|----------|
| **CI** | 每次 push / PR | 编译所有 crate、运行测试、Clippy lint、格式化检查 |
| **Audit** | 定时 + 依赖更新 | cargo audit 安全审计、Trivy 容器扫描 |
| **Eval** | main 分支 push | 自动运行评估基准，对比性能变化 |
| **Release** | 标签推送 | 构建发布产物，生成 GitHub Release |

## 许可证

本项目基于 MIT 许可证开源。详见 [LICENSE](LICENSE) 文件。

---

*定制码匠（CraftCoder） — 你的专属 AI 编码匠人。*
