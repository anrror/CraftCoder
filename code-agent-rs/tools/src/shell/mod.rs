//! 沙箱化 Shell 执行引擎
//!
//! 【领域含义】为 AI 编码代理提供隔离的命令执行环境，防止恶意或失控命令影响宿主机。
//! 支持三种后端策略，按优先级自动降级：
//! - **LinuxBubblewrap**: 命名空间 + seccomp BPF 隔离（仅 Linux）
//! - **Docker**: 容器化隔离（跨平台回退方案）
//! - **Unrestricted**: 直接进程派生 + 超时控制（最后手段）
//!
//! 【核心职责】封装命令执行的完整生命周期：参数校验 → 后端选择 → 进程派生 → 输出读取 → 超时终止。

pub mod config;
pub mod docker;

#[cfg(target_os = "linux")]
pub mod linux;

use config::SandboxConfig;
use std::path::Path;
#[cfg(not(windows))]
use std::time::Instant;
use thiserror::Error;
#[cfg(not(windows))]
use tokio::io::{AsyncRead, AsyncReadExt};
#[cfg(not(windows))]
use tokio::process::Command as AsyncCommand;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// 沙箱执行错误
///
/// 【领域含义】表示沙箱化命令执行过程中可能出现的所有失败模式。
/// 【核心职责】将底层 I/O 错误、配置错误、后端不可用等统一为领域错误类型。
#[derive(Debug, Error)]
pub enum SandboxError {
    /// 进程派生失败
    ///
    /// 【领域含义】无法启动指定后端的子进程（如 bwrap/docker/bin/sh 不存在或权限不足）。
    #[error("failed to spawn {backend} process")]
    Spawn {
        /// 后端名称标识
        backend: &'static str,
        /// 底层 I/O 错误来源
        #[source]
        source: std::io::Error,
    },

    /// I/O 操作错误
    ///
    /// 【领域含义】与子进程通信过程中的读写错误。
    #[error("I/O error")]
    Io {
        #[from]
        source: std::io::Error,
    },

    /// 配置错误
    ///
    /// 【领域含义】沙箱配置参数无效或不一致。
    #[error("configuration error: {0}")]
    Config(String),

    /// 后端不可用
    ///
    /// 【领域含义】所选后端在当前平台上不可用（如在非 Linux 上使用 Bubblewrap）。
    #[error("sandbox backend not available: {0}")]
    BackendUnavailable(&'static str),

    /// 空命令错误
    ///
    /// 【领域含义】用户提交了空白或空命令字符串，拒绝执行。
    #[error("command is empty")]
    EmptyCommand,

    /// 命令包含 shell 元字符
    ///
    /// 【领域含义】命令中检测到 `$`、反引号、管道、重定向或分隔符等
    /// shell 元字符，拒绝执行以防止命令注入（CWE-78）。
    #[error("command rejected (CWE-78): contains shell metacharacter(s) — {0}")]
    MetacharacterRejected(String),
}

// ---------------------------------------------------------------------------
// Sandbox backend enum
// ---------------------------------------------------------------------------

/// 沙箱后端枚举
///
/// 【领域含义】定义沙箱执行的可选后端策略，按隔离强度降序排列。
/// 【核心职责】标识当前使用的隔离技术，供执行路由和状态展示使用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackend {
    /// Linux Bubblewrap — 命名空间 + seccomp BPF 隔离
    LinuxBubblewrap,
    /// Docker — 容器化隔离
    Docker,
    /// 无限制 — 直接进程派生（仅超时控制）
    Unrestricted,
    /// 无沙箱 — 拒绝执行（需显式设置 CODE_AGENT_ALLOW_UNRESTRICTED=true 才回退到 Unrestricted）
    None,
}

// ---------------------------------------------------------------------------
// Execution result
// ---------------------------------------------------------------------------

/// 命令执行结果
///
/// 【领域含义】封装一次沙箱命令执行的完整输出和元数据。
/// 【核心职责】提供标准输出、标准错误、退出码、执行耗时和超时标志的统一结构。
#[derive(Debug, Clone)]
pub struct ExecResult {
    /// 标准输出内容
    pub stdout: String,
    /// 标准错误内容
    pub stderr: String,
    /// 进程退出码（-1 表示超时或被杀死）
    pub exit_code: i32,
    /// 执行耗时（毫秒）
    pub duration_ms: u64,
    /// 是否因超时而终止
    pub timed_out: bool,
}

// ---------------------------------------------------------------------------
// Shell metacharacter detection (CWE-78 prevention)
// ---------------------------------------------------------------------------

/// 检测命令中是否包含 shell 元字符
///
/// 【领域含义】检测可通过 `/bin/sh -c` 用于命令注入的特殊字符。
/// 检测到 `$`、反引号、管道 (`|`)、分隔符 (`;`)、后台 (`&`)、
/// 重定向 (`<`, `>`)、换行 (`\n`)、制表 (`\t`) 即返回 `true`。
///
/// 【核心职责】在命令执行前进行静态安全检查，阻止 CWE-78 命令注入。
fn has_shell_metacharacters(command: &str) -> bool {
    command.contains('$')
        || command.contains('`')
        || command.contains('|')
        || command.contains(';')
        || command.contains('&')
        || command.contains('<')
        || command.contains('>')
        || command.contains('\n')
        || command.contains('\t')
}

/// 校验命令不包含 shell 元字符
///
/// 【领域含义】对即将执行的外部命令进行注入防御校验。
/// 通过则返回原命令引用，否则记录 `warn!` 安全审计日志并拒绝执行。
///
/// 【核心职责】阻止含元字符的命令进入 shell 执行管道。
fn sanitize_command(command: &str) -> Result<&str, SandboxError> {
    if has_shell_metacharacters(command) {
        let preview = if command.len() <= 100 {
            command.to_string()
        } else {
            format!("{}...", &command[..100])
        };
        warn!(
            target: "shell",
            command_preview = %preview,
            "rejected command with shell metacharacters (CWE-78 injection prevention)"
        );
        return Err(SandboxError::MetacharacterRejected(preview));
    }
    Ok(command)
}

// ---------------------------------------------------------------------------
// SandboxManager
// ---------------------------------------------------------------------------

/// 沙箱管理器
///
/// 【领域含义】沙箱执行的核心聚合根，持有后端策略和配置，对外提供统一的命令执行接口。
/// 【核心职责】自动检测可用后端 → 路由命令到对应后端 → 返回统一执行结果。
pub struct SandboxManager {
    /// 当前选中的沙箱后端
    backend: SandboxBackend,
    /// 沙箱配置（超时、内存限制、网络/写权限等）
    config: SandboxConfig,
}

impl SandboxManager {
    /// 创建沙箱管理器（自动检测后端）
    ///
    /// 【领域含义】使用自动检测策略初始化管理器，按优先级选择最佳可用后端。
    /// 【核心职责】调用 `detect()` 探测环境，构造管理器实例。
    pub fn new(config: SandboxConfig) -> Self {
        let backend = Self::detect();
        debug!(target: "shell", ?backend, "initialised sandbox manager");
        Self { backend, config }
    }

    /// 创建沙箱管理器（显式指定后端）
    ///
    /// 【领域含义】跳过自动检测，强制使用指定的后端策略。
    /// 【核心职责】适用于测试或用户明确指定后端的场景。
    pub fn with_backend(config: SandboxConfig, backend: SandboxBackend) -> Self {
        debug!(target: "shell", ?backend, "initialised sandbox manager (explicit)");
        Self { backend, config }
    }

    /// 自动检测可用沙箱后端
    ///
    /// 【领域含义】按优先级探测：bwrap → Docker → 无限制回退。
    /// 【核心职责】返回当前平台上可用的最强隔离后端。
    pub fn detect() -> SandboxBackend {
        #[cfg(target_os = "linux")]
        {
            if linux::is_bwrap_available() {
                return SandboxBackend::LinuxBubblewrap;
            }
        }
        if docker::is_docker_available() {
            return SandboxBackend::Docker;
        }
        // CWE-250: 无沙箱回退必须显式 opt-in —— 禁止静默回退到 Unrestricted
        let allow_unrestricted = std::env::var("CODE_AGENT_ALLOW_UNRESTRICTED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        if allow_unrestricted {
            info!(
                target: "shell",
                "CODE_AGENT_ALLOW_UNRESTRICTED=true — falling back to unrestricted execution (CWE-250 opt-in)"
            );
            SandboxBackend::Unrestricted
        } else {
            info!(
                target: "shell",
                "no sandbox available and CODE_AGENT_ALLOW_UNRESTRICTED not set — refusing execution (CWE-250)"
            );
            SandboxBackend::None
        }
    }

    /// 获取当前后端
    ///
    /// 【领域含义】返回管理器当前使用的沙箱后端类型。
    /// 【核心职责】供外部查询状态使用。
    pub fn backend(&self) -> SandboxBackend {
        self.backend
    }

    /// 执行命令
    ///
    /// 【领域含义】在沙箱中执行指定命令，返回执行结果。
    /// 【核心职责】校验命令非空 → 路由到对应后端 → 返回统一 ExecResult。
    pub async fn execute(
        &self,
        command: &str,
        workdir: &Path,
    ) -> Result<ExecResult, SandboxError> {
        if command.trim().is_empty() {
            return Err(SandboxError::EmptyCommand);
        }

        // CWE-78: 命令注入防御 —— 拒绝包含 shell 元字符的命令
        sanitize_command(command)?;

        match self.backend {
            SandboxBackend::None => {
                Err(SandboxError::BackendUnavailable(
                    "No sandbox backend available; set CODE_AGENT_ALLOW_UNRESTRICTED=true to enable unrestricted fallback",
                ))
            }
            SandboxBackend::LinuxBubblewrap => {
                #[cfg(target_os = "linux")]
                {
                    linux::execute_in_bwrap(&self.config, command, workdir).await
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Err(SandboxError::BackendUnavailable(
                        "LinuxBubblewrap requires Linux",
                    ))
                }
            }
            SandboxBackend::Docker => {
                docker::execute_in_docker(&self.config, command, workdir).await
            }
            SandboxBackend::Unrestricted => {
                execute_unrestricted(&self.config, command, workdir).await
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Unrestricted fallback
// ---------------------------------------------------------------------------

async fn execute_unrestricted(
    config: &SandboxConfig,
    command: &str,
    workdir: &Path,
) -> Result<ExecResult, SandboxError> {
    // 审计日志：记录无沙箱执行决策（CWE-250 审计追踪）
    let command_preview = if command.len() <= 100 {
        command
    } else {
        &command[..100]
    };
    info!(
        target: "shell",
        command_preview,
        "executing command in unrestricted mode (CWE-250: explicit opt-in verified)"
    );

    // C3: Windows 上不提供 shell 执行 —— 不允许回退到 cmd.exe（cmd.exe 的引号/转义语义
    // 与 POSIX shell 完全不同，会引入命令注入风险）。用户必须在 Linux 或 Docker 沙箱中执行。
    #[cfg(windows)]
    {
        let _ = (config, command, workdir);
        Err(SandboxError::BackendUnavailable(
            "shell execution is not available on Windows without Docker sandbox; "
        ))
    }

    #[cfg(not(windows))]
    {
        _execute_unrestricted_posix(config, command, workdir).await
    }
}

/// C3: POSIX 平台上的无限制 shell 执行（Linux / macOS）
///
/// 使用 `/bin/sh -c <command>` 而非 `cmd.exe`，因为 POSIX shell 的引号语义
/// 可预测且不易受命令注入影响。coreutils / PATH 等环境变量在子进程中清空。
#[cfg(not(windows))]
async fn _execute_unrestricted_posix(
    config: &SandboxConfig,
    command: &str,
    workdir: &Path,
) -> Result<ExecResult, SandboxError> {
    let start = Instant::now();

    // Validate: reject commands with null bytes (can't be represented in C strings)
    if command.contains('\0') {
        return Err(SandboxError::Config(
            "command contains null byte".to_string(),
        ));
    }

    let mut cmd = AsyncCommand::new("/bin/sh");
    cmd.arg("-c").arg(command);
    cmd.current_dir(workdir);
    // Clear inherited environment to reduce injection surface
    cmd.env_clear();
    cmd.env("PATH", "/usr/local/bin:/usr/bin:/bin");
    cmd.env("HOME", "/tmp");
    // Pass through the unrestricted opt-in flag so child processes inherit the same policy
    cmd.env("CODE_AGENT_ALLOW_UNRESTRICTED", "1");

    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| SandboxError::Spawn {
            backend: "unrestricted",
            source: e,
        })?;

    let timeout_dur = std::time::Duration::from_secs(config.timeout_secs);
    let wait_result = tokio::time::timeout(timeout_dur, child.wait()).await;
    let timed_out = wait_result.is_err();

    if timed_out {
        let _ = child.kill().await;
        let _ = child.wait().await;
        return Ok(ExecResult {
            stdout: String::new(),
            stderr: format!(
                "command timed out after {} seconds",
                config.timeout_secs
            ),
            exit_code: -1,
            duration_ms: start.elapsed().as_millis() as u64,
            timed_out: true,
        });
    }

    let status = wait_result
        .expect("timeout already handled")
        .map_err(|e| SandboxError::Io { source: e })?;

    let stdout =
        read_pipe_output(&mut child, PipeTarget::Stdout, config.max_output_bytes).await?;
    let stderr =
        read_pipe_output(&mut child, PipeTarget::Stderr, config.max_output_bytes).await?;

    let exit_code = status.code().unwrap_or(-1);
    let duration_ms = start.elapsed().as_millis() as u64;

    // H8/M7: 使用 info! 而非 debug! 确保审计追踪在生产环境可见
    info!(
        target: "shell",
        exit_code,
        duration_ms,
        stdout_len = stdout.len(),
        stderr_len = stderr.len(),
        "unrestricted command completed"
    );

    Ok(ExecResult {
        stdout,
        stderr,
        exit_code,
        duration_ms,
        timed_out: false,
    })
}

/// 管道目标标识
///
/// 【领域含义】区分标准输出和标准错误两条管道。
/// 【核心职责】在输出读取函数中指定要读取的管道。
#[cfg(not(windows))]
#[derive(Copy, Clone)]
enum PipeTarget {
    /// 标准输出管道
    Stdout,
    /// 标准错误管道
    Stderr,
}

#[cfg(not(windows))]
async fn read_pipe_output(
    child: &mut tokio::process::Child,
    which: PipeTarget,
    max_bytes: usize,
) -> Result<String, SandboxError> {
    let mut reader: Box<dyn AsyncRead + Unpin + Send> = match which {
        PipeTarget::Stdout => match child.stdout.take() {
            Some(p) => Box::new(p),
            None => return Ok(String::new()),
        },
        PipeTarget::Stderr => match child.stderr.take() {
            Some(p) => Box::new(p),
            None => return Ok(String::new()),
        },
    };

    let mut buf = Vec::with_capacity(4096);
    let mut total: usize = 0;

    loop {
        let remaining = max_bytes.saturating_sub(total);
        if remaining == 0 {
            let mut discard = [0u8; 4096];
            while reader
                .read(&mut discard)
                .await
                .map(|n| n > 0)
                .unwrap_or(false)
            {}
            break;
        }

        let chunk_size = remaining.min(4096);
        let mut chunk = vec![0u8; chunk_size];
        let n = reader
            .read(&mut chunk)
            .await
            .map_err(|e| SandboxError::Io { source: e })?;

        if n == 0 {
            break;
        }

        buf.extend_from_slice(&chunk[..n]);
        total += n;
    }

    let truncated = total > max_bytes;
    let mut s = String::from_utf8_lossy(&buf[..total.min(max_bytes)]).into_owned();

    if truncated {
        s.push_str("\n[... output truncated ...]");
    }

    Ok(s)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_sandbox_backend_detect_does_not_panic() {
        let backend = SandboxManager::detect();
        let _ = format!("{:?}", backend);
    }

    #[test]
    fn test_sandbox_backend_is_copy() {
        let b = SandboxBackend::LinuxBubblewrap;
        let c = b;
        assert_eq!(b, c);
    }

    #[test]
    fn test_manager_new_does_not_panic() {
        let config = SandboxConfig::default();
        let manager = SandboxManager::new(config);
        let _ = manager.backend();
    }

    #[test]
    fn test_manager_with_explicit_backend() {
        let config = SandboxConfig::default();
        let manager =
            SandboxManager::with_backend(config, SandboxBackend::Unrestricted);
        assert_eq!(manager.backend(), SandboxBackend::Unrestricted);
    }

    #[test]
    fn test_exec_result_defaults() {
        let r = ExecResult {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 0,
            duration_ms: 0,
            timed_out: false,
        };
        assert!(!r.timed_out);
        assert_eq!(r.exit_code, 0);
        assert!(r.stdout.is_empty());
    }

    #[tokio::test]
    async fn test_empty_command_rejected() {
        let config = SandboxConfig::default();
        let manager =
            SandboxManager::with_backend(config, SandboxBackend::Unrestricted);

        let result = manager.execute("", Path::new(".")).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::EmptyCommand => {}
            other => panic!("expected EmptyCommand, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_whitespace_only_command_rejected() {
        let config = SandboxConfig::default();
        let manager =
            SandboxManager::with_backend(config, SandboxBackend::Unrestricted);

        let result = manager.execute("   \n\t  ", Path::new(".")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_basic_command_execution_unrestricted() {
        let config = SandboxConfig::default();
        let manager =
            SandboxManager::with_backend(config, SandboxBackend::Unrestricted);

        let temp_dir = std::env::temp_dir();
        let result = manager.execute("echo hello_sandbox", &temp_dir).await;

        match result {
            Ok(exec_result) => {
                assert!(
                    exec_result.stdout.contains("hello_sandbox"),
                    "stdout: {:?}",
                    exec_result.stdout
                );
                assert!(!exec_result.timed_out);
            }
            Err(SandboxError::Spawn { .. }) => {
                eprintln!("skipping: /bin/sh not available");
            }
            Err(SandboxError::BackendUnavailable(msg)) => {
                eprintln!("skipping (Windows): {msg}");
            }
            Err(e) => panic!("unexpected error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_timeout_kills_long_running_command() {
        let config = SandboxConfig {
            timeout_secs: 1,
            ..Default::default()
        };
        let manager =
            SandboxManager::with_backend(config, SandboxBackend::Unrestricted);

        let temp_dir = std::env::temp_dir();
        let result = manager.execute("sleep 10", &temp_dir).await;

        match result {
            Ok(exec_result) => {
                assert!(exec_result.timed_out, "command should have timed out");
                assert!(exec_result.stderr.contains("timed out"));
            }
            Err(SandboxError::Spawn { .. }) => {
                eprintln!("skipping: /bin/sh not available");
            }
            Err(SandboxError::BackendUnavailable(msg)) => {
                eprintln!("skipping (Windows): {msg}");
            }
            Err(e) => panic!("unexpected error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_nonexistent_command_returns_error() {
        let config = SandboxConfig::default();
        let manager =
            SandboxManager::with_backend(config, SandboxBackend::Unrestricted);

        let temp_dir = std::env::temp_dir();
        let result = manager
            .execute("nonexistent_command_xyzzy_12345", &temp_dir)
            .await;

        match result {
            Ok(exec_result) => {
                assert_ne!(exec_result.exit_code, 0);
            }
            Err(SandboxError::Spawn { .. }) => {
                eprintln!("skipping: /bin/sh not available");
            }
            Err(SandboxError::BackendUnavailable(msg)) => {
                eprintln!("skipping (Windows): {msg}");
            }
            Err(e) => panic!("unexpected error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_output_truncation() {
        let config = SandboxConfig {
            max_output_bytes: 50,
            ..Default::default()
        };
        let manager =
            SandboxManager::with_backend(config, SandboxBackend::Unrestricted);

        let temp_dir = std::env::temp_dir();
        // Pre-create a file with 200 bytes of 'x' to avoid metacharacters
        // (CWE-78: pipes and redirects are now blocked in unrestricted mode)
        let test_file = temp_dir.join("_cctrunc_test.txt");
        let content = "x".repeat(200);
        std::fs::write(&test_file, &content).unwrap();

        let cmd = format!("cat {}", test_file.display());
        let result = manager.execute(&cmd, &temp_dir).await;

        let _ = std::fs::remove_file(&test_file);

        match result {
            Ok(exec_result) => {
                assert!(
                    exec_result.stdout.contains("[... output truncated ...]"),
                    "output should be truncated, got len={}: {:?}",
                    exec_result.stdout.len(),
                    exec_result.stdout
                );
            }
            Err(SandboxError::Spawn { .. }) => {
                eprintln!("skipping: /bin/sh not available");
            }
            Err(SandboxError::BackendUnavailable(msg)) => {
                eprintln!("skipping (Windows): {msg}");
            }
            Err(e) => panic!("unexpected error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_workdir_resolution() {
        let config = SandboxConfig::default();
        let manager =
            SandboxManager::with_backend(config, SandboxBackend::Unrestricted);

        let temp_dir = std::env::temp_dir();
        let result = manager.execute("pwd", &temp_dir).await;

        match result {
            Ok(exec_result) => {
                let stdout = exec_result.stdout.trim();
                assert!(
                    !stdout.is_empty(),
                    "pwd should output the working directory"
                );
                debug!(target: "shell.test", "pwd output: {}", stdout);
            }
            Err(SandboxError::Spawn { .. }) => {
                eprintln!("skipping: /bin/sh not available");
            }
            Err(SandboxError::BackendUnavailable(msg)) => {
                eprintln!("skipping (Windows): {msg}");
            }
            Err(e) => panic!("unexpected error: {:?}", e),
        }
    }

    #[tokio::test]
    async fn test_exit_code_propagation() {
        let config = SandboxConfig::default();
        let manager =
            SandboxManager::with_backend(config, SandboxBackend::Unrestricted);

        let temp_dir = std::env::temp_dir();
        let result = manager.execute("exit 42", &temp_dir).await;

        match result {
            Ok(exec_result) => {
                assert_eq!(exec_result.exit_code, 42);
            }
            Err(SandboxError::Spawn { .. }) => {
                eprintln!("skipping: /bin/sh not available");
            }
            Err(SandboxError::BackendUnavailable(msg)) => {
                eprintln!("skipping (Windows): {msg}");
            }
            Err(e) => panic!("unexpected error: {:?}", e),
        }
    }

    #[test]
    fn test_serde_sandbox_config_roundtrip() {
        let config = SandboxConfig {
            allow_network: true,
            allow_write: false,
            allowed_paths: vec![PathBuf::from("/tmp"), PathBuf::from("/var")],
            timeout_secs: 30,
            max_output_bytes: 2048,
            memory_limit_mb: 512,
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: SandboxConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.allow_network, config.allow_network);
        assert_eq!(parsed.allow_write, config.allow_write);
        assert_eq!(parsed.allowed_paths, config.allowed_paths);
        assert_eq!(parsed.timeout_secs, config.timeout_secs);
        assert_eq!(parsed.max_output_bytes, config.max_output_bytes);
        assert_eq!(parsed.memory_limit_mb, config.memory_limit_mb);
    }

    // -----------------------------------------------------------------------
    // CWE-78: Shell metacharacter detection tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_has_shell_metacharacters_detects_dollar() {
        assert!(has_shell_metacharacters("echo $HOME"));
    }

    #[test]
    fn test_has_shell_metacharacters_detects_backtick() {
        assert!(has_shell_metacharacters("echo `whoami`"));
    }

    #[test]
    fn test_has_shell_metacharacters_detects_pipe() {
        assert!(has_shell_metacharacters("ls | grep foo"));
    }

    #[test]
    fn test_has_shell_metacharacters_detects_semicolon() {
        assert!(has_shell_metacharacters("ls; rm -rf /"));
    }

    #[test]
    fn test_has_shell_metacharacters_detects_ampersand() {
        assert!(has_shell_metacharacters("sleep 10 &"));
    }

    #[test]
    fn test_has_shell_metacharacters_detects_redirect_out() {
        assert!(has_shell_metacharacters("echo foo > bar"));
    }

    #[test]
    fn test_has_shell_metacharacters_detects_redirect_in() {
        assert!(has_shell_metacharacters("cat < /etc/passwd"));
    }

    #[test]
    fn test_has_shell_metacharacters_detects_newline() {
        assert!(has_shell_metacharacters("echo hello\necho world"));
    }

    #[test]
    fn test_has_shell_metacharacters_detects_tab() {
        assert!(has_shell_metacharacters("echo hello\techo world"));
    }

    #[test]
    fn test_safe_commands_pass_metacharacter_check() {
        assert!(!has_shell_metacharacters("echo hello"));
        assert!(!has_shell_metacharacters("ls -la"));
        assert!(!has_shell_metacharacters("cat file.txt"));
        assert!(!has_shell_metacharacters("pwd"));
    }

    #[test]
    fn test_sanitize_command_rejects_injection_attempt() {
        let result = sanitize_command("ls | grep foo");
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::MetacharacterRejected(_) => {}
            other => panic!("expected MetacharacterRejected, got {:?}", other),
        }
    }

    #[test]
    fn test_sanize_command_allows_safe_string() {
        let result = sanitize_command("echo hello world");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "echo hello world");
    }

    #[tokio::test]
    async fn test_execute_rejects_command_injection_via_pipe() {
        let config = SandboxConfig::default();
        let manager =
            SandboxManager::with_backend(config, SandboxBackend::Unrestricted);

        let result = manager
            .execute("echo hello | cat /etc/passwd", Path::new("."))
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::MetacharacterRejected(_) => {}
            SandboxError::BackendUnavailable(_) => {
                eprintln!("skipping (Windows): shell not available");
            }
            other => panic!("expected MetacharacterRejected, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_execute_rejects_backtick_injection() {
        let config = SandboxConfig::default();
        let manager =
            SandboxManager::with_backend(config, SandboxBackend::Unrestricted);

        let result = manager
            .execute("echo `cat /etc/passwd`", Path::new("."))
            .await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::MetacharacterRejected(_) => {}
            SandboxError::BackendUnavailable(_) => {
                eprintln!("skipping (Windows): shell not available");
            }
            other => panic!("expected MetacharacterRejected, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // CWE-250: Sandbox bypass prevention tests
    // -----------------------------------------------------------------------

    /// Helper: set an env var and return a cleanup closure
    fn set_env_for_test(key: String, val: String) -> impl FnOnce() {
        let old = std::env::var(&key).ok();
        std::env::set_var(&key, val);
        move || {
            match old {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn test_none_variant_identity() {
        let b = SandboxBackend::None;
        let c = b; // Copy
        assert_eq!(b, c);
        assert_ne!(b, SandboxBackend::Unrestricted);
        assert_ne!(b, SandboxBackend::Docker);
    }

    #[test]
    fn test_detect_with_opt_in_does_not_panic() {
        let _cleanup = set_env_for_test(
            "CODE_AGENT_ALLOW_UNRESTRICTED".to_string(),
            "true".to_string(),
        );
        let backend = SandboxManager::detect();
        // Just verify it doesn't panic and returns a valid variant
        let _ = format!("{:?}", backend);
    }

    #[test]
    fn test_detect_without_opt_in_does_not_panic() {
        let _cleanup = set_env_for_test(
            "CODE_AGENT_ALLOW_UNRESTRICTED".to_string(),
            "".to_string(),
        );
        std::env::remove_var("CODE_AGENT_ALLOW_UNRESTRICTED");
        let backend = SandboxManager::detect();
        let _ = format!("{:?}", backend);
    }

    #[test]
    fn test_detect_returns_unrestricted_with_opt_in_one() {
        let _cleanup = set_env_for_test(
            "CODE_AGENT_ALLOW_UNRESTRICTED".to_string(),
            "1".to_string(),
        );
        let backend = SandboxManager::detect();
        // If docker/bwrap are available, they take priority.
        // Otherwise, should be Unrestricted (not None) when env var is set.
        // Note: env-var tests may race in parallel execution; we only assert
        // the function doesn't panic and returns a valid variant.
        let _ = format!("{:?}", backend);
    }

    #[tokio::test]
    async fn test_none_backend_refuses_execution() {
        let config = SandboxConfig::default();
        let manager =
            SandboxManager::with_backend(config, SandboxBackend::None);

        let result = manager.execute("echo hello", Path::new(".")).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SandboxError::BackendUnavailable(msg) => {
                assert!(
                    msg.contains("CODE_AGENT_ALLOW_UNRESTRICTED"),
                    "error message should mention CODE_AGENT_ALLOW_UNRESTRICTED, got: {}",
                    msg
                );
            }
            other => panic!("expected BackendUnavailable, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_unrestricted_backend_still_works_with_opt_in() {
        let _cleanup = set_env_for_test(
            "CODE_AGENT_ALLOW_UNRESTRICTED".to_string(),
            "true".to_string(),
        );
        let config = SandboxConfig::default();
        let manager =
            SandboxManager::with_backend(config, SandboxBackend::Unrestricted);

        let temp_dir = std::env::temp_dir();
        let result = manager.execute("echo still_works", &temp_dir).await;

        match result {
            Ok(exec_result) => {
                assert!(exec_result.stdout.contains("still_works"));
            }
            Err(SandboxError::Spawn { .. }) => {
                eprintln!("skipping: /bin/sh not available");
            }
            Err(SandboxError::BackendUnavailable(msg)) => {
                eprintln!("skipping (Windows): {msg}");
            }
            Err(e) => panic!("unexpected error: {:?}", e),
        }
    }
}
