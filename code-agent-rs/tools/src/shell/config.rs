use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// 沙箱执行配置
///
/// 【领域含义】定义沙箱执行环境的约束参数，控制命令的权限边界和资源限制。
/// 【核心职责】提供可序列化的配置结构，支持从 JSON/YAML 反序列化加载。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// 允许网络访问
    ///
    /// 【领域含义】沙箱内命令是否可以访问网络。关闭时 Docker 使用 `--network none`，bwrap 使用 `--unshare-net`。
    #[serde(default)]
    pub allow_network: bool,

    /// 允许文件系统写入
    ///
    /// 【领域含义】沙箱内命令是否可以修改文件系统。关闭时工作目录和允许路径以只读方式挂载。
    #[serde(default)]
    pub allow_write: bool,

    /// 暴露给沙箱的额外路径
    ///
    /// 【领域含义】除工作目录外，允许沙箱访问的其他文件系统路径。
    #[serde(default)]
    pub allowed_paths: Vec<PathBuf>,

    /// 执行超时秒数。默认：60。
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,

    /// 最大输出字节数（stdout + stderr 合计）。默认：1 MiB。
    #[serde(default = "default_max_output_bytes")]
    pub max_output_bytes: usize,

    /// 内存限制（MB）。默认：4096。
    #[serde(default = "default_memory_limit_mb")]
    pub memory_limit_mb: u64,
}

/// 默认超时秒数：60 秒
const fn default_timeout_secs() -> u64 {
    60
}

/// 默认最大输出字节数：1 MiB
const fn default_max_output_bytes() -> usize {
    1024 * 1024 // 1 MiB
}

/// 默认内存限制：4096 MB
const fn default_memory_limit_mb() -> u64 {
    4096
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            allow_network: false,
            allow_write: false,
            allowed_paths: Vec::new(),
            timeout_secs: default_timeout_secs(),
            max_output_bytes: default_max_output_bytes(),
            memory_limit_mb: default_memory_limit_mb(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_values() {
        let config = SandboxConfig::default();
        assert!(!config.allow_network);
        assert!(!config.allow_write);
        assert!(config.allowed_paths.is_empty());
        assert_eq!(config.timeout_secs, 60);
        assert_eq!(config.max_output_bytes, 1024 * 1024);
        assert_eq!(config.memory_limit_mb, 4096);
    }

    #[test]
    fn test_custom_config() {
        let config = SandboxConfig {
            allow_network: true,
            allow_write: true,
            allowed_paths: vec![PathBuf::from("/tmp")],
            timeout_secs: 30,
            max_output_bytes: 512,
            memory_limit_mb: 256,
        };
        assert!(config.allow_network);
        assert!(config.allow_write);
        assert_eq!(config.allowed_paths.len(), 1);
        assert_eq!(config.timeout_secs, 30);
        assert_eq!(config.max_output_bytes, 512);
        assert_eq!(config.memory_limit_mb, 256);
    }

    #[test]
    fn test_serde_defaults() {
        let json = r#"{}"#;
        let config: SandboxConfig = serde_json::from_str(json).unwrap();
        assert!(!config.allow_network);
        assert!(!config.allow_write);
        assert_eq!(config.timeout_secs, 60);
    }
}
