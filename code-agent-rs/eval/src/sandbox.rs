//! Docker sandbox availability checks and management.
//!
//! The actual sandbox execution lives in Python (`code_agent_eval.sandbox`).
//! This module provides pre-flight checks so the Rust side can determine
//! whether Docker is available before delegating to Python.

use std::process::Command;

/// 检查 Docker 守护进程是否可达
///
/// 【领域含义】评估基础设施的前置检查，确认 Docker 沙箱环境可用性。
/// 【核心职责】执行 `docker version --format json` 命令，返回 true 表示 Docker 可用。
/// 这是一个轻量预检，避免启动 Python 进程后才发现 Docker 不可用。
pub fn check_docker() -> bool {
    match Command::new("docker")
        .args(["version", "--format", "json"])
        .output()
    {
        Ok(output) => output.status.success(),
        Err(_) => false,
    }
}

/// Docker 沙箱环境信息
///
/// 【领域含义】表示评估沙箱环境的领域服务，封装 Docker 可用性状态。
/// 【核心职责】提供 Docker 可用性探测和状态查询，供 EvalRunner 决策是否使用沙箱。
#[derive(Debug, Clone)]
pub struct DockerSandbox {
    /// Docker 可用性 — 当前主机上 Docker 是否可用。
    pub docker_available: bool,
}

impl DockerSandbox {
    /// 创建沙箱句柄
    ///
    /// 【领域含义】构造 DockerSandbox 实例，自动探测 Docker 可用性。
    /// 【核心职责】初始化时执行 Docker 可达性检查，缓存结果供后续查询。
    pub fn new() -> Self {
        Self {
            docker_available: check_docker(),
        }
    }

    /// 判断 Docker 是否可达
    ///
    /// 【领域含义】查询 Docker 沙箱的可用性状态。
    /// 【核心职责】返回构造时缓存的 Docker 可用性检查结果。
    pub fn is_available(&self) -> bool {
        self.docker_available
    }
}

impl Default for DockerSandbox {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_docker_returns_bool() {
        // This test doesn't assert a particular value because Docker may
        // or may not be installed in the test environment.
        let available = check_docker();
        // The function must not panic regardless of environment.
        assert!(available || !available);
    }

    #[test]
    fn sandbox_default_does_not_panic() {
        let sb = DockerSandbox::default();
        let _ = sb.is_available();
    }
}
