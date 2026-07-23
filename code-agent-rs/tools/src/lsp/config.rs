use std::path::Path;

/// 语言服务器配置
///
/// 【领域含义】定义一种编程语言的 LSP 服务器信息，包括语言标识、文件扩展名和启动命令。
/// 【核心职责】提供 LSP 服务器发现所需的元数据，供 `LspServerManager` 启动服务器使用。
#[derive(Debug, Clone)]
pub struct LanguageConfig {
    /// 语言标识符（如 "python"、"rust"）
    pub language: &'static str,
    /// 该语言关联的文件扩展名（不含点号）
    pub extensions: &'static [&'static str],
    /// LSP 服务器可执行命令
    pub command: &'static str,
    /// LSP 服务器命令行参数
    pub args: &'static [&'static str],
}

/// 所有支持的 LSP 语言配置列表
///
/// 【领域含义】预定义的支持语言集合，LSP 服务器必须预先安装在系统 PATH 中。
/// 【核心职责】作为语言发现的数据源，供 `language_for_extension` 和 `detect_language` 查询。
pub static SUPPORTED_LANGUAGES: &[LanguageConfig] = &[
    LanguageConfig {
        language: "python",
        extensions: &["py", "pyi"],
        command: "pyright-langserver",
        args: &["--stdio"],
    },
    LanguageConfig {
        language: "rust",
        extensions: &["rs"],
        command: "rust-analyzer",
        args: &[],
    },
    LanguageConfig {
        language: "typescript",
        extensions: &["ts", "tsx", "js", "jsx", "mts", "cts"],
        command: "typescript-language-server",
        args: &["--stdio"],
    },
    LanguageConfig {
        language: "go",
        extensions: &["go"],
        command: "gopls",
        args: &[],
    },
    LanguageConfig {
        language: "java",
        extensions: &["java"],
        command: "jdtls",
        args: &[],
    },
];

/// 根据文件扩展名查找语言配置
///
/// 【领域含义】通过文件扩展名（不含点号）查找对应的 `LanguageConfig`。
/// 【核心职责】遍历 `SUPPORTED_LANGUAGES`，匹配扩展名，返回配置引用。
/// 返回 `None` 表示该扩展名没有对应的已知语言。
pub fn language_for_extension(ext: &str) -> Option<&'static LanguageConfig> {
    SUPPORTED_LANGUAGES
        .iter()
        .find(|cfg| cfg.extensions.contains(&ext))
}

/// 检测文件的语言
///
/// 【领域含义】通过文件路径的扩展名自动检测编程语言。
/// 【核心职责】提取扩展名 → 调用 `language_for_extension` → 返回语言标识符字符串。
/// 返回 `None` 表示无法识别该文件的语言。
pub fn detect_language(file: &Path) -> Option<&'static str> {
    let ext = file.extension()?.to_str()?;
    language_for_extension(ext).map(|cfg| cfg.language)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_rust_from_rs_extension() {
        assert_eq!(detect_language(Path::new("main.rs")), Some("rust"));
    }

    #[test]
    fn detects_python_from_py_extension() {
        assert_eq!(detect_language(Path::new("app.py")), Some("python"));
    }

    #[test]
    fn detects_typescript_from_ts_extension() {
        assert_eq!(
            detect_language(Path::new("index.ts")),
            Some("typescript")
        );
    }

    #[test]
    fn detects_typescript_from_tsx_extension() {
        assert_eq!(
            detect_language(Path::new("component.tsx")),
            Some("typescript")
        );
    }

    #[test]
    fn detects_go_from_go_extension() {
        assert_eq!(detect_language(Path::new("main.go")), Some("go"));
    }

    #[test]
    fn detects_java_from_java_extension() {
        assert_eq!(detect_language(Path::new("Main.java")), Some("java"));
    }

    #[test]
    fn returns_none_for_unknown_extension() {
        assert_eq!(detect_language(Path::new("script.sh")), None);
    }

    #[test]
    fn returns_none_for_no_extension() {
        assert_eq!(detect_language(Path::new("Makefile")), None);
    }
}
