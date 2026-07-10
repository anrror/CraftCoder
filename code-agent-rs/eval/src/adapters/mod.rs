//! Benchmark adapter trait and registry.
//!
//! Each benchmark (HumanEval, SWE-bench-Live, etc.) implements
//! [`BenchmarkAdapter`] to convert its native task format into
//! [`crate::EvalTask`] and to convert raw agent output into
//! [`crate::EvalResult`].
//!
//! ## Architecture
//!
//! ```text
//! Benchmark dataset ──► Adapter.load_tasks() ──► Vec<EvalTask>
//!                                                       │
//!                                               EvalRunner.run_batch()
//!                                                       │
//! Agent output ──────────► Adapter.process_result() ──► EvalResult
//! ```

pub mod human_eval;
pub mod swe_bench;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

use async_trait::async_trait;

use crate::types::{EvalResult, EvalTask};

// ---------------------------------------------------------------------------
// AgentOutput
// ---------------------------------------------------------------------------

/// Agent 原始输出
///
/// 【领域含义】Agent 尝试解决基准测试任务时产生的原始输出，不同基准测试解释不同字段。
/// 【核心职责】作为 BenchmarkAdapter 的输入，承载 Agent 生成的代码、补丁或自由文本。
///
/// 字段映射：
/// - **HumanEval**: 使用 `code` — 生成的函数体。
/// - **SWE-bench-Live**: 使用 `patch` — 生成的 diff/补丁。
/// - **通用**: 使用 `content` — 自由文本输出。
#[derive(Debug, Clone, Default)]
pub struct AgentOutput {
    /// 自由文本输出 — Agent 的原始文本输出。
    pub content: String,
    /// 代码块 — 提取的函数体、类等（可选）。
    pub code: Option<String>,
    /// 补丁 — Agent 生成的统一 diff 格式补丁（可选）。
    pub patch: Option<String>,
    /// 退出码 — Agent 进程的退出码（可选）。
    pub exit_code: Option<i32>,
}

impl AgentOutput {
    /// 从纯文本创建输出
    ///
    /// 【领域含义】使用自由文本内容构造 AgentOutput。
    /// 【核心职责】提供便捷构造器，适用于通用场景。
    pub fn from_content(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            code: None,
            patch: None,
            exit_code: None,
        }
    }

    /// 从生成的代码创建输出
    ///
    /// 【领域含义】使用生成的代码块构造 AgentOutput，适用于 HumanEval 等代码生成基准。
    /// 【核心职责】提供便捷构造器，设置 code 字段。
    pub fn from_code(code: impl Into<String>) -> Self {
        Self {
            content: String::new(),
            code: Some(code.into()),
            patch: None,
            exit_code: None,
        }
    }

    /// 从补丁/diff 创建输出
    ///
    /// 【领域含义】使用补丁内容构造 AgentOutput，适用于 SWE-bench 等补丁生成基准。
    /// 【核心职责】提供便捷构造器，设置 patch 字段。
    pub fn from_patch(patch: impl Into<String>) -> Self {
        Self {
            content: String::new(),
            code: None,
            patch: Some(patch.into()),
            exit_code: None,
        }
    }

    /// 判断输出是否为空
    ///
    /// 【领域含义】检查 AgentOutput 是否包含任何非空数据。
    /// 【核心职责】供调用方判断 Agent 是否产生了有效输出。
    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
            && self.code.as_ref().map_or(true, |c| c.is_empty())
            && self.patch.as_ref().map_or(true, |p| p.is_empty())
    }
}

// ---------------------------------------------------------------------------
// BenchmarkAdapter trait
// ---------------------------------------------------------------------------

/// 基准测试适配器
///
/// 【领域含义】将特定基准测试的原生格式转换为 eval crate 的标准 EvalTask/EvalResult 类型。
/// 【核心职责】定义基准测试适配器的领域接口，实现者需提供名称、任务加载和结果处理能力。
/// 实现者必须满足 Send + Sync，以便存储在全局适配器注册表中并在异步任务间共享。
#[async_trait]
pub trait BenchmarkAdapter: Send + Sync {
    /// 基准测试名称 — 人类可读的标识符（如 "human_eval"、"swe_bench_live"）。
    fn name(&self) -> &str;

    /// 加载所有任务
    ///
    /// 【领域含义】从基准测试数据集加载所有任务，转换为标准 EvalTask。
    /// 【核心职责】解析数据集文件，为每个任务生成合适的 setup 和 test 命令。
    async fn load_tasks(&self) -> Result<Vec<EvalTask>, AdapterError>;

    /// 处理 Agent 输出
    ///
    /// 【领域含义】将 Agent 的原始输出转换为结构化的评估结果。
    /// 【核心职责】执行基准测试特定的评分逻辑：
    /// - HumanEval: 对生成的函数运行测试用例。
    /// - SWE-bench-Live: 应用补丁并运行 FAIL_TO_PASS 测试。
    async fn process_result(
        &self,
        task: &EvalTask,
        output: &AgentOutput,
    ) -> Result<EvalResult, AdapterError>;
}

// ---------------------------------------------------------------------------
// AdapterError
// ---------------------------------------------------------------------------

/// 适配器错误
///
/// 【领域含义】表示适配器操作过程中可能发生的领域错误。
/// 【核心职责】封装数据集加载、任务查找、Agent 输出解析、基础设施故障等各类异常场景。
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    /// 数据集加载失败 — 无法读取或解析数据集文件。
    #[error("Failed to load dataset: {0}")]
    DatasetLoad(String),

    /// 任务未找到 — 引用的任务在数据集中不存在。
    #[error("Task not found: {0}")]
    TaskNotFound(String),

    /// Agent 输出解析失败 — 未找到有效的代码/补丁。
    #[error("Failed to parse agent output: {0}")]
    Parse(String),

    /// 评估基础设施故障 — Docker、子进程等。
    #[error("Evaluation error: {0}")]
    Evaluation(String),

    /// I/O 错误。
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON 序列化错误。
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// 网络错误 — 数据集下载过程中发生。
    #[cfg(feature = "remote-datasets")]
    #[error("Network error: {0}")]
    Network(#[from] reqwest::Error),
}

// ---------------------------------------------------------------------------
// Adapter registry
// ---------------------------------------------------------------------------

/// 全局适配器注册表
///
/// 【领域含义】已注册基准测试适配器的全局注册表，支持并发读写。
/// 【核心职责】以 Arc<dyn BenchmarkAdapter> 形式存储适配器实例，支持跨异步任务的廉价克隆和共享。
static REGISTRY: LazyLock<RwLock<HashMap<String, Arc<dyn BenchmarkAdapter>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

/// Helper: acquire a read lock, recovering from poison.
fn registry_read() -> std::sync::RwLockReadGuard<'static, HashMap<String, Arc<dyn BenchmarkAdapter>>> {
    REGISTRY.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Helper: acquire a write lock, recovering from poison.
fn registry_write() -> std::sync::RwLockWriteGuard<'static, HashMap<String, Arc<dyn BenchmarkAdapter>>> {
    REGISTRY.write().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 注册基准测试适配器
///
/// 【领域含义】将适配器注册到全局注册表中，供后续按名称查找使用。
/// 【核心职责】向全局注册表插入适配器实例。
///
/// # Panics
/// 如果同名适配器已注册则 panic。
pub fn register_adapter(adapter: impl BenchmarkAdapter + 'static) {
    let mut registry = registry_write();
    let name = adapter.name().to_string();
    if registry.contains_key(&name) {
        panic!("adapter '{name}' already registered");
    }
    registry.insert(name, Arc::new(adapter));
}

/// 按名称获取适配器
///
/// 【领域含义】从全局注册表中按名称查找已注册的基准测试适配器。
/// 【核心职责】返回适配器的 Arc 引用，支持跨异步任务共享。
pub fn get_adapter(name: &str) -> Option<Arc<dyn BenchmarkAdapter>> {
    let registry = registry_read();
    registry.get(name).cloned()
}

/// 列出所有已注册的适配器名称
///
/// 【领域含义】返回全局注册表中所有已注册适配器的名称列表。
/// 【核心职责】供 CLI 和诊断工具展示可用基准测试。
pub fn list_adapters() -> Vec<String> {
    let registry = registry_read();
    registry.keys().cloned().collect()
}

/// 清空适配器注册表
///
/// 【领域含义】移除注册表中所有适配器，主要用于测试场景。
/// 【核心职责】确保测试之间注册表状态隔离。
pub fn clear_registry() {
    let mut registry = registry_write();
    registry.clear();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::EvalStatus;

    // Dummy adapter for registry testing.
    struct DummyAdapter {
        name: String,
    }

    #[async_trait]
    impl BenchmarkAdapter for DummyAdapter {
        fn name(&self) -> &str {
            &self.name
        }

        async fn load_tasks(&self) -> Result<Vec<EvalTask>, AdapterError> {
            Ok(vec![EvalTask {
                task_id: format!("{}/0", self.name),
                test_commands: vec!["echo hello".into()],
                ..Default::default()
            }])
        }

        async fn process_result(
            &self,
            _task: &EvalTask,
            _output: &AgentOutput,
        ) -> Result<EvalResult, AdapterError> {
            Ok(EvalResult {
                task_id: format!("{}/0", self.name),
                status: EvalStatus::Pass,
                score: Some(1.0),
                logs: vec![],
                patch: None,
                metrics: Default::default(),
            })
        }
    }

    #[test]
    fn agent_output_is_empty() {
        let empty = AgentOutput::default();
        assert!(empty.is_empty());

        let with_content = AgentOutput::from_content("x");
        assert!(!with_content.is_empty());

        let with_code = AgentOutput::from_code("def f(): pass");
        assert!(!with_code.is_empty());

        let with_patch = AgentOutput::from_patch("--- a\n+++ b");
        assert!(!with_patch.is_empty());
    }

    #[test]
    fn agent_output_default_is_all_none() {
        let o = AgentOutput::default();
        assert!(o.content.is_empty());
        assert!(o.code.is_none());
        assert!(o.patch.is_none());
        assert!(o.exit_code.is_none());
    }

    #[test]
    fn adapter_error_display() {
        let e = AdapterError::DatasetLoad("file not found".into());
        assert!(e.to_string().contains("file not found"));

        let e = AdapterError::TaskNotFound("abc".into());
        assert!(e.to_string().contains("abc"));

        let e = AdapterError::Parse("bad format".into());
        assert!(e.to_string().contains("bad format"));
    }

    #[test]
    fn adapter_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "no file");
        let adapter_err: AdapterError = io_err.into();
        assert!(adapter_err.to_string().contains("no file"));
    }

    #[test]
    fn adapter_error_from_json() {
        let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let adapter_err: AdapterError = json_err.into();
        assert!(adapter_err.to_string().contains("JSON"));
    }

    // ------------------------------------------------------------------
    // Registry tests
    // ------------------------------------------------------------------

    #[test]
    fn registry_register_and_get() {
        clear_registry();
        register_adapter(DummyAdapter {
            name: "dummy".into(),
        });

        let retrieved = get_adapter("dummy");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name(), "dummy");

        assert!(get_adapter("nonexistent").is_none());
        clear_registry();
    }

    #[test]
    fn registry_list_adapters() {
        clear_registry();
        register_adapter(DummyAdapter {
            name: "a".into(),
        });
        register_adapter(DummyAdapter {
            name: "b".into(),
        });

        let mut names = list_adapters();
        names.sort();
        assert_eq!(names, vec!["a", "b"]);
        clear_registry();
    }

    #[test]
    #[should_panic(expected = "already registered")]
    fn registry_duplicate_panics() {
        clear_registry();
        register_adapter(DummyAdapter {
            name: "dup".into(),
        });
        register_adapter(DummyAdapter {
            name: "dup".into(),
        });
    }
}
