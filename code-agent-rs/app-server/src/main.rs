//! Binary entry point for the Code Agent App Server.
//!
//! Launches a JSON-RPC 2.0 server over stdio that IDE extensions (VS Code,
//! IntelliJ, etc.) can connect to. The server reads requests from stdin
//! and writes responses + event notifications to stdout.
//!
//! # Usage
//!
//! ```bash
//! # With API key from environment (LLM_API_KEY)
//! code-agent-app-server
//!
//! # With explicit configuration
//! code-agent-app-server --api-key sk-abc --model custom-model
//!
//! # Custom base URL
//! code-agent-app-server --api-key sk-abc --base-url https://api.example.com/v1
//! ```
//!
//! # Protocol
//!
//! See [`code_agent_app_server::types`] for the JSON-RPC 2.0 envelope types
//! and method parameter structs.

use clap::Parser;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::EnvFilter;

use code_agent_app_server::server::run_stdio_server;
use code_agent_core::agent::ThreadManager;
use code_agent_core::model::{create_model_client, ModelConfig, ProviderKind};
use code_agent_core::tools::registry::DefaultToolRegistry;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

/// Code Agent App Server — JSON-RPC 2.0 over stdio for IDE integration.
#[derive(Parser, Debug)]
#[command(name = "code-agent-app-server", version, about)]
struct Cli {
    /// API key for the model provider (default: LLM_API_KEY env var).
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

    /// Log level (default: info).
    #[arg(long, default_value = "info")]
    log_level: String,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    // Parse CLI
    let cli = Cli::parse();

    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new(&cli.log_level)),
        )
        .with_writer(std::io::stderr) // Log to stderr to avoid corrupting stdout protocol
        .init();

    info!("Starting Code Agent App Server v{}", env!("CARGO_PKG_VERSION"));

    // Build model config
    let model_config = build_model_config(&cli);

    // Create model client
    let model_client = create_model_client(ProviderKind::from_model_name(&model_config.model), model_config);

    // Create tool registry and register all built-in tools
    let mut tool_registry = DefaultToolRegistry::new();
    code_agent_tools::register_all_core_tools(&mut tool_registry);

    // Create thread manager (max 100 concurrent threads)
    let thread_manager = Arc::new(std::sync::Mutex::new(ThreadManager::new(100)));

    info!("Server ready. Waiting for JSON-RPC requests on stdin...");

    // Run the stdio server (blocks until stdin EOF)
    run_stdio_server(
        thread_manager,
        Some(model_client),
        Some(Arc::new(tool_registry)),
        Arc::new(std::sync::Mutex::new(false)),
        Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
    )
    .await;

    info!("Server shutting down.");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn build_model_config(cli: &Cli) -> ModelConfig {
    // Try env-based config first, then override with CLI args
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
