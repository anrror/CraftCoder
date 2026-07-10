//! Data types for the eval runner.
//!
//! Mirrors the Pydantic models in `code-agent-eval-py/code_agent_eval/models.py`
//! with Rust-friendly serde serialisation.

use serde::{Deserialize, Serialize};

/// 评估状态
///
/// 【领域含义】表示单个评估任务执行结果的领域枚举，是评估流程的核心状态机。
/// 【核心职责】区分任务通过/失败/错误/超时四种终态，供指标计算和报告生成使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvalStatus {
    /// 通过 — 所有测试命令返回码为 0 且输出匹配预期。
    Pass,
    /// 失败 — 测试命令返回非零码或输出不匹配。
    Fail,
    /// 错误 — 内部错误（沙箱崩溃、超时等）。
    Error,
    /// 超时 — 任务超过配置的超时时间。
    Timeout,
}

impl EvalStatus {
    /// 判断是否通过
    ///
    /// 【领域含义】快速判断任务是否处于通过状态。
    /// 【核心职责】供指标计算和报告生成时过滤通过的任务。
    pub fn is_pass(self) -> bool {
        matches!(self, Self::Pass)
    }

    /// 判断是否为失败终态
    ///
    /// 【领域含义】判断任务是否处于失败、错误或超时等非通过终态。
    /// 【核心职责】供指标计算时统计失败任务数量。
    pub fn is_failure(self) -> bool {
        matches!(self, Self::Fail | Self::Error | Self::Timeout)
    }
}

/// 评估指标
///
/// 【领域含义】评估过程中收集的性能和资源消耗指标值对象。
/// 【核心职责】记录任务耗时、Token 消耗和 Agent 交互轮次，供成本分析和效率评估使用。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EvalMetrics {
    /// 耗时（毫秒）— 任务执行的墙钟时间。
    pub duration_ms: u64,
    /// Token 消耗 — Agent 消耗的预估 Token 数量。
    pub tokens_used: u32,
    /// 交互轮次 — Agent 执行的编辑/提示轮次数。
    pub turns_taken: u32,
}

/// 评估任务
///
/// 【领域含义】表示单个评估任务的输入聚合，包含执行测试所需的所有指令和参数。
/// 【核心职责】封装测试命令、环境准备脚本和超时配置，供 EvalRunner 执行。
/// 字段与 Python 侧的 EvalTask Pydantic 模型对齐，额外支持 SWE-bench 风格的 repository/base_commit 字段。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalTask {
    /// 任务标识 — 唯一标识符（如 "HumanEval/0"、"swe-bench/org__repo-123"）。
    pub task_id: String,

    /// 环境准备命令 — 测试前设置沙箱环境的 Shell 命令列表。
    #[serde(default)]
    pub setup_commands: Vec<String>,

    /// 测试命令 — 构成实际测试的 Shell 命令列表。
    pub test_commands: Vec<String>,

    /// 预期输出 — 若设置，将 stdout 与此值进行子串匹配（标准化空白后）。
    #[serde(default)]
    pub expected_output: Option<String>,

    /// 超时秒数 — 整个任务的最大墙钟时间。
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    /// Git 仓库 URL — 用于 SWE-bench 风格的任务。
    #[serde(default)]
    pub repository: Option<String>,

    /// 基准提交哈希 — SWE-bench 风格任务的缺陷基线提交。
    #[serde(default)]
    pub base_commit: Option<String>,
}

const fn default_timeout_secs() -> u64 {
    120
}

impl Default for EvalTask {
    fn default() -> Self {
        Self {
            task_id: String::new(),
            setup_commands: Vec::new(),
            test_commands: Vec::new(),
            expected_output: None,
            timeout_secs: default_timeout_secs(),
            repository: None,
            base_commit: None,
        }
    }
}

/// 评估结果
///
/// 【领域含义】表示单个评估任务执行结果的输出聚合，包含状态、分数、日志和指标。
/// 【核心职责】封装评估任务的完整输出信息，供指标计算和报告生成使用。
/// 字段与 Python 侧的 EvalResult Pydantic 模型对齐。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalResult {
    /// 任务标识 — 与输入 EvalTask 的 task_id 对应。
    pub task_id: String,

    /// 结果状态 — 通过/失败/错误/超时。
    pub status: EvalStatus,

    /// 分数 — 0.0 到 1.0 之间的数值评分。
    #[serde(default)]
    pub score: Option<f64>,

    /// 日志 — 沙箱的 stdout + stderr 合并输出，每行一条。
    #[serde(default)]
    pub logs: Vec<String>,

    /// 补丁 — 评估过程中产生的 diff/代码变更（可选）。
    #[serde(default)]
    pub patch: Option<String>,

    /// 性能指标 — 运行过程中收集的耗时、Token 消耗等。
    #[serde(default)]
    pub metrics: EvalMetrics,
}

impl EvalResult {
    /// 创建错误结果
    ///
    /// 【领域含义】当基础设施在 Python 侧产生结果之前失败时，构造一个错误状态的 EvalResult。
    /// 【核心职责】提供便捷构造器，用于基础设施故障场景（如 Docker 不可用、子进程启动失败）。
    pub fn error(task_id: impl Into<String>, logs: Vec<String>) -> Self {
        Self {
            task_id: task_id.into(),
            status: EvalStatus::Error,
            score: Some(0.0),
            logs,
            patch: None,
            metrics: EvalMetrics::default(),
        }
    }

    /// 创建超时结果
    ///
    /// 【领域含义】当任务超过配置的超时时间时，构造一个超时状态的 EvalResult。
    /// 【核心职责】提供便捷构造器，用于超时场景，记录实际耗时。
    pub fn timeout(task_id: impl Into<String>, duration_ms: u64) -> Self {
        Self {
            task_id: task_id.into(),
            status: EvalStatus::Timeout,
            score: Some(0.0),
            logs: vec!["Task timed out".to_string()],
            patch: None,
            metrics: EvalMetrics {
                duration_ms,
                tokens_used: 0,
                turns_taken: 0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_eval_task_from_json() {
        let json = r#"{
            "task_id": "test/0",
            "test_commands": ["echo ok"],
            "expected_output": "ok",
            "timeout_secs": 30
        }"#;

        let task: EvalTask = serde_json::from_str(json).expect("deserialize EvalTask");
        assert_eq!(task.task_id, "test/0");
        assert_eq!(task.test_commands, vec!["echo ok"]);
        assert_eq!(task.expected_output, Some("ok".to_string()));
        assert_eq!(task.timeout_secs, 30);
        assert!(task.setup_commands.is_empty());
        assert!(task.repository.is_none());
    }

    #[test]
    fn parse_eval_task_minimal() {
        let json = r#"{"task_id": "minimal", "test_commands": ["true"]}"#;
        let task: EvalTask = serde_json::from_str(json).expect("deserialize EvalTask");
        assert_eq!(task.timeout_secs, 120); // default
        assert_eq!(task.expected_output, None);
    }

    #[test]
    fn serialize_eval_result_to_json() {
        let result = EvalResult {
            task_id: "t/0".into(),
            status: EvalStatus::Pass,
            score: Some(1.0),
            logs: vec!["hello".into()],
            patch: None,
            metrics: EvalMetrics {
                duration_ms: 500,
                tokens_used: 42,
                turns_taken: 3,
            },
        };
        let json = serde_json::to_string(&result).expect("serialize EvalResult");
        let parsed: EvalResult = serde_json::from_str(&json).expect("roundtrip");
        assert_eq!(parsed.task_id, "t/0");
        assert_eq!(parsed.status, EvalStatus::Pass);
        assert_eq!(parsed.metrics.duration_ms, 500);
    }

    #[test]
    fn eval_status_is_pass_and_failure() {
        assert!(EvalStatus::Pass.is_pass());
        assert!(!EvalStatus::Pass.is_failure());

        assert!(!EvalStatus::Fail.is_pass());
        assert!(EvalStatus::Fail.is_failure());

        assert!(!EvalStatus::Error.is_pass());
        assert!(EvalStatus::Error.is_failure());

        assert!(!EvalStatus::Timeout.is_pass());
        assert!(EvalStatus::Timeout.is_failure());
    }

    #[test]
    fn eval_result_error_constructor() {
        let r = EvalResult::error("task/99", vec!["boom".into()]);
        assert_eq!(r.status, EvalStatus::Error);
        assert_eq!(r.score, Some(0.0));
        assert_eq!(r.logs, vec!["boom"]);
    }

    #[test]
    fn eval_result_timeout_constructor() {
        let r = EvalResult::timeout("task/1", 30_000);
        assert_eq!(r.status, EvalStatus::Timeout);
        assert_eq!(r.metrics.duration_ms, 30_000);
    }
}
