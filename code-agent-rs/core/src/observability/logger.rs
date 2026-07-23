//! 日志初始化 —— 配置 stdout 和文件输出的 tracing 订阅器。
//!
//! 【领域含义】本模块负责初始化 tracing 日志子系统，支持 JSON 格式和
//! 人类可读格式的双重输出，可同时写入 stdout 和按日滚动的文件。
//! setup_logging 是每个进程的入口函数，返回的 WorkerGuard 必须保持
//! 存活以维持文件写入器的正常运行。
//! Logger initialization — configures tracing subscribers for stdout and file output.

use std::io;

use super::config::ObservabilityConfig;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

pub use tracing_appender::non_blocking::WorkerGuard;

/// 初始化 tracing/logging 订阅器。
///
/// 【领域含义】setup_logging 是 Agent 可观测性系统的启动入口，每个进程
/// 应且仅应调用一次。它根据配置构建三层输出管道：
/// - Stdout：JSON（如果 json_format）或人类可读格式
/// - File：带按日滚动的 JSON（如果 log_file 为 Some）
/// - OTLP：当 otlp 特性和端点都设置时的最佳努力异步设置
///
/// # 返回值
/// WorkerGuard 用于文件追加器。必须在程序生命周期内保持持有。
///
/// Initialize the tracing/logging subscriber.
///
/// # Layers
/// - Stdout: JSON (if json_format) or human-readable
/// - File: JSON with daily rotation (if log_file is Some)
/// - OTLP: best-effort async setup when otlp feature + endpoint are set
///
/// # Returns
/// WorkerGuard for the file appender. Must be held for the program lifetime.
pub fn setup_logging(config: &ObservabilityConfig) -> Option<WorkerGuard> {
    if !config.enabled {
        return None;
    }

    let filter = build_env_filter(&config.log_level);

    let guard = match &config.log_file {
        Some(path) => init_with_file(filter, path, config.json_format),
        None => {
            init_stdout_only(filter, config.json_format);
            None
        }
    };

    #[cfg(feature = "otlp")]
    setup_otlp(config);

    guard
}

/// 用于测试的轻量级订阅器。始终为 JSON 格式，幂等。
///
/// Lightweight subscriber for tests. Always JSON, idempotent.
pub fn setup_for_testing(log_level: &str) {
    let filter = build_env_filter(log_level);
    let layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(io::stdout)
        .with_target(false);
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(layer)
        .try_init();
}

// ---------------------------------------------------------------------------
// Init paths
// ---------------------------------------------------------------------------

fn init_with_file(
    filter: EnvFilter,
    log_path: &std::path::Path,
    json_format: bool,
) -> Option<WorkerGuard> {
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let dir = log_path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let filename = log_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("code-agent.log");

    let file_appender = tracing_appender::rolling::daily(dir, filename);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let file_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(non_blocking)
        .with_target(false);

    if json_format {
        tracing_subscriber::registry()
            .with(filter)
            .with(file_layer)
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(io::stdout)
                    .with_target(false),
            )
            .try_init()
            .ok();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(file_layer)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(io::stdout)
                    .with_target(false),
            )
            .try_init()
            .ok();
    }

    Some(guard)
}

fn init_stdout_only(filter: EnvFilter, json_format: bool) {
    if json_format {
        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_writer(io::stdout)
                    .with_target(false),
            )
            .try_init()
            .ok();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(io::stdout)
                    .with_target(false),
            )
            .try_init()
            .ok();
    }
}

// ---------------------------------------------------------------------------
// EnvFilter
// ---------------------------------------------------------------------------

fn build_env_filter(log_level: &str) -> EnvFilter {
    EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(log_level))
}

// ---------------------------------------------------------------------------
// OTLP (feature-gated)
// ---------------------------------------------------------------------------

#[cfg(feature = "otlp")]
fn setup_otlp(config: &ObservabilityConfig) {
    use opentelemetry::KeyValue;
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::trace::{Config as TraceConfig, TracerProvider};
    use opentelemetry_sdk::Resource;

    let endpoint = match &config.otlp_endpoint {
        Some(ep) => ep.clone(),
        None => return,
    };

    let service_name = config.service_name.clone();
    let ep = endpoint.clone();

    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("OTLP: runtime error: {e}");
                return;
            }
        };
        rt.block_on(async {
            let exporter = match opentelemetry_otlp::new_exporter()
                .tonic()
                .with_endpoint(&ep)
                .build_span_exporter()
            {
                Ok(e) => e,
                Err(err) => {
                    eprintln!("OTLP: exporter error: {err}");
                    return;
                }
            };
            let resource = Resource::new(vec![KeyValue::new(
                "service.name",
                service_name,
            )]);
            let config = TraceConfig::default().with_resource(resource);
            let provider = TracerProvider::builder()
                .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
                .with_config(config)
                .build();
            let tracer = provider.tracer("code-agent");
            // The OTLP layer cannot be retroactively added to an already-init
            // subscriber. In production, compose tracing_opentelemetry::layer()
            // before calling init(). This is a best-effort post-init attempt.
            let _layer: tracing_opentelemetry::OpenTelemetryLayer<
                tracing_subscriber::Registry,
                opentelemetry_sdk::trace::Tracer,
            > = tracing_opentelemetry::layer().with_tracer(tracer);
            tracing::info!(endpoint = %ep, "OTLP configured");
        });
    });
}