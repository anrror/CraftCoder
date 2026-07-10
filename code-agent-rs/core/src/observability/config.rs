//! 可观测性配置 —— 追踪、日志和可选 OTLP 导出的配置。
//!
//! 【领域含义】ObservabilityConfig 是 Agent 可观测性子系统的总配置聚合。
//! 它设计为环境驱动：每个字段均可通过环境变量设置，并为开发和
//! 生产环境提供合理的默认值。配置包括主开关、日志级别、JSON 格式、
//! 文件输出路径和 OTLP 导出端点。
//! Configuration for the observability subsystem.
//!
//! ObservabilityConfig controls tracing, logging, and optional OTLP export.
//! It is designed to be environment-driven: every field can be set via env var,
//! with sensible defaults for development and production.

use std::path::PathBuf;

/// 顶层可观测性配置。
///
/// 【领域含义】ObservabilityConfig 聚合了 Agent 可观测性的所有配置项，
/// 包括是否启用、日志级别、输出格式、文件路径和 OTLP 导出设置。
/// 它是可观测性子系统的单一配置入口，支持通过环境变量覆盖和
/// Builder 模式链式调用。
///
/// # 环境变量
///
/// | 变量                            | 字段              | 默认值          |
/// |--------------------------------|--------------------|----------------|
/// | CODE_AGENT_LOG_LEVEL           | log_level          | "info"          |
/// | CODE_AGENT_LOG_FILE            | log_file           | None            |
/// | CODE_AGENT_LOG_JSON            | json_format        | true            |
/// | CODE_AGENT_OTLP_ENDPOINT       | otlp_endpoint      | None            |
/// | CODE_AGENT_OBSERVABILITY       | enabled            | true            |
///
/// Top-level observability configuration.
///
/// # Environment Variables
///
/// | Variable                        | Field              | Default        |
/// |--------------------------------|--------------------|----------------|
/// | CODE_AGENT_LOG_LEVEL           | log_level          | "info"         |
/// | CODE_AGENT_LOG_FILE            | log_file           | None           |
/// | CODE_AGENT_LOG_JSON            | json_format        | true           |
/// | CODE_AGENT_OTLP_ENDPOINT       | otlp_endpoint      | None           |
/// | CODE_AGENT_OBSERVABILITY       | enabled            | true           |
#[derive(Clone, Debug)]
pub struct ObservabilityConfig {
    /// 主开关。当为 false 时，所有追踪和日志被禁用，Tracer 方法变为空操作。
    ///
    /// Master switch. When false, all tracing and logging is disabled and
    /// the Tracer methods become no-ops.
    pub enabled: bool,

    /// 日志级别过滤器："trace"、"debug"、"info"、"warn"、"error"。
    /// 直接传递给 tracing_subscriber::EnvFilter。
    ///
    /// Log level filter: "trace", "debug", "info", "warn", "error".
    /// Passed directly to tracing_subscriber::EnvFilter.
    pub log_level: String,

    /// 文件日志输出的可选路径。日志按日轮转。
    /// 当为 None 时，日志仅输出到 stdout。
    ///
    /// Optional path for file-based log output. Logs are rotated daily.
    /// When None, logs go to stdout only.
    pub log_file: Option<PathBuf>,

    /// 当为 true 时，日志以 JSON 行格式发射（机器可读）。
    /// 当为 false 时，日志使用人类可读格式。
    ///
    /// When true, logs are emitted as JSON lines (machine-readable).
    /// When false, logs use a human-readable format.
    pub json_format: bool,

    /// OTLP 收集器端点（如 "http://localhost:4317"）。
    /// 需要 otlp 特性。当为 None 时，不进行 OTLP 导出。
    ///
    /// OTLP collector endpoint (e.g., "http://localhost:4317").
    /// Requires the otlp feature. When None, no OTLP export occurs.
    pub otlp_endpoint: Option<String>,

    /// 在 OTLP 资源属性中报告的服务名称。
    /// 默认值："code-agent"。
    ///
    /// Service name reported in OTLP resource attributes.
    /// Default: "code-agent".
    pub service_name: String,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            log_level: "info".to_string(),
            log_file: None,
            json_format: true,
            otlp_endpoint: None,
            service_name: "code-agent".to_string(),
        }
    }
}

impl ObservabilityConfig {
    /// 从环境变量构建配置，失败时回退到默认值。
    ///
    /// # Panics
    ///
    /// 不 panic。所有环境变量解析失败时静默回退到默认值。
    ///
    /// Build configuration from environment variables, falling back to defaults.
    ///
    /// # Panics
    ///
    /// Does not panic. All env-var parsing failures silently fall back to defaults.
    pub fn from_env() -> Self {
        let enabled = std::env::var("CODE_AGENT_OBSERVABILITY")
            .map(|v| v != "0" && v != "false" && v != "off")
            .unwrap_or(true);

        let log_level = std::env::var("CODE_AGENT_LOG_LEVEL")
            .unwrap_or_else(|_| "info".to_string());

        let log_file = std::env::var("CODE_AGENT_LOG_FILE")
            .ok()
            .map(PathBuf::from);

        let json_format = std::env::var("CODE_AGENT_LOG_JSON")
            .map(|v| v != "0" && v != "false")
            .unwrap_or(true);

        let otlp_endpoint = std::env::var("CODE_AGENT_OTLP_ENDPOINT").ok();

        let service_name = std::env::var("CODE_AGENT_SERVICE_NAME")
            .unwrap_or_else(|_| "code-agent".to_string());

        Self {
            enabled,
            log_level,
            log_file,
            json_format,
            otlp_endpoint,
            service_name,
        }
    }

    /// 便捷 Builder：启用/禁用整个可观测性子系统。
    ///
    /// Convenience builder: enable / disable the entire observability subsystem.
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// 便捷 Builder：设置日志级别。
    ///
    /// Convenience builder: set the log level.
    pub fn with_log_level(mut self, level: impl Into<String>) -> Self {
        self.log_level = level.into();
        self
    }

    /// 便捷 Builder：设置日志文件路径。
    ///
    /// Convenience builder: set the log file path.
    pub fn with_log_file(mut self, path: Option<PathBuf>) -> Self {
        self.log_file = path;
        self
    }

    /// 便捷 Builder：设置 OTLP 端点。
    ///
    /// Convenience builder: set the OTLP endpoint.
    pub fn with_otlp_endpoint(mut self, endpoint: Option<String>) -> Self {
        self.otlp_endpoint = endpoint;
        self
    }
}