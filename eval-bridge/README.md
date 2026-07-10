# eval-bridge — Rust⇄Python 评估桥接层

通过 PyO3 在 Rust 中内嵌 Python 解释器，直接调用 `code-agent-eval-py` 的评估函数。
相比 JSON-over-subprocess 方式，消除了子进程开销和临时文件 I/O。

## 架构

```
┌─────────────────────┐     PyO3 FFI      ┌──────────────────────┐
│  code-agent-rs       │ ──────────────►   │  code-agent-eval-py  │
│  (Rust eval crate)   │                   │  (Python)            │
│                      │  run_eval()       │                      │
│  eval-bridge ◄───────┤  run_benchmark()  │  bridge.py           │
│  (this crate)        │ ◄──────────────── │  ├── run_single_py() │
│                      │   JSON result     │  └── run_batch_py()  │
└─────────────────────┘                    └──────────────────────┘
```

### 公共 API

| 函数 | 输入 | 输出 |
|------|------|------|
| `run_eval` | 单个 EvalTask JSON | 单个 EvalResult JSON |
| `run_benchmark` | EvalTask JSON 数组 | EvalResult JSON 数组 |

两个函数都是阻塞调用，内部获取 Python GIL。

## 编译要求

- Python 3.12+（含开发头文件）
- Rust 1.80+
- 平台：Windows / Linux / macOS

### Windows

需要安装 Python 调试符号（Visual Studio Installer → Python 开发支持），或设置 `PYTHONHOME` 环境变量。

### Linux

```bash
sudo apt install python3-dev python3-venv
```

### macOS

```bash
brew install python@3.12
```

## 使用

```rust
use code_agent_eval_bridge::{run_eval, run_benchmark};

// 单个任务
let result = run_eval(r#"{"task_id":"t","test_commands":["echo ok"]}"#)?;
println!("{result}");

// 批量任务
let tasks = r#"[
    {"task_id":"a","test_commands":["echo a"]},
    {"task_id":"b","test_commands":["echo b"]}
]"#;
let results = run_benchmark(tasks)?;
```

## 测试

```bash
cd eval-bridge
cargo test -- --test-threads=1
```

测试需要 `code-agent-eval-py` 已安装在当前 Python 环境中。

## 注意

**此 crate 独立于 `code-agent-rs` workspace。**

PyO3 有特殊的编译要求（Python 开发头文件、链接参数），如果加入 workspace 会导致非 Python 相关的 workspace 成员也需要安装 Python 开发环境。因此 `eval-bridge` 作为独立 crate 维护，由需要 Python 桥接的 crate 通过 path 依赖引入。

```toml
# 在需要使用桥接的 crate 中
[dependencies]
code-agent-eval-bridge = { path = "../eval-bridge" }
```

## 错误处理

| 错误类型 | 说明 |
|----------|------|
| `BridgeError::Import` | Python 包导入失败（`code_agent_eval` 未安装） |
| `BridgeError::Python` | Python 执行时抛出异常 |
| `BridgeError::Serialization` | JSON 序列化/反序列化失败 |

## 许可证

MIT
