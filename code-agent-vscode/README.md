# code-agent-vscode — 定制码匠 VS Code 扩展

VS Code 扩展，为 AI Coding Agent 提供原生 IDE 集成。
通过 JSON-RPC 连接 `code-agent-app-server`，实现聊天面板、内联代码编辑 diff、语义搜索和状态栏监控。

## 功能

- **聊天面板** — 在 VS Code 侧边栏中与 AI 代理对话，支持流式输出
- **内联代码编辑 diff** — 代理生成的代码变更以 diff 形式展示，可逐块接受或拒绝
- **语义搜索** — 通过 TreeView 管理多线程会话
- **状态栏** — 实时显示连接状态、模型名称、Token 用量
- **Fix This** — 选中代码后右键快速修复
- **线程管理** — 创建、分叉、归档对话线程

## 架构

```
┌─────────────────────────────────────────────────┐
│  VS Code Extension (TypeScript)                 │
│                                                  │
│  extension.ts ─── 入口，注册命令、管理生命周期      │
│  ├── appServer/ ── JSON-RPC 客户端                │
│  │   ├── jsonRpcClient.ts ── 传输层（stdio）       │
│  │   └── types.ts ────────── 类型定义              │
│  ├── chat/ ─────── WebView 聊天面板                │
│  │   ├── webviewProvider.ts ── WebView 提供者      │
│  │   └── index.ts ──────────── 导出               │
│  ├── editor/ ───── 内联 diff 装饰器                │
│  │   ├── diffDecorations.ts ── diff 渲染与操作     │
│  │   └── index.ts ──────────── 导出               │
│  └── statusBar.ts ── 状态栏组件                    │
│                                                  │
│  JSON-RPC (stdio)                                │
│         │                                        │
│         ▼                                        │
│  code-agent-app-server (Rust)                    │
│   - 线程管理 / 流式响应 / 工具调用                 │
└─────────────────────────────────────────────────┘
```

通信协议：扩展通过子进程启动 `code-agent-app-server`，通过 stdin/stdout 进行 JSON-RPC 通信。支持流式事件推送（`agent_message_delta`、`tool_call_begin`、`tool_call_end`）。

## 安装

### 从 VS Code 市场安装

搜索 "AI Coding Agent" 并安装。

### VSIX 手动安装

```bash
# 打包
npm run package

# 安装
code --install-extension code-agent-vscode-0.1.0.vsix
```

## 开发

```bash
# 克隆项目后
cd code-agent-vscode

# 安装依赖
npm install

# 编译 TypeScript
npm run compile

# 监听模式
npm run watch

# 启动调试（VS Code）
# 按 F5 或运行 "Extension" launch configuration
```

### 调试说明

1. 确保 `code-agent-app-server` 已编译并在 PATH 中，或通过配置指定路径
2. 在 VS Code 中按 F5 启动 Extension Development Host
3. 扩展会自动启动 app-server 子进程并建立连接

## 配置项

在 VS Code 设置中搜索 `codeAgent`：

| 配置项 | 类型 | 默认值 | 说明 |
|--------|------|--------|------|
| `codeAgent.appServerPath` | `string` | `""` | app-server 二进制路径，为空则使用扩展内置 |
| `codeAgent.modelName` | `string` | `""` | 模型名称，传递给 app-server |
| `codeAgent.autoStart` | `boolean` | `true` | 启动 VS Code 时自动启动 app-server |
| `codeAgent.maxIterations` | `number` | `20` | 每次对话的最大 ReAct 循环轮次 |
| `codeAgent.diffPreviewMode` | `string` | `"inline"` | diff 显示模式：`inline` / `side-by-side` / `none` |

## 命令列表

| 命令 | 标题 | 快捷键 | 说明 |
|------|------|--------|------|
| `code-agent.start` | Start Agent | — | 启动 app-server 并建立连接 |
| `code-agent.chat` | Open Chat | — | 打开聊天面板 |
| `code-agent.fix` | Fix This | — | 修复选中的代码（编辑器右键菜单） |
| `code-agent.stop` | Stop Agent | — | 断开连接并停止 app-server |
| `code-agent.newThread` | New Thread | — | 创建新对话线程 |
| `code-agent.forkThread` | Fork Thread | — | 从现有线程分叉新线程 |
| `code-agent.archiveThread` | Archive Thread | — | 归档对话线程 |

## 事件流

扩展通过 JSON-RPC 事件机制接收 app-server 的实时推送：

```
turn_started        → 对话轮次开始
agent_message_delta → 流式消息内容（增量）
tool_call_begin     → 工具调用开始
tool_call_end       → 工具调用结束（含结果）
turn_complete       → 对话轮次完成（含最终消息）
token_usage         → Token 用量更新
error               → 错误信息
```

## 许可证

MIT
