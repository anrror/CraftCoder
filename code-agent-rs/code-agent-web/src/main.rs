//! Binary entry point for the Code Agent Web Server.
//!
//! Launches an Axum-based HTTP server with REST + SSE endpoints for
//! managing agent threads. Supports CORS for local development, API key
//! authentication, and optional static file serving for the Monaco chat UI.
//!
//! # Usage
//!
//! ```bash
//! # With API key
//! code-agent-web --api-key sk-abc
//!
//! # Custom host/port
//! code-agent-web --host 0.0.0.0 --port 8080
//!
//! # Without auth (localhost only)
//! code-agent-web
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use clap::Parser;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use tracing::info;
use tracing_subscriber::EnvFilter;

use code_agent_core::model::{create_model_client, ModelConfig, ProviderKind};
use code_agent_core::tools::registry::DefaultToolRegistry;
use code_agent_web::{routes, AppState};

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

/// Code Agent Web Server — REST + SSE API for agent thread management.
#[derive(Parser, Debug)]
#[command(name = "code-agent-web", version, about)]
struct Cli {
    /// Host to bind to.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Port to listen on.
    #[arg(long, default_value = "3000")]
    port: u16,

    /// API key for web authentication (default: LLM_API_KEY env var).
    /// C7/C15: Separated from LLM model API key — use --llm-api-key or LLM_API_KEY
    /// for the model key instead.
    #[arg(long, env = "WEB_API_KEY")]
    api_key: Option<String>,

    /// LLM model API key (default: LLM_API_KEY env var).
    /// C7/C15: Use this for model access; --api-key/WEB_API_KEY is for web auth.
    #[arg(long, env = "LLM_API_KEY")]
    llm_api_key: Option<String>,

    /// Model name to use for chat completions.
    #[arg(long)]
    model: Option<String>,

    /// Base URL for the OpenAI-compatible API gateway.
    #[arg(long)]
    base_url: Option<String>,

    /// Sampling temperature (0.0–2.0, default: 0.2).
    #[arg(long, default_value = "0.2")]
    temperature: f32,

    /// Max tokens per completion (default: 4096).
    #[arg(long, default_value = "4096")]
    max_tokens: u32,

    /// Path to static files directory (optional, for Monaco UI).
    #[arg(long, default_value = "static")]
    static_dir: String,

    /// Allow all origins (CORS). Default: localhost only.
    /// P1: 生产环境应使用反向代理处理 CORS，而不是在应用层全部放开。
    #[arg(long)]
    allow_all_origins: bool,

    /// Log level (default: info).
    #[arg(long, default_value = "info")]
    log_level: String,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(&cli.log_level)),
        )
        .init();

    info!(
        "Starting Code Agent Web Server v{}",
        env!("CARGO_PKG_VERSION")
    );

    // Build model config
    let model_config = build_model_config(&cli);

    // Create model client
    let model_client = create_model_client(ProviderKind::from_model_name(&model_config.model), model_config);

    // Create tool registry and register all built-in tools + MCP plugins
    let mut tool_registry = DefaultToolRegistry::new();
    code_agent_tools::register_all_core_tools(&mut tool_registry);
    code_agent_tools::mcp::register_from_env(&mut tool_registry);

    // Build application state
    let api_keys = cli.api_key
        .map(|k| {
            let mut m = HashMap::new();
            m.insert(k, String::new());
            m
        })
        .unwrap_or_default();
    let state = Arc::new(
        AppState::new(api_keys)
            .with_model_client(model_client)
            .with_tool_registry(Arc::new(tool_registry)),
    );

    // Build CORS layer — P1: 默认仅允许 localhost，生产环境需显式 opt-in
    let cors = if cli.allow_all_origins {
        info!("CORS: allowing all origins (--allow-all-origins enabled)");
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
    } else {
        info!("CORS: restricting to localhost (default)");
        // Allow http://localhost:* and http://127.0.0.1:*
        CorsLayer::new()
            .allow_origin([
                "http://localhost:3000".parse().unwrap(),
                "http://localhost:5173".parse().unwrap(),  // Vite dev server
                "http://127.0.0.1:3000".parse().unwrap(),
                "http://127.0.0.1:5173".parse().unwrap(),
            ])
            .allow_methods(Any)
            .allow_headers(Any)
    };

    // Build router with API routes + static file serving
    let app = routes::build_router(state)
        .layer(cors)
        .nest_service("/", ServeDir::new(&cli.static_dir));

    let addr = format!("{}:{}", cli.host, cli.port);
    info!("Listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind address");

    axum::serve(listener, app)
        .await
        .expect("Server failed");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_model_config(cli: &Cli) -> ModelConfig {
    // C7/C15: Use separate llm_api_key arg, not web api_key
    let api_key = cli
        .llm_api_key
        .clone()
        .or_else(|| cli.api_key.clone())
        .or_else(|| std::env::var("LLM_API_KEY").ok());

    let base_url = cli
        .base_url
        .clone()
        .or_else(|| std::env::var("LLM_API_BASE_URL").ok());

    let mut builder = ModelConfig::builder()
        .api_key(api_key.expect("API key required: set --api-key or LLM_API_KEY"))
        .temperature(cli.temperature)
        .max_tokens(cli.max_tokens)
        .stream(true);

    if let Some(url) = base_url {
        builder = builder.api_base_url(url);
    }

    if let Some(model) = cli.model.clone().or_else(|| std::env::var("LLM_CHAT_MODEL").ok()) {
        builder = builder.model(model);
    }

    builder
        .build()
        .expect("model config must be valid")
}
