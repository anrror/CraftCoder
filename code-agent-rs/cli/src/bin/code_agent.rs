//! Code Agent — AI Coding Agent CLI entry point.
//!
//! ```text
//! Usage: code-agent <COMMAND>
//!
//! Commands:
//!   tui          Launch the interactive terminal UI
//!   exec         Execute a prompt in headless mode (CI/automation)
//!   app-server   Start the JSON-RPC 2.0 app server over stdio
//!   config       Manage configuration (get, set, path, init)
//!   eval         Run evaluation benchmarks
//!   doctor       Diagnose the environment (model, tools, config)
//!   help         Print this message or the help of the given subcommand(s)
//!
//! Options:
//!   -m, --model <MODEL>          Model name override
//!   -t, --timeout <SECS>         Timeout in seconds
//!       --api-key <KEY>          API key override
//!       --api-base-url <URL>     API base URL override
//!   -h, --help                   Print help
//!   -V, --version                Print version
//! ```

use std::path::{Path, PathBuf};
use std::process;

use clap::{Parser, Subcommand};

use code_agent_cli::config::{ConfigManager, Config};
use code_agent_cli::exec::{ExecCli, OutputFormat};
use code_agent_app_server::AppServer;

// ---------------------------------------------------------------------------
// CLI definition
// ---------------------------------------------------------------------------

/// AI Coding Agent — terminal-native pair programming assistant.
#[derive(Parser)]
#[command(
    name = "code-agent",
    version = env!("CARGO_PKG_VERSION"),
    about = "AI Coding Agent — terminal-native pair programming assistant",
    long_about = None,
)]
struct Cli {
    /// Model name override (e.g. "qwen3.6-27b", "gpt-4o")
    #[arg(short = 'm', long, global = true)]
    model: Option<String>,

    /// API key for the model provider
    #[arg(long = "api-key", global = true)]
    api_key: Option<String>,

    /// API base URL override
    #[arg(long = "api-base-url", global = true)]
    api_base_url: Option<String>,

    /// Execution timeout in seconds
    #[arg(short = 't', long, global = true)]
    timeout: Option<u64>,

    /// Maximum agent iterations per turn
    #[arg(long = "max-iterations", global = true)]
    max_iterations: Option<u32>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Launch the interactive terminal UI (TUI).
    ///
    /// Provides a full ratatui-based interface with chat pane,
    /// input bar, and status bar. Press Ctrl+C to exit.
    Tui {
        /// Initial prompt to send (optional, skips the first input)
        prompt: Option<String>,
    },

    /// Execute a prompt in headless mode and exit.
    ///
    /// Useful for CI pipelines, scripting, and automation.
    /// Reads stdin for additional context when piped.
    Exec {
        /// The task prompt to execute
        prompt: String,

        /// Output format: text, json, or silent
        #[arg(short = 'o', long, default_value = "text")]
        output: String,
    },

    /// Start the JSON-RPC 2.0 app server over stdin/stdout.
    ///
    /// IDE plugins connect via stdio to manage agent threads.
    /// Implements the MCP-compatible app-server protocol.
    AppServer,

    /// Manage code-agent configuration.
    #[command(subcommand)]
    Config(ConfigCmd),

    /// Run evaluation benchmarks.
    #[command(subcommand)]
    Eval(EvalCmd),

    /// Diagnose the environment and report issues.
    ///
    /// Checks: config files, model API connectivity, tool availability,
    /// Rust toolchain, and workspace health.
    Doctor,
}

// ── Config subcommands ─────────────────────────────────────────────

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the fully-resolved configuration.
    Show,

    /// Get a specific config value by key.
    Get {
        /// Config key (e.g. "model", "timeout_secs", "api_base_url")
        key: String,
    },

    /// Set a config value (writes to global config).
    Set {
        /// Config key
        key: String,
        /// New value
        value: String,
    },

    /// Print the path to the global and project-local config files.
    Path,

    /// Initialize a new project-local config file from defaults.
    Init {
        /// Target: "global" or "project" (default: project)
        #[arg(long, default_value = "project")]
        target: String,
    },
}

// ── Eval subcommands ───────────────────────────────────────────────

#[derive(Subcommand)]
enum EvalCmd {
    /// Run a benchmark suite.
    Run {
        /// Benchmark name: "humaneval" or "swe-bench-live"
        benchmark: String,

        /// Path to dataset file or cache directory
        #[arg(long)]
        dataset: Option<PathBuf>,

        /// Number of tasks to run
        #[arg(long, default_value = "50")]
        tasks: usize,

        /// Dataset split (for SWE-bench: lite, verified, full)
        #[arg(long, default_value = "lite")]
        split: String,

        /// Output JSON file for results
        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Compare two result files and generate a report.
    Compare {
        /// Path to baseline result JSON
        #[arg(long)]
        baseline: PathBuf,

        /// Path to current result JSON
        #[arg(long)]
        current: PathBuf,

        /// Output report file
        #[arg(long, default_value = "eval-report.md")]
        output: PathBuf,

        /// Output format: markdown, html, or json
        #[arg(long, default_value = "markdown")]
        format: String,
    },

    /// Check for performance regressions against a baseline.
    CheckRegression {
        /// Path to baseline result JSON
        #[arg(long)]
        baseline: PathBuf,

        /// Path to current result JSON
        #[arg(long)]
        current: PathBuf,

        /// Regression threshold in percentage points
        #[arg(long, default_value = "3.0")]
        threshold: f64,
    },
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    // Initialize tracing for structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "code_agent=info".into()),
        )
        .init();

    let cli = Cli::parse();

    // Build configuration from all layers
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let config = build_config(&cli, &cwd);

    let exit_code = match cli.command {
        Command::Tui { prompt } => cmd_tui(config, prompt).await,
        Command::Exec { prompt, output } => cmd_exec(config, prompt, output).await,
        Command::AppServer => cmd_app_server(config).await,
        Command::Config(cmd) => cmd_config(config, cmd),
        Command::Eval(cmd) => cmd_eval(config, cmd).await,
        Command::Doctor => cmd_doctor(config),
    };

    process::exit(exit_code);
}

// ---------------------------------------------------------------------------
// config builder helper
// ---------------------------------------------------------------------------

fn build_config(cli: &Cli, cwd: &Path) -> Config {
    let mut builder = ConfigManager::load_all(cwd);

    if let Some(ref v) = cli.model {
        builder = builder.model(v.clone());
    }
    if let Some(v) = cli.api_key.as_ref() {
        builder = builder.api_key(v.clone());
    }
    if let Some(v) = cli.api_base_url.as_ref() {
        builder = builder.api_base_url(v.clone());
    }
    if let Some(v) = cli.timeout {
        builder = builder.timeout_secs(v);
    }
    if let Some(v) = cli.max_iterations {
        builder = builder.max_iterations(v);
    }

    builder.build()
}

// ---------------------------------------------------------------------------
// cmd_tui
// ---------------------------------------------------------------------------

async fn cmd_tui(config: Config, initial_prompt: Option<String>) -> i32 {
    use code_agent_cli::tui::App;
    use code_agent_core::agent::{Session, SessionConfig};
    use code_agent_core::model::{ModelClient, ModelConfig, Qwen3OpenAIClient};
    use code_agent_core::tools::registry::ToolRegistry;
    use code_agent_protocol::{Message, PermissionMode, SessionId, ThreadId, TurnInput};
    use std::sync::Arc;
    use tokio::sync::mpsc;

    tracing::info!("Launching TUI...");

    // Create channels for bidirectional communication
    let (agent_input_tx, mut agent_input_rx) = mpsc::unbounded_channel::<String>();
    let (agent_event_tx, agent_event_rx) = mpsc::unbounded_channel::<code_agent_cli::tui::events::AppEvent>();

    // Build model client from config or env
    let model_client: Arc<dyn ModelClient> = if !config.model.is_empty() && !config.api_key.is_empty() {
        let mc = ModelConfig::builder()
            .model(config.model.clone())
            .api_key(config.api_key.clone())
            .api_base_url(config.api_base_url.clone())
            .stream(true)
            .build()
            .expect("model config must be valid");
        Arc::new(Qwen3OpenAIClient::new(mc))
    } else if let (Ok(api_key), Ok(model)) = (std::env::var("LLM_API_KEY"), std::env::var("LLM_CHAT_MODEL")) {
        let mc = ModelConfig::builder()
            .model(model)
            .api_key(api_key)
            .stream(true)
            .build()
            .expect("model config must be valid");
        Arc::new(Qwen3OpenAIClient::new(mc))
    } else {
        eprintln!("error: LLM model not configured. Set LLM_API_KEY and LLM_CHAT_MODEL, or configure via TUI config.");
        return 1;
    };

    // Create tool registry
    let tool_registry = Arc::new(ToolRegistry::new());

    // Create session
    let session_config = SessionConfig {
        id: SessionId::from(format!("tui-{}", uuid::Uuid::new_v4())),
        system_instructions: String::new(),
        max_iterations: config.max_iterations as usize,
        permission_mode: PermissionMode::Auto,
        model_client: Arc::clone(&model_client),
        tool_registry: Arc::clone(&tool_registry),
    };
    let mut session = Session::new(session_config).await;

    // Spawn background task: receive user input → run session turn → emit events
    tokio::spawn(async move {
        while let Some(text) = agent_input_rx.recv().await {
            let turn_input = TurnInput {
                thread_id: ThreadId::from("tui-thread"),
                messages: vec![Message::UserMessage { content: text }],
            };
            let events = session.run_turn(turn_input).await;
            for event in events {
                if agent_event_tx.send(code_agent_cli::tui::events::AppEvent::Agent(event)).is_err() {
                    break;
                }
            }
            let _ = agent_event_tx.send(code_agent_cli::tui::events::AppEvent::AgentStreamEnded);
        }
    });

    // Create TUI app with agent channels
    let app_agent_tx = agent_input_tx.clone();
    let mut app = match App::new(app_agent_tx, agent_event_rx) {
        Ok(app) => app,
        Err(e) => {
            eprintln!("error: failed to initialize TUI: {e}");
            return 1;
        }
    };

    if let Some(prompt) = initial_prompt {
        tracing::info!(prompt = %prompt, "Initial prompt provided");
        // Send initial prompt to agent via the channel
        let _ = agent_input_tx.send(prompt);
    }

    if let Err(e) = app.run().await {
        app.cleanup().ok();
        eprintln!("error: TUI run failed: {e}");
        return 1;
    }

    app.cleanup().ok();
    0
}

// ---------------------------------------------------------------------------
// cmd_exec
// ---------------------------------------------------------------------------

async fn cmd_exec(config: Config, prompt: String, output: String) -> i32 {
    let output_format = match OutputFormat::from_str(&output) {
        Ok(fmt) => fmt,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };

    let cli = ExecCli::new(prompt)
        .timeout_secs(config.timeout_secs)
        .output_format(output_format)
        .cwd(config.work_dir.clone());

    let cli = if !config.model.is_empty() {
        cli.model(config.model)
    } else {
        cli
    };

    match cli.execute().await {
        Ok(code) => code.as_i32(),
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

// ---------------------------------------------------------------------------
// cmd_app_server
// ---------------------------------------------------------------------------

async fn cmd_app_server(_config: Config) -> i32 {
    tracing::info!("Starting JSON-RPC 2.0 app server over stdio...");

    let mut server = AppServer::new();

    // Model client and tool registry would be configured from the resolved
    // config in a full integration. For now, start empty and let the
    // initialize handshake inject them.

    match server.start().await {
        Ok(()) => {
            tracing::info!("App server stopped");
            0
        }
        Err(e) => {
            eprintln!("error: app server failed: {e}");
            1
        }
    }
}

// ---------------------------------------------------------------------------
// cmd_config
// ---------------------------------------------------------------------------

fn cmd_config(config: Config, cmd: ConfigCmd) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    match cmd {
        ConfigCmd::Show => {
            println!("model             = {}", config.model);
            println!("api_base_url      = {}", config.api_base_url);
            println!("timeout_secs      = {}", config.timeout_secs);
            println!("max_iterations    = {}", config.max_iterations);
            println!("permission_mode   = {}", config.permission_mode);
            println!("output_format     = {}", config.output_format);
            println!("work_dir          = {}", config.work_dir.display());
            println!("cache_dir         = {}", config.cache_dir.display());
            println!("system_instructions = {}", config.system_instructions);
            if config.tool_allowlist.is_empty() {
                println!("tool_allowlist    = (all)");
            } else {
                println!("tool_allowlist    = [{}]", config.tool_allowlist.join(", "));
            }
            0
        }

        ConfigCmd::Get { key } => {
            match key.as_str() {
                "model" => println!("{}", config.model),
                "api_base_url" => println!("{}", config.api_base_url),
                "timeout_secs" => println!("{}", config.timeout_secs),
                "max_iterations" => println!("{}", config.max_iterations),
                "permission_mode" => println!("{}", config.permission_mode),
                "output_format" => println!("{}", config.output_format),
                "work_dir" => println!("{}", config.work_dir.display()),
                "cache_dir" => println!("{}", config.cache_dir.display()),
                "system_instructions" => println!("{}", config.system_instructions),
                "tool_allowlist" => {
                    if config.tool_allowlist.is_empty() {
                        println!("(all)");
                    } else {
                        println!("{}", config.tool_allowlist.join(", "));
                    }
                }
                other => {
                    eprintln!("error: unknown config key '{other}'");
                    return 1;
                }
            }
            0
        }

        ConfigCmd::Set { key, value } => {
            // Load existing global config, update one field, save back
            let mut existing = ConfigManager::load_global().unwrap_or_default();

            match key.as_str() {
                "model" => existing.model = Some(value),
                "api_key" => existing.api_key = Some(value),
                "api_base_url" => existing.api_base_url = Some(value),
                "timeout_secs" => {
                    existing.timeout_secs = Some(value.parse().map_err(|_| {
                        eprintln!("error: invalid number '{value}'");
                        process::exit(1);
                    }).unwrap_or_default());
                }
                "max_iterations" => {
                    existing.max_iterations = Some(value.parse().map_err(|_| {
                        eprintln!("error: invalid number '{value}'");
                        process::exit(1);
                    }).unwrap_or_default());
                }
                "permission_mode" => existing.permission_mode = Some(value),
                "output_format" => existing.output_format = Some(value),
                "system_instructions" => existing.system_instructions = Some(value),
                other => {
                    eprintln!("error: unknown config key '{other}'");
                    return 1;
                }
            }

            match ConfigManager::save_global(&existing) {
                Ok(path) => {
                    println!("Saved to {}", path.display());
                    0
                }
                Err(e) => {
                    eprintln!("error: failed to save config: {e}");
                    1
                }
            }
        }

        ConfigCmd::Path => {
            if let Some(global) = ConfigManager::global_config_path() {
                println!("global  = {}", global.display());
            } else {
                println!("global  = (not found — no config directory)");
            }
            let project = ConfigManager::project_config_path(&cwd);
            println!("project = {}", project.display());
            0
        }

        ConfigCmd::Init { target } => {
            let config = code_agent_cli::config::AgentConfig::default();
            match target.as_str() {
                "global" => match ConfigManager::save_global(&config) {
                    Ok(path) => {
                        println!("Initialized global config at {}", path.display());
                        0
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        1
                    }
                },
                _ => match ConfigManager::save_project(&cwd, &config) {
                    Ok(path) => {
                        println!("Initialized project config at {}", path.display());
                        0
                    }
                    Err(e) => {
                        eprintln!("error: {e}");
                        1
                    }
                },
            }
        }
    }
}

// ---------------------------------------------------------------------------
// cmd_eval
// ---------------------------------------------------------------------------

async fn cmd_eval(config: Config, cmd: EvalCmd) -> i32 {
    use code_agent_eval::{
        adapters::human_eval::HumanEvalAdapter,
        adapters::swe_bench::SWEBenchLiveAdapter,
        EvalRunner, MetricsCalculator,
        RegressionDetector,
        metrics::report::ReportGenerator,
    };

    match cmd {
        EvalCmd::Run {
            benchmark,
            dataset,
            tasks,
            split,
            output,
        } => {
            tracing::info!(benchmark = %benchmark, tasks, "Running benchmark");

            let cache_dir = config.cache_dir.clone();
            let script = PathBuf::from("code-agent-eval-py/code_agent_eval/cli.py");
            let runner = EvalRunner::new(&script);

            match benchmark.as_str() {
                "humaneval" | "human_eval" | "HumanEval" => {
                    let dataset_path = match dataset {
                        Some(p) => p,
                        None => {
                            eprintln!("error: --dataset PATH is required for HumanEval (JSONL file)");
                            return 1;
                        }
                    };

                    if !dataset_path.exists() {
                        eprintln!("error: dataset not found: {}", dataset_path.display());
                        return 1;
                    }

                    let adapter = HumanEvalAdapter::new(&dataset_path);
                    let problems = match adapter.parse_problems() {
                        Ok(p) => p,
                        Err(e) => {
                            eprintln!("error: failed to parse dataset: {e}");
                            return 1;
                        }
                    };

                    let eval_tasks: Vec<_> = problems
                        .iter()
                        .take(tasks)
                        .map(HumanEvalAdapter::problem_to_task)
                        .collect();

                    let results = match runner.run_batch(eval_tasks).await {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("error: eval runner failed: {e}");
                            return 1;
                        }
                    };

                    let calc = MetricsCalculator::default();
                    let report = calc.compute_all_named(&results, "HumanEval");
                    print_benchmark_summary(&report);

                    if let Some(out_path) = output {
                        save_eval_results(&results, &report, &out_path);
                    } else {
                        let default_out = cache_dir.join("humaneval-results.json");
                        save_eval_results(&results, &report, &default_out);
                    }
                    0
                }

                "swe-bench-live" | "swe_bench_live" | "swb" => {
                    let data_dir = dataset.unwrap_or_else(|| cache_dir.clone());
                    let adapter = SWEBenchLiveAdapter::new(&data_dir, &split);

                    let eval_tasks = match adapter.load_tasks(Some(tasks)).await {
                        Ok(t) => t,
                        Err(e) => {
                            eprintln!("error: failed to load SWE-bench tasks: {e}");
                            return 1;
                        }
                    };

                    let results = match runner.run_batch(eval_tasks).await {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!("error: eval runner failed: {e}");
                            return 1;
                        }
                    };

                    let calc = MetricsCalculator::default();
                    let report = calc.compute_all_named(&results, "SWE-bench-Live");
                    print_benchmark_summary(&report);

                    if let Some(out_path) = output {
                        save_eval_results(&results, &report, &out_path);
                    } else {
                        let default_out = cache_dir.join("swe-bench-results.json");
                        save_eval_results(&results, &report, &default_out);
                    }
                    0
                }

                other => {
                    eprintln!(
                        "error: unknown benchmark '{other}'. Supported: humaneval, swe-bench-live"
                    );
                    1
                }
            }
        }

        EvalCmd::Compare {
            baseline,
            current,
            output,
            format,
        } => {
            let baseline_results = match load_eval_results(&baseline) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
            let current_results = match load_eval_results(&current) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };

            let calc = MetricsCalculator::default();
            let baseline_report = calc.compute_all_named(&baseline_results, "Baseline");
            let current_report = calc.compute_all_named(&current_results, "Current");

            let signals = RegressionDetector::detect(&current_report, &baseline_report);

            let content = match format.as_str() {
                "html" => ReportGenerator::to_html(&current_report, Some(&baseline_report)),
                "json" => ReportGenerator::to_json(&current_report),
                _ => ReportGenerator::to_markdown(&current_report, Some(&baseline_report)),
            };

            if let Err(e) = std::fs::write(&output, &content) {
                eprintln!("error: failed to write report: {e}");
                return 1;
            }

            let has_regression = signals.iter().any(|s| s.is_regression);
            if has_regression {
                println!("Warning: regressions detected. See {}.", output.display());
            } else {
                println!("No regressions detected. Report: {}", output.display());
            }
            0
        }

        EvalCmd::CheckRegression {
            baseline,
            current,
            threshold,
        } => {
            let baseline_results = match load_eval_results(&baseline) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
            let current_results = match load_eval_results(&current) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };

            let calc = MetricsCalculator::default();
            let baseline_report = calc.compute_all_named(&baseline_results, "Baseline");
            let current_report = calc.compute_all_named(&current_results, "Current");

            let signals = RegressionDetector::detect(&current_report, &baseline_report);
            let regressions: Vec<_> = signals.iter().filter(|s| s.is_regression).collect();

            for r in &regressions {
                eprintln!(
                    "REGRESSION: {}  {:.2} -> {:.2}  ({:+.2})",
                    r.metric, r.before, r.after, r.delta
                );
            }

            let critical = regressions
                .iter()
                .filter(|r| {
                    use code_agent_eval::RegressionSeverity;
                    r.severity >= RegressionSeverity::Warning
                })
                .count();

            if critical > 0 {
                eprintln!(
                    "error: {} regression(s) detected (threshold: {}%). Failing.",
                    critical, threshold,
                );
                1
            } else {
                println!("All metrics within threshold — no regressions.");
                0
            }
        }
    }
}

fn print_benchmark_summary(report: &code_agent_eval::BenchmarkReport) {
    println!("Benchmark: {}", report.benchmark);
    println!("  Tasks:  {} total, {} passed, {} failed, {} errored",
        report.total_tasks, report.passed, report.failed, report.errored);
    println!("  pass@1: {:.2}%", report.pass_at_1 * 100.0);
    println!("  resolve_rate: {:.2}%", report.resolve_rate * 100.0);
    println!("  avg tokens/task: {:.0}", report.avg_tokens_per_task);
    println!("  avg turns/task:  {:.1}", report.avg_turns_per_task);
}

// ── eval helpers ──────────────────────────────────────────────────

fn save_eval_results(
    results: &[code_agent_eval::EvalResult],
    report: &code_agent_eval::BenchmarkReport,
    output_path: &PathBuf,
) {
    use serde::Serialize;
    #[derive(Serialize)]
    struct ResultsFile {
        benchmark: String,
        total_tasks: usize,
        passed: usize,
        failed: usize,
        errored: usize,
        pass_at_1: f64,
        resolve_rate: f64,
        avg_tokens_per_task: f64,
        avg_turns_per_task: f64,
        total_cost_estimate: f64,
        per_task: Vec<code_agent_eval::TaskResult>,
        raw_results: Vec<code_agent_eval::EvalResult>,
    }

    let file = ResultsFile {
        benchmark: report.benchmark.clone(),
        total_tasks: report.total_tasks,
        passed: report.passed,
        failed: report.failed,
        errored: report.errored,
        pass_at_1: report.pass_at_1,
        resolve_rate: report.resolve_rate,
        avg_tokens_per_task: report.avg_tokens_per_task,
        avg_turns_per_task: report.avg_turns_per_task,
        total_cost_estimate: report.total_cost_estimate,
        per_task: report.per_task.clone(),
        raw_results: results.to_vec(),
    };

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    match serde_json::to_string_pretty(&file) {
        Ok(json) => {
            if let Err(e) = std::fs::write(output_path, &json) {
                eprintln!("error: failed to write results: {e}");
            } else {
                println!("Results saved to {}", output_path.display());
            }
        }
        Err(e) => {
            eprintln!("error: failed to serialize results: {e}");
        }
    }
}

fn load_eval_results(path: &PathBuf) -> Result<Vec<code_agent_eval::EvalResult>, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;

    // Try ResultsFile wrapper
    if let Ok(wrapper) = serde_json::from_str::<serde_json::Value>(&content) {
        if let Some(raw) = wrapper.get("raw_results") {
            return serde_json::from_value(raw.clone())
                .map_err(|e| format!("Failed to parse raw_results: {e}"));
        }
        // Fallback: reconstruct from per_task
        if let Some(per_task) = wrapper.get("per_task") {
            let task_results: Vec<code_agent_eval::TaskResult> =
                serde_json::from_value(per_task.clone()).map_err(|e| e.to_string())?;
            return Ok(task_results
                .into_iter()
                .map(|tr| code_agent_eval::EvalResult {
                    task_id: tr.task_id,
                    status: tr.status,
                    score: tr.score,
                    logs: Vec::new(),
                    patch: None,
                    metrics: code_agent_eval::EvalMetrics {
                        duration_ms: tr.duration_ms,
                        tokens_used: tr.tokens_used,
                        turns_taken: tr.turns_taken,
                    },
                })
                .collect());
        }
    }

    // Try raw array
    serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse eval results: {e}"))
}

// ---------------------------------------------------------------------------
// cmd_doctor
// ---------------------------------------------------------------------------

fn cmd_doctor(config: Config) -> i32 {
    println!("=== Code Agent Doctor ===\n");

    let mut issues = 0u32;

    // 1. Configuration
    println!("[config]");
    let global_path = ConfigManager::global_config_path();
    match &global_path {
        Some(p) if p.exists() => println!("  global config: {}", p.display()),
        Some(p) => {
            println!("  global config: {} (not found)", p.display());
            issues += 1;
        }
        None => {
            println!("  global config: (no config directory)");
            issues += 1;
        }
    }

    let cwd = config.work_dir.clone();
    let project_path = ConfigManager::project_config_path(&cwd);
    if project_path.exists() {
        println!("  project config: {}", project_path.display());
    } else {
        println!("  project config: {} (not found)", project_path.display());
    }

    println!("  resolved model: {}", config.model);
    println!("  timeout: {}s", config.timeout_secs);

    // 2. API Key
    println!("\n[api]");
    let api_key = if !config.api_key.is_empty() {
        let masked = if config.api_key.len() > 8 {
            format!("{}...{}", &config.api_key[..4], &config.api_key[config.api_key.len()-4..])
        } else {
            "****".to_string()
        };
        println!("  API key from config: {masked}");
        true
    } else if let Ok(env_key) = std::env::var("LLM_API_KEY") {
        let masked = if env_key.len() > 8 {
            format!("{}...{}", &env_key[..4], &env_key[env_key.len()-4..])
        } else {
            "****".to_string()
        };
        println!("  API key from env: {masked}");
        true
    } else if let Ok(env_key) = std::env::var("OPENAI_API_KEY") {
        let masked = if env_key.len() > 8 {
            format!("{}...{}", &env_key[..4], &env_key[env_key.len()-4..])
        } else {
            "****".to_string()
        };
        println!("  API key from env (OPENAI_API_KEY): {masked}");
        true
    } else {
        println!("  WARNING: No API key found (set LLM_API_KEY or configure api_key)");
        issues += 1;
        false
    };
    if !api_key {
        println!("  hint: run `code-agent config set api_key <your-key>`");
    }

    // 3. Rust toolchain
    println!("\n[rust]");
    let rustc_output = std::process::Command::new("rustc")
        .arg("--version")
        .output();
    match rustc_output {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout);
            println!("  rustc: {}", version.trim());
        }
        _ => {
            println!("  WARNING: rustc not found");
            issues += 1;
        }
    }

    let cargo_output = std::process::Command::new("cargo")
        .arg("--version")
        .output();
    match cargo_output {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout);
            println!("  cargo: {}", version.trim());
        }
        _ => {
            println!("  WARNING: cargo not found");
            issues += 1;
        }
    }

    // 4. Tools
    println!("\n[tools]");
    let tools = ["git", "docker", "python"];
    for tool in &tools {
        let found = std::process::Command::new(tool)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        let status = if found { "ok" } else { "NOT FOUND" };
        println!("  {tool}: {status}");
        if !found {
            issues += 1;
        }
    }

    // 5. Network
    println!("\n[network]");
    println!("  api_base_url: {}", config.api_base_url);

    // 6. Summary
    println!("\n=== Summary ===");
    if issues == 0 {
        println!("All checks passed.");
        0
    } else {
        println!("{issues} issue(s) found.");
        1
    }
}
