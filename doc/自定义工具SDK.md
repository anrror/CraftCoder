# 自定义工具 SDK 开发指南

> 面向第三方开发者的 CraftCoder 工具扩展开发文档。
> 本文档涵盖 Tool trait 实现、注册、钩子、插件、测试与集成全流程。

---

## 目录

- [1. 概述](#1-概述)
- [2. 快速开始](#2-快速开始)
- [3. Tool trait 参考](#3-tool-trait-参考)
- [4. 关键类型](#4-关键类型)
- [5. 辅助函数](#5-辅助函数)
- [6. 注册工具](#6-注册工具)
- [7. 执行管线](#7-执行管线)
- [8. 钩子系统](#8-钩子系统)
- [9. 插件系统](#9-插件系统)
- [10. 完整示例](#10-完整示例)
- [11. 测试指南](#11-测试指南)
- [12. 集成指南](#12-集成指南)

---

## 1. 概述

CraftCoder 的自定义工具 SDK（`code-agent-core`）提供了一套标准化的接口，让第三方开发者可以为 AI Agent 编写自定义工具。Agent 可以通过工具访问文件系统、网络、外部 API、数据库等外部资源。

### 核心设计原则

- **Trait 驱动**：所有工具实现统一的 `Tool` trait，Agent 通过 trait 接口动态发现和调用工具
- **安全内建**：权限检查、路径穿越防护、工作区边界强制执行贯穿全链路
- **可组合**：支持钩子（Hook）和插件（Plugin）两种扩展模式
- **异步优先**：所有工具执行方法都是 `async`，支持 I/O 密集型操作

### 依赖关系

```
code-agent-protocol  ←  code-agent-core  ←  your-custom-tool
```

你的自定义工具 crate 只需要依赖 `code-agent-core`，它会自动引入 `code-agent-protocol` 中的所有基础类型。

---

## 2. 快速开始

三步完成一个自定义工具：

### 步骤 1：实现 Tool trait

```rust
use async_trait::async_trait;
use code_agent_core::tools::{
    Tool, ToolError,
    tool_success, require_string,
};
use code_agent_protocol::{CapabilityLevel, ToolResultMessage};

struct GreetTool;

#[async_trait]
impl Tool for GreetTool {
    fn name(&self) -> &str {
        "greet"
    }

    fn description(&self) -> &str {
        "Greet a person by name."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The name of the person to greet"
                }
            },
            "required": ["name"]
        })
    }

    fn capability(&self) -> CapabilityLevel {
        CapabilityLevel::Read
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let name = require_string(&params, "name")?;
        let greeting = format!("你好，{name}！欢迎使用 CraftCoder。");
        Ok(tool_success("", greeting))
    }
}
```

### 步骤 2：注册到 ToolRegistry

```rust
use std::sync::Arc;
use code_agent_core::tools::registry::{DefaultToolRegistry, ToolRegistry};

let mut registry = DefaultToolRegistry::new();
registry.register(Arc::new(GreetTool))?;
```

### 步骤 3：通过配置文件加载

在 `~/.config/code-agent/config.toml` 中引入你的工具插件：

```toml
# 通过插件注册自定义工具
[plugins.my_tools]
type = "custom"
path = "path/to/your_tool_crate"
```

或通过 `PluginManager` 程序化加载（详见 [插件系统](#9-插件系统)）。

---

## 3. Tool trait 参考

### 完整签名

```rust
use async_trait::async_trait;
use code_agent_protocol::{CapabilityLevel, ToolResultMessage};
use code_agent_core::tools::{Tool, ToolError};

#[async_trait]
pub trait Tool: Send + Sync {
    /// 返回工具的唯一名称（例如 "read_file", "web_search"）
    fn name(&self) -> &str;

    /// 返回工具功能的可读描述（会发送给模型上下文）
    fn description(&self) -> &str;

    /// 返回工具输入参数的 JSON Schema
    fn input_schema(&self) -> serde_json::Value;

    /// 返回使用此工具所需的能力级别
    fn capability(&self) -> CapabilityLevel;

    /// 使用给定的参数执行工具
    async fn execute(&self, params: serde_json::Value)
        -> Result<ToolResultMessage, ToolError>;
}
```

### 各方法说明

| 方法 | 返回值 | 说明 |
|------|--------|------|
| `name()` | `&str` | 工具的唯一标识符。全小写，使用下划线分隔（如 `"web_search"`）。此名称在注册表中必须唯一。 |
| `description()` | `&str` | 工具的自然语言描述，会出现在模型的系统提示中。应清晰描述工具的用途和使用场景。 |
| `input_schema()` | `serde_json::Value` | JSON Schema 格式的参数定义。模型据此生成合法的工具调用参数。必须包含 `"type": "object"` 和 `"properties"`。 |
| `capability()` | `CapabilityLevel` | 工具所需的能力级别。`Read`（只读）、`Edit`（读写）、`Exec`（完全）。权限检查时会与此值比较。 |
| `execute()` | `Result<ToolResultMessage, ToolError>` | 核心执行方法。接收 JSON 参数，返回执行结果。是异步方法，支持 I/O 操作。 |

### 实现规范

1. **name()** 必须返回唯一名称，全小写 + 下划线分隔
2. **input_schema()** 必须返回合法的 JSON Schema 对象，包含 `type`、`properties`、`required`
3. **capability()** 应根据工具的实际影响选择最低权限级别：
   - 只读操作（读取文件、HTTP GET、搜索）→ `CapabilityLevel::Read`
   - 修改操作（写文件、HTTP POST/DELETE、Git 操作）→ `CapabilityLevel::Edit`
   - 执行代码/命令（Shell 执行、脚本运行）→ `CapabilityLevel::Exec`
4. **execute()** 应使用辅助函数提取参数（如 `require_string`），返回结构化的 `ToolResultMessage`

---

## 4. 关键类型

### 4.1 ToolResultMessage

工具执行的结果载体。成功时 `output` 有值，失败时 `error` 有值。

```rust
// 定义于 code-agent-protocol
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResultMessage {
    /// 对应的工具调用 ID，用于关联请求与响应
    pub tool_call_id: String,

    /// 工具执行成功时的输出文本
    pub output: Option<String>,

    /// 工具执行失败时的错误描述
    pub error: Option<String>,
}

impl ToolResultMessage {
    /// 判断工具是否执行成功（error 为 None）
    pub fn is_success(&self) -> bool;

    /// 判断工具是否执行失败（error 有值）
    pub fn is_error(&self) -> bool;
}
```

**使用示例**：
```rust
let result = tool_success("call-001", "操作完成");
assert!(result.is_success());

let result = tool_error("call-002", "网络连接超时");
assert!(result.is_error());
```

### 4.2 CapabilityLevel

工具所需的操作能力级别。是一个全序枚举（`Read < Edit < Exec`），可直接用 `>` `<` 比较。

```rust
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum CapabilityLevel {
    /// 只读 — 查看但不能修改（读取文件、搜索代码、HTTP GET）
    Read,
    /// 读写 — 在 Read 基础上增加修改（写文件、Git 操作）
    Edit,
    /// 完全 — 在 Edit 基础上增加命令执行（Shell、脚本）
    Exec,
}
```

**选择指南**：
| 工具类型 | CapabilityLevel | 示例 |
|----------|-----------------|------|
| 文件读取、HTTP GET、搜索 | `Read` | `read_file`, `grep`, `web_search`, `web_fetch` |
| 文件写入、HTTP POST/PUT/DELETE、Git | `Edit` | `write_file`, `git_commit`, `api_create` |
| Shell 命令、脚本执行 | `Exec` | `bash`, `python_run` |

### 4.3 ToolError

工具操作期间可能发生的所有错误类型。

```rust
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// 请求的工具名称未在注册表中找到
    #[error("tool not found: {0}")]
    NotFound(String),

    /// 此名称的工具已被注册
    #[error("tool already registered: {0}")]
    AlreadyRegistered(String),

    /// 调用者缺乏足够的权限
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// 参数无效或缺少必需字段
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// 工具执行期间遇到错误
    #[error("execution error: {0}")]
    ExecutionError(String),

    /// I/O 错误（实现 From<std::io::Error> 可自动转换）
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// 解析后的路径在工作区外部（CWE-22 路径穿越防护）
    #[error("path outside workspace: {path} (workspace root: {workspace})")]
    OutsideWorkspace {
        path: std::path::PathBuf,
        workspace: std::path::PathBuf,
    },
}
```

**构造器方法**：
```rust
// 便捷构造器
ToolError::not_found("my_tool")
ToolError::already_registered("my_tool")
ToolError::permission_denied("需要 Edit 权限")
ToolError::invalid_input("缺少必需参数 'url'")
ToolError::execution_error("API 请求超时")
```

**在 execute() 中使用**：
```rust
// 参数校验失败
return Err(ToolError::invalid_input("url 参数不能为空"));

// 业务逻辑错误
return Err(ToolError::execution_error(
    format!("HTTP 请求失败: 状态码 {}", status)
));

// I/O 错误（自动转换）
let content = std::fs::read_to_string(path)?; // 自动转换为 ToolError::Io
```

### 4.4 ToolDefinition

工具的静态元数据，发送给模型上下文。

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDefinition {
    /// 工具的唯一名称
    pub name: String,

    /// 工具功能的可读描述
    pub description: String,

    /// 描述工具输入参数的 JSON Schema
    pub input_schema: serde_json::Value,
}
```

`ToolDefinition` 由 `ToolRegistry::list()` 方法从已注册工具自动生成。你不需要手动构造它。

---

## 5. 辅助函数

所有辅助函数位于 `code_agent_core::tools` 模块中。

### 5.1 结果构建函数

```rust
/// 为成功执行构建 ToolResultMessage
///
/// 参数:
///   tool_call_id: 工具调用 ID（用于关联请求与响应）
///   output: 执行结果文本
pub fn tool_success(tool_call_id: &str, output: impl Into<String>) -> ToolResultMessage;

/// 为失败执行构建 ToolResultMessage
///
/// 参数:
///   tool_call_id: 工具调用 ID
///   error: 错误描述文本
pub fn tool_error(tool_call_id: &str, error: impl Into<String>) -> ToolResultMessage;
```

### 5.2 参数提取函数

```rust
/// 从 JSON 参数中提取必需的字符串参数
///
/// 如果键缺失或值不是字符串，返回 ToolError::InvalidInput
///
/// 参数:
///   params: 工具的 JSON 输入参数
///   key: 要提取的键名
///
/// 返回: Ok(String) 或 Err(ToolError::InvalidInput)
pub fn require_string(params: &serde_json::Value, key: &str) -> Result<String, ToolError>;

/// 从 JSON 参数中提取可选的字符串参数
///
/// 参数:
///   params: 工具的 JSON 输入参数
///   key: 要提取的键名
///
/// 返回: Some(String) 或 None
pub fn optional_string(params: &serde_json::Value, key: &str) -> Option<String>;

/// 从 JSON 参数中提取可选的 u64 整数参数
///
/// 参数:
///   params: 工具的 JSON 输入参数
///   key: 要提取的键名
///
/// 返回: Some(u64) 或 None
pub fn optional_u64(params: &serde_json::Value, key: &str) -> Option<u64>;
```

### 5.3 路径安全函数

```rust
/// 解析并验证文件路径（读取操作）
///
/// 将路径规范化为绝对路径，解析所有 `..`、`.` 和符号链接。
/// 验证解析后的路径必须在工作区根目录内（CWE-22 防护）。
///
/// 如果路径不存在，返回 ExecutionError("path not found: ...")
/// 如果路径在工作区外，返回 OutsideWorkspace
pub fn resolve_safe_path(path_str: &str) -> Result<std::path::PathBuf, ToolError>;

/// 解析写入操作的目标路径（文件可能尚不存在）
///
/// 如果文件存在，直接规范化。如果不存在，从最近存在的祖先目录
/// 开始解析，防止 `../` 路径穿越。
///
/// 如果路径在工作区外，返回 OutsideWorkspace
pub fn resolve_safe_path_create(path_str: &str) -> Result<std::path::PathBuf, ToolError>;

/// 设置工作区根目录（启用路径穿越保护）
///
/// 设置的路径在存储前会被规范化。
/// 调用 set_workspace_root(None) 清除边界检查。
pub fn set_workspace_root(root: Option<std::path::PathBuf>) -> Result<(), ToolError>;

/// 返回当前设置的工作区根目录
pub fn workspace_root() -> Option<std::path::PathBuf>;
```

**使用示例**：
```rust
async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
    // 提取参数
    let url = require_string(&params, "url")?;
    let timeout = optional_u64(&params, "timeout").unwrap_or(30);
    let format = optional_string(&params, "format").unwrap_or_else(|| "text".to_string());

    // 执行操作
    match fetch_url(&url, timeout).await {
        Ok(content) => Ok(tool_success("", content)),
        Err(e) => Ok(tool_error("", format!("请求失败: {e}"))),
    }
}
```

---

## 6. 注册工具

### 6.1 直接注册（ToolRegistry）

最基础的注册方式：创建 `DefaultToolRegistry`，直接注册工具。

```rust
use std::sync::Arc;
use code_agent_core::tools::registry::{DefaultToolRegistry, ToolRegistry};

let mut registry = DefaultToolRegistry::new();

// 注册工具
registry.register(Arc::new(ReadFileTool::default()))?;
registry.register(Arc::new(WebSearchTool::new()))?;

// 查询工具
assert!(registry.get("read_file").is_some());
assert!(registry.get("web_search").is_some());

// 获取所有工具定义（发给模型）
let defs = registry.list();
for def in &defs {
    println!("Tool: {} - {}", def.name, def.description);
}

// 注销工具
registry.unregister("web_search");
```

**ToolRegistry trait 完整接口**：

```rust
pub trait ToolRegistry: Send + Sync {
    /// 注册一个工具。如果工具名已存在则返回 AlreadyRegistered 错误
    fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), ToolError>;

    /// 按名称查找工具。未找到返回 None
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>>;

    /// 返回所有已注册工具的元数据定义
    fn list(&self) -> Vec<ToolDefinition>;

    /// 按名称注销工具。不存在时静默忽略
    fn unregister(&mut self, name: &str);

    /// 返回已注册工具的数量
    fn len(&self) -> usize;

    /// 是否没有注册任何工具
    fn is_empty(&self) -> bool;
}
```

### 6.2 ToolRouter — 完整的执行管线

`ToolRouter` 将注册表、权限检查和钩子组合为完整的执行管线：

```rust
use std::sync::Arc;
use code_agent_core::tools::registry::{DefaultToolRegistry, ToolRegistry};
use code_agent_core::tools::permission::PermissionEnforcer;
use code_agent_core::tools::router::ToolRouter;
use code_agent_protocol::PermissionMode;

// 1. 创建注册表并注册工具
let mut reg = DefaultToolRegistry::new();
reg.register(Arc::new(WebSearchTool::new()))?;

// 2. 创建权限守卫
let permission = Arc::new(PermissionEnforcer::new(PermissionMode::Auto));

// 3. 创建路由器
let router = ToolRouter::new(Arc::new(reg), permission)
    .with_timeout(300);  // 全局超时 300 秒

// 4. 路由工具调用
let call = ToolCall {
    id: "call-001".into(),
    name: "web_search".into(),
    arguments: serde_json::json!({"query": "Rust async programming"}),
};
let result = router.route(&call).await;

if result.is_success() {
    println!("输出: {}", result.output.unwrap());
} else {
    eprintln!("错误: {}", result.error.unwrap());
}
```

### 6.3 通过 Plugin trait 注册

插件模式适合需要批量加载工具的场景（详见 [插件系统](#9-插件系统)）。

---

## 7. 执行管线

每次工具调用都经过完整的五阶段管线：

```
┌─────────────────────────────────────────────────────────┐
│                    ToolRouter::route()                    │
│                                                          │
│  ① PreExecute Hook  ──→  通知所有钩子工具即将执行         │
│          │                                               │
│          ▼                                               │
│  ② Registry Lookup  ──→  按名称查找工具实例               │
│          │                                               │
│          ▼                                               │
│  ③ Permission Check  ──→  按 CapabilityLevel 验证权限     │
│          │                                               │
│          ▼                                               │
│  ④ Execute          ──→  调用 tool.execute(params)       │
│          │                                               │
│          ▼                                               │
│  ⑤ PostExecute Hook ──→  通知所有钩子工具执行完成         │
│                                                          │
└─────────────────────────────────────────────────────────┘
```

### 阶段详解

| 阶段 | 失败时行为 | 说明 |
|------|-----------|------|
| ① PreExecute Hook | 继续执行（钩子不阻止） | 同步依次调用所有已注册钩子的 `on_event(&ToolEvent::PreExecute{...})` |
| ② Registry Lookup | 返回 `tool_error("...not found...")` | 在 ToolRegistry 中按名称查找工具 |
| ③ Permission Check | 返回 `tool_error("...permission denied...")` | 按当前 PermissionMode 和工具的 CapabilityLevel 做矩阵匹配 |
| ④ Execute | 返回 `tool_error("execution error: ...")` | 调用工具的 `execute()` 方法，带全局超时保护 |
| ⑤ PostExecute Hook | 继续执行（钩子不阻止） | 同步依次调用所有已注册钩子的 `on_event(&ToolEvent::PostExecute{...})` |

### PermissionMode vs CapabilityLevel 矩阵

| PermissionMode | CapabilityLevel::Read | CapabilityLevel::Edit | CapabilityLevel::Exec |
|----------------|----------------------|----------------------|----------------------|
| `Auto` | ✓ 放行 | ✓ 放行 | ✓ 放行 |
| `Permit` | ✓ 放行 | ✗ 拒绝 | ✗ 拒绝 |
| `Block` | ✓ 放行 | ✗ 拒绝 | ✗ 拒绝 |

---

## 8. 钩子系统

钩子（ToolHook）是工具生命周期的观察者，在工具执行前后触发。用于日志记录、指标收集、安全审计等场景。

### ToolHook trait

```rust
/// 工具生命周期钩子（观察者模式，仅通知不修改）
pub trait ToolHook: Send + Sync {
    /// 生命周期事件回调
    fn on_event(&self, event: &ToolEvent);
}
```

### ToolEvent 枚举

```rust
#[derive(Debug, Clone)]
pub enum ToolEvent {
    // 工具事件
    PreExecute {
        call_id: String,     // 工具调用唯一标识符
        tool_name: String,   // 工具名称
        params_hash: String, // 参数的哈希值（用于去重和日志）
    },
    PostExecute {
        call_id: String,
        tool_name: String,
        result: ToolResultMessage,  // 工具执行结果
        duration_ms: u64,           // 执行耗时（毫秒）
    },

    // 模型事件
    PreModelCall { snapshot: ModelCallSnapshot },
    PostModelCall { snapshot: ModelCallSnapshot, response_tokens: usize, elapsed_ms: u64 },

    // 会话事件
    SessionCreate { thread_id: String, session_id: String, system_instructions: String },
    SessionDestroy { thread_id: String, session_id: String },

    // 命令事件
    PreCommand { command: String, args: String },
    PostCommand { command: String, args: String, success: bool, elapsed_ms: u64 },
}
```

### 实现自定义钩子

```rust
use std::sync::atomic::{AtomicUsize, Ordering};
use code_agent_core::tools::hook::{ToolHook, ToolEvent, HookRegistry};

/// 计数钩子：记录工具调用次数
struct CountingHook {
    pre_count: AtomicUsize,
    post_count: AtomicUsize,
}

impl ToolHook for CountingHook {
    fn on_event(&self, event: &ToolEvent) {
        match event {
            ToolEvent::PreExecute { tool_name, .. } => {
                self.pre_count.fetch_add(1, Ordering::SeqCst);
                tracing::info!(%tool_name, "工具即将执行");
            }
            ToolEvent::PostExecute { tool_name, duration_ms, .. } => {
                self.post_count.fetch_add(1, Ordering::SeqCst);
                tracing::info!(%tool_name, %duration_ms, "工具执行完成");
            }
            _ => {} // 忽略不感兴趣的事件
        }
    }
}
```

### 注册钩子

```rust
// 通过 HookRegistry 批量管理
let mut hook_registry = HookRegistry::new();
hook_registry.register(Arc::new(LoggingHook));
hook_registry.register(Arc::new(MetricsHook));
hook_registry.dispatch(&ToolEvent::PreExecute {
    call_id: "call-001".into(),
    tool_name: "web_search".into(),
    params_hash: "abc123".into(),
});

// 或附加到 ToolRouter
let router = ToolRouter::new(registry, permission)
    .with_hook(Arc::new(MyAuditHook));
```

### 性能约束

- 所有钩子在同一线程上按注册顺序**同步**调用
- 单个钩子阻塞会阻塞整个管线
- 如有耗时操作（如写文件、网络请求），在钩子内部将任务派发给后台线程

---

## 9. 插件系统

Plugin 是批量加载工具的扩展点。通过 Plugin trait，可以将一组相关工具（如来自外部 MCP 服务器的工具）一次性注册到 ToolRegistry。

### Plugin trait

```rust
#[async_trait]
pub trait Plugin: Send + Sync {
    /// 返回插件的唯一名称
    fn name(&self) -> &str;

    /// 加载插件 —— 将工具注册到 ToolRegistry
    async fn load(&self, registry: &mut dyn ToolRegistry)
        -> Result<(), Box<dyn std::error::Error>>;

    /// 返回插件提供的自定义命令（可选）
    fn commands(&self) -> Vec<Command> {
        Vec::new()
    }

    /// 卸载插件 —— 清理资源（可选）
    async fn unload(&self, _registry: &mut dyn ToolRegistry)
        -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
}
```

### 实现自定义插件

```rust
use async_trait::async_trait;
use std::sync::Arc;
use code_agent_core::tools::plugin::Plugin;
use code_agent_core::tools::registry::ToolRegistry;

struct WebToolsPlugin;

#[async_trait]
impl Plugin for WebToolsPlugin {
    fn name(&self) -> &str {
        "web-tools"
    }

    async fn load(&self, registry: &mut dyn ToolRegistry)
        -> Result<(), Box<dyn std::error::Error>>
    {
        registry.register(Arc::new(WebSearchTool::new()))?;
        registry.register(Arc::new(WebFetchTool::new()))?;
        Ok(())
    }
}
```

### PluginManager — 批量管理插件

```rust
use code_agent_core::tools::plugin::PluginManager;

let mut manager = PluginManager::new();

// 注册插件
manager.register(Box::new(WebToolsPlugin));
manager.register(Box::new(DatabasePlugin));

// 批量加载：将插件中的工具注册到 ToolRegistry
let mut registry = DefaultToolRegistry::new();
let results = manager.load_all(&mut registry).await;

for result in &results {
    match result {
        PluginLoadResult::Success { name } => {
            println!("插件 '{name}' 加载成功");
        }
        PluginLoadResult::Failed { name, error } => {
            eprintln!("插件 '{name}' 加载失败: {error}");
        }
    }
}

// 获取插件列表
println!("已注册插件: {:?}", manager.plugin_names());
```

**容错设计**：一个插件加载失败不会阻止其他插件加载。

---

## 10. 完整示例

以下是一个完整的自定义工具实现——`WebSearchTool`，使用 DuckDuckGo Instant Answer API 进行搜索：

```rust
// File: src/lib.rs
use async_trait::async_trait;
use code_agent_core::tools::{
    Tool, ToolError, ToolResultMessage,
    tool_success, require_string, optional_u64,
};
use code_agent_protocol::CapabilityLevel;
use reqwest::Client;

/// 网络搜索工具
///
/// 使用 DuckDuckGo Instant Answer API 进行搜索。
/// 无需 API Key。
pub struct WebSearchTool {
    client: Client,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("CraftCoder/0.1")
                .build()
                .expect("failed to create HTTP client"),
        }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web using DuckDuckGo Instant Answer API. \
         Returns search results as formatted text."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results (default: 5)"
                }
            },
            "required": ["query"]
        })
    }

    fn capability(&self) -> CapabilityLevel {
        CapabilityLevel::Read
    }

    async fn execute(&self, params: serde_json::Value) -> Result<ToolResultMessage, ToolError> {
        let query = require_string(&params, "query")?;
        let max_results = optional_u64(&params, "max_results").unwrap_or(5).min(10);

        let url = format!(
            "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
            urlencoding::encode(&query)
        );

        let response = self.client.get(&url).send().await.map_err(|e| {
            ToolError::execution_error(format!("HTTP 请求失败: {e}"))
        })?;

        let body: serde_json::Value = response.json().await.map_err(|e| {
            ToolError::execution_error(format!("JSON 解析失败: {e}"))
        })?;

        let mut output = String::new();
        output.push_str(&format!("Search results for: {query}\n\n"));

        // Parse Abstract (main answer)
        if let Some(abstract_text) = body.get("AbstractText").and_then(|v| v.as_str()) {
            if !abstract_text.is_empty() {
                output.push_str(&format!("Abstract: {abstract_text}\n"));
                if let Some(source) = body.get("AbstractURL").and_then(|v| v.as_str()) {
                    output.push_str(&format!("Source: {source}\n\n"));
                }
            }
        }

        // Parse Related Topics
        if let Some(topics) = body.get("RelatedTopics").and_then(|v| v.as_array()) {
            let mut count = 0u64;
            for topic in topics {
                if count >= max_results {
                    break;
                }
                if let Some(text) = topic.get("Text").and_then(|v| v.as_str()) {
                    count += 1;
                    output.push_str(&format!("{count}. {text}\n"));
                    if let Some(url) = topic.get("FirstURL").and_then(|v| v.as_str()) {
                        output.push_str(&format!("   URL: {url}\n"));
                    }
                    output.push('\n');
                }
            }
        }

        if output.trim().is_empty() {
            output = format!("No results found for '{query}'.");
        }

        Ok(tool_success("", output))
    }
}
```

### 配合 Plugin 加载

```rust
// 将 WebSearchTool 打包为插件
use async_trait::async_trait;
use std::sync::Arc;
use code_agent_core::tools::plugin::Plugin;
use code_agent_core::tools::registry::ToolRegistry;

struct MyToolsPlugin;

#[async_trait]
impl Plugin for MyToolsPlugin {
    fn name(&self) -> &str {
        "my-tools"
    }

    async fn load(&self, registry: &mut dyn ToolRegistry)
        -> Result<(), Box<dyn std::error::Error>>
    {
        registry.register(Arc::new(WebSearchTool::new()))?;
        registry.register(Arc::new(WebFetchTool::new()))?;
        Ok(())
    }
}
```

---

## 11. 测试指南

### 单元测试

利用内置的测试宏和辅助函数编写单元测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_missing_required_param() {
        let tool = WebSearchTool::new();
        let err = tool
            .execute(serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing required field"));
    }

    #[tokio::test]
    async fn test_valid_params() {
        let tool = WebSearchTool::new();
        let result = tool
            .execute(serde_json::json!({
                "query": "rust programming",
                "max_results": 3
            }))
            .await
            .unwrap();
        assert!(result.is_success());
        assert!(result.output.unwrap().contains("rust programming"));
    }

    #[tokio::test]
    async fn test_default_max_results() {
        let tool = WebSearchTool::new();
        let result = tool
            .execute(serde_json::json!({
                "query": "test"
            }))
            .await
            .unwrap();
        assert!(result.is_success());
    }

    #[tokio::test]
    async fn test_max_results_clamped() {
        let tool = WebSearchTool::new();
        let result = tool
            .execute(serde_json::json!({
                "query": "test",
                "max_results": 100  // Should be clamped to 10
            }))
            .await
            .unwrap();
        assert!(result.is_success());
    }
}
```

### 注册表测试

验证工具在注册表中的行为：

```rust
#[test]
fn test_registry_register_and_get() {
    let mut registry = DefaultToolRegistry::new();
    let tool = Arc::new(WebSearchTool::new());
    registry.register(tool).unwrap();
    assert!(registry.get("web_search").is_some());
    assert_eq!(registry.len(), 1);
}

#[test]
fn test_registry_duplicate_fails() {
    let mut registry = DefaultToolRegistry::new();
    registry.register(Arc::new(WebSearchTool::new())).unwrap();
    let err = registry.register(Arc::new(WebSearchTool::new())).unwrap_err();
    assert!(matches!(err, ToolError::AlreadyRegistered(_)));
}
```

### 管线测试

测试完整的 ToolRouter 管线（含钩子）：

```rust
#[tokio::test]
async fn test_router_with_hook() {
    let mut reg = DefaultToolRegistry::new();
    reg.register(Arc::new(WebSearchTool::new())).unwrap();

    let permission = Arc::new(PermissionEnforcer::new(PermissionMode::Auto));
    let hook = Arc::new(TestHook::new());

    let router = ToolRouter::new(Arc::new(reg), permission)
        .with_hook(hook);

    let call = ToolCall {
        id: "call-test".into(),
        name: "web_search".into(),
        arguments: serde_json::json!({"query": "test"}),
    };

    let result = router.route(&call).await;
    assert!(result.is_success());
}
```

### 运行测试

```bash
# 运行自定义工具的所有测试
cargo test -p your-tool-crate

# 带输出（查看 println! / tracing!）
cargo test -p your-tool-crate -- --nocapture
```

---

## 12. 集成指南

### 方式 A：通过 Cargo 依赖（推荐）

在你的 `Cargo.toml` 中添加依赖：

```toml
[package]
name = "my-craftcoder-tools"
version = "0.1.0"
edition = "2021"

[dependencies]
code-agent-core = { path = "../code-agent-rs/core" }
code-agent-protocol = { path = "../code-agent-rs/protocol" }
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
reqwest = { version = "0.12", features = ["json"] }
tokio = { version = "1", features = ["full"] }
```

然后通过 `PluginManager` 程序化加载：

```rust
use code_agent_core::tools::plugin::PluginManager;
use my_craftcoder_tools::MyToolsPlugin;

let mut manager = PluginManager::new();
manager.register(Box::new(MyToolsPlugin));

let mut registry = DefaultToolRegistry::new();
manager.load_all(&mut registry).await?;
```

### 方式 B：通过 config.toml 插件配置

在 `~/.config/code-agent/config.toml` 中配置插件：

```toml
# 自定义工具插件
[plugins.web_tools]
type = "custom"
path = "/path/to/my-tools/target/release/libmy_tools.so"
enabled = true

# 或配置 MCP 服务器（通过 MCP 协议加载远程工具）
[plugins.remote_search]
type = "mcp"
command = "node"
args = ["/path/to/search-server/index.js"]
```

### 方式 C：直接注册到 CraftCoder 启动代码

```rust
// 在 CraftCoder 的 main.rs 或初始化代码中
use std::sync::Arc;
use code_agent_core::tools::registry::{DefaultToolRegistry, ToolRegistry};

async fn init_tool_registry() -> DefaultToolRegistry {
    let mut registry = DefaultToolRegistry::new();

    // 注册内置工具
    registry.register(Arc::new(ReadFileTool::default())).unwrap();
    registry.register(Arc::new(WriteFileTool::default())).unwrap();
    // ...

    // 注册自定义工具
    registry.register(Arc::new(WebSearchTool::new())).unwrap();
    registry.register(Arc::new(WebFetchTool::new())).unwrap();

    registry
}
```

### 终端到端集成检查清单

1. [ ] `Tool` trait 实现完整（5 个方法）
2. [ ] `input_schema()` 返回合法的 JSON Schema，包含 `required` 字段
3. [ ] `capability()` 返回正确的权限级别
4. [ ] `execute()` 使用辅助函数提取参数
5. [ ] 成功时返回 `tool_success(...)`，失败时返回 `tool_error(...)` 或 `Err(ToolError::...)`
6. [ ] 所有依赖正确配置在 `Cargo.toml` 中
7. [ ] 工具包含单元测试（覆盖正常路径、参数缺失、错误路径）
8. [ ] 通过 Plugin trait 或直接注册集成到 ToolRegistry
9. [ ] 如需路径操作，使用 `resolve_safe_path` 进行安全验证
10. [ ] 对于只读工具，确认 `capability()` 返回 `CapabilityLevel::Read`

---

## 参考资源

- **SDK 源码**：`code-agent-rs/core/src/tools/`
- **内置工具示例**：`code-agent-rs/core/src/tools/builtin/`（`read_file.rs`、`write_file.rs`、`grep.rs` 等）
- **注册表实现**：`code-agent-rs/core/src/tools/registry.rs`
- **执行管线**：`code-agent-rs/core/src/tools/router.rs`
- **权限体系**：`code-agent-rs/core/src/tools/permission.rs`
- **钩子系统**：`code-agent-rs/core/src/tools/hook.rs`
- **插件系统**：`code-agent-rs/core/src/tools/plugin.rs`
- **示例项目**：`examples/web-search/`、`examples/web-fetch/`
