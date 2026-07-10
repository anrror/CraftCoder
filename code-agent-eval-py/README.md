# code-agent-eval-py — AI Coding Agent 评估运行器

Python 评估环境，通过 Docker 沙箱运行 HumanEval、SWE-bench-Live 等基准测试。
作为 `code-agent-rs` 评估管线的 Python 后端，通过 JSON-over-subprocess 或 PyO3 桥接集成。

## 目录结构

```
code-agent-eval-py/
├── code_agent_eval/
│   ├── __init__.py      # 包入口，导出 EvalTask / EvalResult / EvalRunner / DockerSandbox
│   ├── models.py        # Pydantic v2 数据模型（EvalTask, EvalResult, EvalStatus）
│   ├── runner.py        # EvalRunner — 并行任务编排（ThreadPoolExecutor）
│   ├── sandbox.py       # DockerSandbox — 容器化沙箱执行（docker run --rm）
│   ├── bridge.py        # Python 侧桥接函数（供 PyO3 Rust 层调用）
│   └── cli.py           # Click CLI 入口
├── tests/
│   ├── test_models_roundtrip.py
│   └── test_sandbox.py
├── Dockerfile           # 评估环境容器镜像
├── pyproject.toml
└── README.md
```

## 安装

```bash
pip install -e .
```

依赖（自动安装）：
- `pydantic>=2.0` — 数据模型与序列化
- `docker>=7.0` — Docker SDK（当前 sandbox 使用 subprocess 调用 docker CLI，预留 SDK 接口）
- `click>=8.0` — CLI 框架

测试依赖：
```bash
pip install -e ".[test]"
```

## CLI 使用

```bash
# 查看帮助
python -m code_agent_eval.cli --help

# 运行评估任务
python -m code_agent_eval.cli run tasks.json

# 指定 Docker 镜像和并发数
python -m code_agent_eval.cli run tasks.json --image python:3.12-slim --max-workers 8

# 输出到文件
python -m code_agent_eval.cli run tasks.json -o results.json
```

`tasks.json` 格式（JSON 数组）：

```json
[
  {
    "task_id": "HumanEval/0",
    "setup_commands": ["pip install pytest"],
    "test_commands": ["python -c \"assert add(1, 2) == 3\""],
    "expected_output": null,
    "timeout": 120
  }
]
```

也可通过注册的 `code-agent-eval` 命令调用：

```bash
code-agent-eval run tasks.json
```

## 数据模型

### EvalTask（输入）

| 字段 | 类型 | 说明 |
|------|------|------|
| `task_id` | `str` | 唯一标识符 |
| `setup_commands` | `list[str]` | 环境准备命令 |
| `test_commands` | `list[str]` | 测试命令 |
| `expected_output` | `str \| None` | 预期输出（子串匹配） |
| `timeout` | `int` | 超时秒数（默认 120） |

### EvalResult（输出）

| 字段 | 类型 | 说明 |
|------|------|------|
| `task_id` | `str` | 对应输入任务 ID |
| `status` | `EvalStatus` | `pass` / `fail` / `error` |
| `score` | `float` | 0.0–1.0 |
| `logs` | `str` | 合并的 stdout + stderr |
| `patch` | `str \| None` | 评估产生的 diff |

## 沙箱执行

每个任务在独立的 Docker 容器中运行，容器自动清理（`--rm`）：

- 网络隔离：`--network none`
- 资源限制：`--memory 4g --cpus 2`
- 命令链：`set -e; set -o pipefail; <setup>; <test>`
- 超时控制：`subprocess.run(timeout=...)`

## 与 Rust eval crate 的集成

### 方式一：JSON over subprocess（默认）

`code-agent-rs/eval` crate 中的 `EvalRunner` 将任务写入临时 JSON 文件，然后 spawn Python 子进程：

```rust
// code-agent-rs/eval/src/runner.rs
let output = TokioCommand::new("python")
    .arg("-m")
    .arg("code_agent_eval.cli")
    .arg("run")
    .arg(&temp_path)
    .output()
    .await?;
```

Python CLI 输出 JSON 结果数组到 stdout，Rust 侧反序列化为 `EvalResult`。

### 方式二：PyO3 直接调用（eval-bridge）

通过 `eval-bridge` crate 在 Rust 进程中内嵌 Python 解释器，直接调用 Python 函数：

```rust
use code_agent_eval_bridge::{run_eval, run_benchmark};

let result = run_eval(r#"{"task_id":"t","test_commands":["echo ok"]}"#)?;
```

Python 侧对应的桥接函数在 `bridge.py` 中：

```python
def run_single_py(task_json: str) -> str: ...
def run_batch_py(tasks_json: str) -> str: ...
```

## 开发测试

```bash
# 安装测试依赖
pip install -e ".[test]"

# 运行测试
pytest tests/

# 跳过需要 Docker 的测试
pytest tests/ -m "not skip_if_no_docker"
```

测试文件：
- `test_models_roundtrip.py` — EvalTask / EvalResult 序列化往返
- `test_sandbox.py` — Docker 沙箱执行（需要 Docker 守护进程）

## Docker 镜像

项目自带 `Dockerfile`，用于构建评估环境容器：

```bash
docker build -t code-agent-eval:latest -f code-agent-eval-py/Dockerfile .
```

镜像包含 Python 3.12、git、jq、curl 等评估常用工具，默认入口为 `code-agent-eval --help`。

## 许可证

MIT
