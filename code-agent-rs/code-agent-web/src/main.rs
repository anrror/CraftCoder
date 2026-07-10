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

use std::sync::Arc;

use clap::Parser;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use tracing::info;
use tracing_subscriber::EnvFilter;

use code_agent_core::model::{ModelConfig, Qwen3OpenAIClient};
use code_agent_core::tools::builtin::{
    edit_file::EditFileTool, glob::GlobTool, grep::GrepTool, list_dir::ListDirTool,
    read_file::ReadFileTool, write_file::WriteFileTool,
};
use code_agent_core::tools::registry::ToolRegistry;
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

    /// API key for authentication (default: LLM_API_KEY env var).
    #[arg(long, env = "LLM_API_KEY")]
    api_key: Option<String>,

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
    let model_client = Qwen3OpenAIClient::new(model_config);

    // Create tool registry and register all built-in tools
    let mut tool_registry = ToolRegistry::new();
    register_builtin_tools(&mut tool_registry);

    // Build application state
    let state = Arc::new(
        AppState::new(cli.api_key)
            .with_model_client(Arc::new(model_client))
            .with_tool_registry(Arc::new(tool_registry)),
    );

    // Build CORS layer (allow all origins for local development)
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

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

/// Register all built-in tools into the registry.
fn register_builtin_tools(registry: &mut ToolRegistry) {
    let tools: Vec<Arc<dyn code_agent_core::tools::Tool>> = vec![
        Arc::new(ReadFileTool::default()),
        Arc::new(WriteFileTool),
        Arc::new(EditFileTool),
        Arc::new(ListDirTool),
        Arc::new(GrepTool),
        Arc::new(GlobTool),
    ];
    for tool in tools {
        registry
            .register(tool)
            .expect("built-in tool must register without conflict");
    }
}

fn build_model_config(cli: &Cli) -> ModelConfig {
    let api_key = cli
        .api_key
        .clone()
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
