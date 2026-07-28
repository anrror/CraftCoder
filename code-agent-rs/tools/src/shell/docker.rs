//! Docker 沙箱后端
//!
//! 【领域含义】基于 Docker 容器的沙箱执行方案，作为非 Linux 平台或 bwrap 不可用时的回退。
//! 【核心职责】通过 `docker run` 创建一次性容器执行命令，支持网络/写入权限控制和资源限制。

use crate::shell::config::SandboxConfig;
use crate::shell::{ExecResult, SandboxError};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command as AsyncCommand;
use tokio::time::timeout;
use tracing::debug;

const DEFAULT_IMAGE: &str = "alpine:latest";

/// 在 Docker 容器中执行命令
///
/// 【领域含义】通过 `docker run --rm` 创建一次性容器，在隔离环境中执行命令。
/// 【核心职责】配置容器参数（网络、内存、挂载、只读根文件系统）→ 启动容器 → 等待完成 → 读取输出。
pub async fn execute_in_docker(
    config: &SandboxConfig,
    command: &str,
    workdir: &Path,
) -> Result<ExecResult, SandboxError> {
    let start = Instant::now();
    let workdir = canonicalize_or_identity(workdir);

    let mut cmd = AsyncCommand::new("docker");
    cmd.arg("run").arg("--rm").arg("-i");

    if !config.allow_network {
        cmd.arg("--network").arg("none");
    }

    cmd.arg("--memory")
        .arg(format!("{}m", config.memory_limit_mb));
    cmd.arg("--memory-swap")
        .arg(format!("{}m", config.memory_limit_mb));
    cmd.arg("--cpus").arg("2");

    let sandbox_workdir = "/sandbox/workdir";
    let mount_opt = if config.allow_write {
        format!("{}:{}", workdir.display(), sandbox_workdir)
    } else {
        format!("{}:{}:ro", workdir.display(), sandbox_workdir)
    };
    cmd.arg("-v").arg(&mount_opt);

    for (i, allowed_path) in config.allowed_paths.iter().enumerate() {
        let ap = canonicalize_or_identity(allowed_path);
        let container_path = format!("/sandbox/extra_{}", i);
        let mount_opt = if config.allow_write {
            format!("{}:{}", ap.display(), container_path)
        } else {
            format!("{}:{}:ro", ap.display(), container_path)
        };
        cmd.arg("-v").arg(&mount_opt);
    }

    cmd.arg("-w").arg(sandbox_workdir);
    cmd.arg("--security-opt").arg("no-new-privileges:true");
    cmd.arg("--read-only");
    // M3: noexec prevents code execution from /tmp (defense-in-depth against shell/DLL injection)
    cmd.arg("--tmpfs").arg("/tmp:noexec,size=512M");

    cmd.arg(DEFAULT_IMAGE);
    cmd.arg("/bin/sh").arg("-c").arg(command);

    debug!(
        target: "shell.docker",
        ?command,
        ?workdir,
        "spawning docker container"
    );

    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| SandboxError::Spawn {
            backend: "docker",
            source: e,
        })?;

    let timeout_dur = std::time::Duration::from_secs(config.timeout_secs);
    let wait_result = timeout(timeout_dur, child.wait()).await;
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

    let stdout = read_pipe_output(&mut child, StdioPipe::Stdout, config.max_output_bytes).await?;
    let stderr = read_pipe_output(&mut child, StdioPipe::Stderr, config.max_output_bytes).await?;

    let exit_code = status.code().unwrap_or(-1);
    let duration_ms = start.elapsed().as_millis() as u64;

    debug!(
        target: "shell.docker",
        exit_code,
        duration_ms,
        stdout_len = stdout.len(),
        stderr_len = stderr.len(),
        "docker completed"
    );

    Ok(ExecResult {
        stdout,
        stderr,
        exit_code,
        duration_ms,
        timed_out: false,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// 标准 I/O 管道标识
///
/// 【领域含义】区分标准输出和标准错误管道。
/// 【核心职责】在输出读取函数中指定要读取的目标管道。
#[derive(Copy, Clone)]
enum StdioPipe {
    /// 标准输出
    Stdout,
    /// 标准错误
    Stderr,
}

async fn read_pipe_output(
    child: &mut tokio::process::Child,
    which: StdioPipe,
    max_bytes: usize,
) -> Result<String, SandboxError> {
    let mut reader: Box<dyn AsyncRead + Unpin + Send> = match which {
        StdioPipe::Stdout => match child.stdout.take() {
            Some(p) => Box::new(p),
            None => return Ok(String::new()),
        },
        StdioPipe::Stderr => match child.stderr.take() {
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

/// 路径规范化（失败时返回原路径）
///
/// 【领域含义】尝试将路径解析为绝对规范路径，失败时保持原样。
/// 【核心职责】确保挂载路径是绝对路径，避免 Docker 挂载相对路径时的歧义。
fn canonicalize_or_identity(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// 检测 Docker 是否可用
///
/// 【领域含义】检查系统中是否安装了 Docker CLI 且可执行。
/// 【核心职责】通过 `which::which("docker")` 探测 docker 命令是否在 PATH 中。
pub fn is_docker_available() -> bool {
    which::which("docker").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_docker_detection() {
        let _ = is_docker_available();
    }

    #[test]
    fn test_canonicalize_identity() {
        let p = Path::new("/nonexistent/docker/test");
        let result = canonicalize_or_identity(p);
        assert_eq!(result, p);
    }
}
