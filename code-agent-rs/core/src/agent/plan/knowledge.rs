//! 知识体系 —— Rules/Skills 的存储、检索与注入。
//!
//! 【领域含义】Phase E 的核心：为 Plan 引擎提供结构化的知识注入能力。
//! 知识以 `.md` 文件形式存储在 `.craftcoder/rules/` 和 `.craftcoder/skills/`
//! 目录中，每个文件包含 YAML 风格的前置元数据（frontmatter）和 Markdown 正文。
//!
//! # 架构
//!
//! ```text
//! KnowledgeConfig (rules_dir, skills_dir)
//!     ↓
//! KnowledgeStore (加载 .craftcoder/rules/*.md → Vec<Rule>)
//!     ↓
//! KnowledgeProvider trait (get_rules, format_context)
//!     ↓
//! PlanExecutor / Decomposer / QualityGate 注入点
//! ```
//!
//! # 文件格式
//!
//! ```markdown
//! ---
//! name: rust-style
//! description: Rust 编码规范
//! severity: error
//! tags: ["rust", "style"]
//! ---
//! ## 命名规范
//! - 使用 snake_case 命名函数和变量
//! - 使用 CamelCase 命名类型和 trait
//! ...
//! ```
//!
//! # 集成点
//!
//! | 组件 | 注入方式 |
//! |------|---------|
//! | `DecompositionEngine::build_prompt()` | 在 prompt 中插入 `## Knowledge Base` 节 |
//! | `PlanExecutor` | 持有 `Option<Arc<dyn KnowledgeProvider>>` |
//! | `QualityGate` | `KnowledgeGate` 将规则检查纳入门禁管道 |
//!
//! # 三层层级
//!
//! 知识按优先级从高到低合并（参考 CLI ConfigManager 的 merge 模式）：
//! 1. 会话级（运行时注入）
//! 2. 项目级（`.craftcoder/rules/`）
//! 3. 全局级（`~/.craftcoder/rules/`）

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::quality::{GateVerdict, QualityGate};
use super::types::{PlanConfig, PlanNode};
use crate::agent::sub_agent::AgentResult;

// ---------------------------------------------------------------------------
// RuleSeverity — 规则严重级别
// ---------------------------------------------------------------------------

/// 规则严重级别。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSeverity {
    /// 关键规则，违反会导致计划失败
    Critical,
    /// 错误级别，应修复
    Error,
    /// 警告级别，建议遵循
    Warning,
    /// 信息级别，仅供参考
    Info,
}

impl fmt::Display for RuleSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleSeverity::Critical => write!(f, "critical"),
            RuleSeverity::Error => write!(f, "error"),
            RuleSeverity::Warning => write!(f, "warning"),
            RuleSeverity::Info => write!(f, "info"),
        }
    }
}

// ---------------------------------------------------------------------------
// Rule — 单条知识规则
// ---------------------------------------------------------------------------

/// 从 `.craftcoder/rules/*.md` 文件加载的规则。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Rule {
    /// 规则名称（文件名或 frontmatter name）
    pub name: String,
    /// 规则描述
    pub description: String,
    /// 严重级别
    pub severity: RuleSeverity,
    /// 标签（用于检索过滤）
    #[serde(default)]
    pub tags: Vec<String>,
    /// Markdown 正文内容
    pub content: String,
    /// 源文件路径（调试用）
    pub source_path: Option<String>,
}

// ---------------------------------------------------------------------------
// KnowledgeConfig — 知识模块配置
// ---------------------------------------------------------------------------

/// 知识模块配置。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeConfig {
    /// 是否启用知识注入（默认 true）
    pub enabled: bool,
    /// 规则目录路径（相对于项目根目录，默认 ".craftcoder/rules"）
    pub rules_dir: Option<String>,
    /// 技能目录路径（相对于项目根目录，默认 ".craftcoder/skills"）
    pub skills_dir: Option<String>,
    /// 是否自动将知识注入分解 prompt
    pub inject_decomposition: bool,
    /// 是否启用 KnowledgeGate 质量门禁
    pub enable_gate: bool,
}

impl Default for KnowledgeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            rules_dir: Some(".craftcoder/rules".into()),
            skills_dir: Some(".craftcoder/skills".into()),
            inject_decomposition: true,
            enable_gate: false, // 默认关闭，待知识库完善后开启
        }
    }
}

impl KnowledgeConfig {
    /// 从配置构建 KnowledgeProvider。
    ///
    /// 如果 `enabled = true` 且 `rules_dir` 有值，则加载对应的 KnowledgeStore。
    /// 否则返回 EmptyKnowledgeProvider（空实现，不报错）。
    pub async fn build_provider(&self) -> Arc<dyn KnowledgeProvider> {
        if !self.enabled {
            return Arc::new(EmptyKnowledgeProvider);
        }
        match &self.rules_dir {
            Some(dir) => {
                let store = KnowledgeStore::load(dir).await;
                if store.is_empty() {
                    tracing::info!(
                        dir = %dir,
                        "KnowledgeConfig: rules directory empty or missing, using empty store"
                    );
                }
                Arc::new(store) as Arc<dyn KnowledgeProvider>
            }
            None => {
                tracing::info!("KnowledgeConfig: no rules_dir configured, using empty provider");
                Arc::new(EmptyKnowledgeProvider) as Arc<dyn KnowledgeProvider>
            }
        }
    }
}

// ---------------------------------------------------------------------------
// KnowledgeError — 知识模块错误
// ---------------------------------------------------------------------------

/// 知识模块错误。
#[derive(Debug, thiserror::Error)]
pub enum KnowledgeError {
    /// 文件 I/O 错误
    #[error("Knowledge IO error: {0}")]
    Io(#[from] std::io::Error),

    /// 规则解析错误
    #[error("Knowledge parse error: {0}")]
    Parse(String),

    /// 未配置知识目录
    #[error("Knowledge directory not configured")]
    NoDirectory,

    /// 规则未找到
    #[error("Rule not found: {0}")]
    RuleNotFound(String),
}

// ---------------------------------------------------------------------------
// KnowledgeProvider trait — 知识提供者接口
// ---------------------------------------------------------------------------

/// 知识提供者接口。
///
/// 实现该 trait 的类型可以提供结构化知识（规则/技能/领域知识），
/// 用于注入到分解 prompt、Coder 系统指令和质量门禁中。
#[async_trait::async_trait]
pub trait KnowledgeProvider: Send + Sync {
    /// 提供者名称（用于日志和调试）。
    fn name(&self) -> &str;

    /// 获取与查询相关的所有规则。
    async fn get_rules(&self, query: &str) -> Vec<Rule>;

    /// 获取指定标签的规则。
    async fn get_rules_by_tag(&self, tag: &str) -> Vec<Rule>;

    /// 获取所有规则。
    async fn all_rules(&self) -> Vec<Rule>;

    /// 格式化为注入 prompt 的上下文文本。
    async fn format_context(&self, query: &str) -> String;
}

/// 空知识提供者（不提供任何规则，用于关闭知识注入）。
pub struct EmptyKnowledgeProvider;

#[async_trait::async_trait]
impl KnowledgeProvider for EmptyKnowledgeProvider {
    fn name(&self) -> &str {
        "EmptyKnowledgeProvider"
    }

    async fn get_rules(&self, _query: &str) -> Vec<Rule> {
        vec![]
    }

    async fn get_rules_by_tag(&self, _tag: &str) -> Vec<Rule> {
        vec![]
    }

    async fn all_rules(&self) -> Vec<Rule> {
        vec![]
    }

    async fn format_context(&self, _query: &str) -> String {
        String::new()
    }
}

// ---------------------------------------------------------------------------
// Frontmatter Parser — 前置元数据解析
// ---------------------------------------------------------------------------

/// 解析 Markdown 文件的 YAML-like 前置元数据。
///
/// 格式：
/// ```markdown
/// ---
/// name: rule-name
/// description: Rule description
/// severity: warning
/// tags: ["tag1", "tag2"]
/// ---
/// Content body...
/// ```
fn parse_frontmatter(content: &str) -> Result<(Rule, String), KnowledgeError> {
    let content = content.trim();

    // 检查是否有 --- 分隔符
    if !content.starts_with("---") {
        return Err(KnowledgeError::Parse(
            "Missing frontmatter delimiter '---'".into(),
        ));
    }

    // 找到第二个 ---
    let rest = &content[3..];
    let end = rest
        .find("\n---")
        .ok_or_else(|| KnowledgeError::Parse("Missing closing frontmatter delimiter '---'".into()))?;

    let frontmatter_str = rest[..end].trim();
    let body = rest[end + 4..].trim();

    // 解析 frontmatter 字段
    let mut name = String::new();
    let mut description = String::new();
    let mut severity = RuleSeverity::Warning;
    let mut tags: Vec<String> = Vec::new();

    for line in frontmatter_str.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim();
            match key {
                "name" => name = value.to_string(),
                "description" => description = clean_value(value),
                "severity" => {
                    severity = match value {
                        "critical" => RuleSeverity::Critical,
                        "error" => RuleSeverity::Error,
                        "warning" => RuleSeverity::Warning,
                        "info" => RuleSeverity::Info,
                        _ => RuleSeverity::Warning,
                    };
                }
                "tags" => {
                    // Parse JSON array like ["rust", "style"]
                    let trimmed = value.trim();
                    if trimmed.starts_with('[') && trimmed.ends_with(']') {
                        let inner = &trimmed[1..trimmed.len() - 1];
                        for t in inner.split(',') {
                            let t = t.trim().trim_matches('"').trim_matches('\'').to_string();
                            if !t.is_empty() {
                                tags.push(t);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if name.is_empty() {
        return Err(KnowledgeError::Parse(
            "Frontmatter missing required 'name' field".into(),
        ));
    }

    Ok((
        Rule {
            name,
            description,
            severity,
            tags,
            content: body.to_string(),
            source_path: None,
        },
        body.to_string(),
    ))
}

/// 清理值字符串中的引号。
fn clean_value(s: &str) -> String {
    s.trim().trim_matches('"').trim_matches('\'').to_string()
}

// ---------------------------------------------------------------------------
// KnowledgeStore — 基于文件系统的知识存储
// ---------------------------------------------------------------------------

/// 基于文件系统的知识存储。
///
/// 从指定目录加载所有 `.md` 文件，解析 frontmatter 并缓存。
/// 支持按标签和查询过滤。
pub struct KnowledgeStore {
    /// 所有已加载的规则
    rules: Vec<Rule>,
    /// 规则根目录
    _root_dir: PathBuf,
}

impl KnowledgeStore {
    /// 从指定目录加载知识规则。
    ///
    /// 扫描目录下所有 `*.md` 文件，解析每个文件的 frontmatter 和正文。
    /// 如果目录不存在或为空，返回空的 KnowledgeStore（不报错）。
    pub async fn load<P: AsRef<Path>>(dir: P) -> Self {
        let dir = dir.as_ref().to_path_buf();
        let mut rules = Vec::new();

        if dir.exists() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(_) => {
                    return Self {
                        rules: vec![],
                        _root_dir: dir,
                    };
                }
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "md").unwrap_or(false) {
                    match load_rule_from_file(&path) {
                        Ok(mut rule) => {
                            rule.source_path = Some(path.to_string_lossy().to_string());
                            rules.push(rule);
                        }
                        Err(e) => {
                            tracing::warn!(
                                path = %path.display(),
                                error = %e,
                                "KnowledgeStore: skipping invalid rule file"
                            );
                        }
                    }
                }
            }
        } else {
            tracing::info!(
                dir = %dir.display(),
                "KnowledgeStore: rules directory does not exist, using empty store"
            );
        }

        Self {
            rules,
            _root_dir: dir,
        }
    }

    /// 规则总数。
    pub fn count(&self) -> usize {
        self.rules.len()
    }

    /// 是否为空。
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// 获取所有规则。
    pub fn all_rules(&self) -> &[Rule] {
        &self.rules
    }

    /// 获取匹配查询的规则（简单关键词匹配）。
    pub fn query(&self, query: &str) -> Vec<Rule> {
        let q = query.to_lowercase();
        self.rules
            .iter()
            .filter(|r| {
                r.name.to_lowercase().contains(&q)
                    || r.description.to_lowercase().contains(&q)
                    || r.content.to_lowercase().contains(&q)
                    || r.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .cloned()
            .collect()
    }

    /// 获取指定标签的规则。
    pub fn by_tag(&self, tag: &str) -> Vec<Rule> {
        let t = tag.to_lowercase();
        self.rules
            .iter()
            .filter(|r| r.tags.iter().any(|rt| rt.to_lowercase() == t))
            .cloned()
            .collect()
    }

    /// 格式化为 prompt 注入文本。
    pub fn format_context(&self, query: &str) -> String {
        let matched = self.query(query);
        if matched.is_empty() {
            return String::new();
        }

        let mut ctx = String::from("\n## Knowledge Base\n\n");
        for rule in &matched {
            ctx.push_str(&format!("### {} ({})\n", rule.name, rule.severity));
            if !rule.description.is_empty() {
                ctx.push_str(&format!("{}\n\n", rule.description));
            }
            ctx.push_str(&format!("{}\n\n", rule.content));
        }
        ctx
    }
}

#[async_trait::async_trait]
impl KnowledgeProvider for KnowledgeStore {
    fn name(&self) -> &str {
        "KnowledgeStore"
    }

    async fn get_rules(&self, query: &str) -> Vec<Rule> {
        self.query(query)
    }

    async fn get_rules_by_tag(&self, tag: &str) -> Vec<Rule> {
        self.by_tag(tag)
    }

    async fn all_rules(&self) -> Vec<Rule> {
        self.rules.clone()
    }

    async fn format_context(&self, query: &str) -> String {
        self.format_context(query)
    }
}

/// 从单个文件加载并解析规则。
fn load_rule_from_file(path: &Path) -> Result<Rule, KnowledgeError> {
    let content = std::fs::read_to_string(path)?;
    let (mut rule, _body) = parse_frontmatter(&content)?;
    rule.source_path = Some(path.to_string_lossy().to_string());
    Ok(rule)
}

// ---------------------------------------------------------------------------
// KnowledgeGate — 知识驱动的质量门禁
// ---------------------------------------------------------------------------

/// 知识驱动的质量门禁 —— 将规则检查纳入质量门禁管道。
///
/// 根据规则的严重级别，对执行结果进行裁决：
/// - `Critical` 规则违反 → `GateVerdict::Fail`
/// - `Error` 规则违反 → `GateVerdict::Retry`
/// - `Warning` 规则违反 → `GateVerdict::Fail`（不可重试）
/// - `Info` 规则遵守 → `GateVerdict::Pass`
pub struct KnowledgeGate {
    /// 知识提供者
    provider: Arc<dyn KnowledgeProvider>,
    /// 门禁配置
    config: KnowledgeGateConfig,
}

/// KnowledgeGate 配置。
#[derive(Clone, Debug)]
pub struct KnowledgeGateConfig {
    /// 是否启用 Critical 规则强制失败
    pub fail_on_critical: bool,
    /// 是否对 Error 规则启用重试
    pub retry_on_error: bool,
}

impl Default for KnowledgeGateConfig {
    fn default() -> Self {
        Self {
            fail_on_critical: true,
            retry_on_error: true,
        }
    }
}

impl KnowledgeGate {
    /// 创建知识门禁。
    pub fn new(provider: Arc<dyn KnowledgeProvider>) -> Self {
        Self {
            provider,
            config: KnowledgeGateConfig::default(),
        }
    }

    /// 使用自定义配置创建知识门禁。
    pub fn with_config(provider: Arc<dyn KnowledgeProvider>, config: KnowledgeGateConfig) -> Self {
        Self { provider, config }
    }
}

#[async_trait::async_trait]
impl QualityGate for KnowledgeGate {
    fn name(&self) -> &str {
        "KnowledgeGate"
    }

    async fn check(
        &self,
        result: &AgentResult,
        node: &PlanNode,
        _config: &PlanConfig,
    ) -> GateVerdict {
        // 获取与节点相关的规则
        let rules = self
            .provider
            .get_rules(&format!("{} {}", node.label, node.spec.as_deref().unwrap_or("")))
            .await;

        for rule in &rules {
            match rule.severity {
                RuleSeverity::Critical => {
                    if self.config.fail_on_critical && result.error.is_some() {
                        return GateVerdict::Fail {
                            reason: format!(
                                "Critical rule '{}' violated: {}",
                                rule.name, rule.description
                            ),
                        };
                    }
                }
                RuleSeverity::Error => {
                    if self.config.retry_on_error && result.error.is_some() {
                        return GateVerdict::Retry {
                            reason: format!(
                                "Error rule '{}' violated: {}",
                                rule.name, rule.description
                            ),
                        };
                    }
                }
                RuleSeverity::Warning => {
                    if result.error.is_some() {
                        return GateVerdict::Fail {
                            reason: format!(
                                "Warning rule '{}' violation: {}",
                                rule.name, rule.description
                            ),
                        };
                    }
                }
                RuleSeverity::Info => {
                    // Info rules don't gate — informational only
                }
            }
        }

        GateVerdict::Pass
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── Frontmatter Parsing ─────────────────────────────────────────

    #[test]
    fn test_parse_frontmatter_basic() {
        let content = r#"---
name: rust-style
description: Rust 编码风格规范
severity: error
tags: ["rust", "style"]
---
## 命名规则
使用 snake_case 命名函数。"#;

        let (rule, body) = parse_frontmatter(content).unwrap();
        assert_eq!(rule.name, "rust-style");
        assert_eq!(rule.description, "Rust 编码风格规范");
        assert_eq!(rule.severity, RuleSeverity::Error);
        assert_eq!(rule.tags, vec!["rust", "style"]);
        assert!(body.contains("snake_case"));
    }

    #[test]
    fn test_parse_frontmatter_missing_name_is_error() {
        let content = r#"---
description: no name here
severity: warning
---
Some content"#;
        let err = parse_frontmatter(content).unwrap_err();
        assert!(err.to_string().contains("name"));
    }

    #[test]
    fn test_parse_frontmatter_missing_delimiter() {
        let content = "no frontmatter here";
        let err = parse_frontmatter(content).unwrap_err();
        assert!(err.to_string().contains("delimiter"));
    }

    #[test]
    fn test_parse_frontmatter_default_severity() {
        let content = r#"---
name: info-rule
description: Just info
---
Content"#;
        let (rule, _) = parse_frontmatter(content).unwrap();
        assert_eq!(rule.severity, RuleSeverity::Warning);
    }

    #[test]
    fn test_parse_frontmatter_critical_severity() {
        let content = r#"---
name: critical-rule
description: Must not violate
severity: critical
---
Do not do X"#;
        let (rule, _) = parse_frontmatter(content).unwrap();
        assert_eq!(rule.severity, RuleSeverity::Critical);
    }

    // ── KnowledgeStore ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_store_load_nonexistent_dir() {
        let store = KnowledgeStore::load("/tmp/nonexistent-rules-dir-12345").await;
        assert!(store.is_empty());
        assert_eq!(store.count(), 0);
    }

    #[tokio::test]
    async fn test_store_load_from_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test-rule.md");
        let content = r#"---
name: test-rule
description: A test rule
severity: info
tags: ["test"]
---
Test content here"#;
        std::fs::write(&path, content).unwrap();

        let store = KnowledgeStore::load(dir.path()).await;
        assert_eq!(store.count(), 1);
        assert_eq!(store.all_rules()[0].name, "test-rule");
    }

    #[tokio::test]
    async fn test_store_ignores_non_md_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("note.txt"), "not a rule").unwrap();
        std::fs::write(dir.path().join("data.json"), "{}").unwrap();

        let store = KnowledgeStore::load(dir.path()).await;
        assert!(store.is_empty());
    }

    #[tokio::test]
    async fn test_store_query_by_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("rust-style.md"),
            r#"---
name: rust-style
description: Rust coding style
severity: error
tags: ["rust"]
---
Use snake_case"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("python-style.md"),
            r#"---
name: python-style
description: Python coding style
severity: warning
tags: ["python"]
---
Use snake_case"#,
        )
        .unwrap();

        let store = KnowledgeStore::load(dir.path()).await;
        let results = store.query("rust");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "rust-style");
    }

    #[tokio::test]
    async fn test_store_query_by_tag() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("async.md"),
            r#"---
name: async-patterns
description: Async patterns
severity: info
tags: ["rust", "async"]
---
Use tokio"#,
        )
        .unwrap();

        let store = KnowledgeStore::load(dir.path()).await;
        let results = store.by_tag("async");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "async-patterns");
    }

    // ── Format Context ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_format_context_with_matches() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("test.md"),
            r#"---
name: test-rule
description: Test description
severity: info
tags: ["test"]
---
Content body here"#,
        )
        .unwrap();

        let store = KnowledgeStore::load(dir.path()).await;
        let ctx = store.format_context("test");
        assert!(ctx.contains("Knowledge Base"));
        assert!(ctx.contains("test-rule"));
        assert!(ctx.contains("Content body here"));
    }

    #[tokio::test]
    async fn test_format_context_empty_when_no_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("test.md"),
            r#"---
name: test-rule
description: Test
severity: info
---
Content"#,
        )
        .unwrap();

        let store = KnowledgeStore::load(dir.path()).await;
        let ctx = store.format_context("nonexistent");
        assert!(ctx.is_empty());
    }

    // ── KnowledgeGate ───────────────────────────────────────────────

    fn make_clean_result() -> AgentResult {
        AgentResult {
            agent_id: "test".into(),
            status: crate::agent::sub_agent::AgentStatus::Completed,
            events: vec![],
            final_message: Some("ok".into()),
            error: None,
        }
    }

    fn make_err_result() -> AgentResult {
        AgentResult {
            agent_id: "test".into(),
            status: crate::agent::sub_agent::AgentStatus::Failed,
            events: vec![],
            final_message: None,
            error: Some("something went wrong".into()),
        }
    }

    fn make_node() -> PlanNode {
        PlanNode {
            id: "test-node".into(),
            label: "Test Node".into(),
            spec: Some("Do the thing".to_string()),
            ..Default::default()
        }
    }

    struct MockProvider {
        rules: Vec<Rule>,
    }

    #[async_trait::async_trait]
    impl KnowledgeProvider for MockProvider {
        fn name(&self) -> &str {
            "MockProvider"
        }

        async fn get_rules(&self, _query: &str) -> Vec<Rule> {
            self.rules.clone()
        }

        async fn get_rules_by_tag(&self, _tag: &str) -> Vec<Rule> {
            self.rules.clone()
        }

        async fn all_rules(&self) -> Vec<Rule> {
            self.rules.clone()
        }

        async fn format_context(&self, _query: &str) -> String {
            String::new()
        }
    }

    #[tokio::test]
    async fn test_knowledge_gate_passes_clean_result() {
        let provider = Arc::new(MockProvider {
            rules: vec![Rule {
                name: "no-error".into(),
                description: "No errors allowed".into(),
                severity: RuleSeverity::Critical,
                tags: vec![],
                content: String::new(),
                source_path: None,
            }],
        });
        let gate = KnowledgeGate::new(provider);
        let verdict = gate.check(&make_clean_result(), &make_node(), &PlanConfig::default()).await;
        assert_eq!(verdict, GateVerdict::Pass);
    }

    #[tokio::test]
    async fn test_knowledge_gate_fails_on_critical_violation() {
        let provider = Arc::new(MockProvider {
            rules: vec![Rule {
                name: "no-error".into(),
                description: "No errors allowed".into(),
                severity: RuleSeverity::Critical,
                tags: vec![],
                content: String::new(),
                source_path: None,
            }],
        });
        let gate = KnowledgeGate::new(provider);
        let verdict = gate
            .check(&make_err_result(), &make_node(), &PlanConfig::default())
            .await;
        assert!(verdict.is_fail());
        assert!(verdict.reason().unwrap().contains("no-error"));
    }

    #[tokio::test]
    async fn test_knowledge_gate_retries_on_error_violation() {
        let provider = Arc::new(MockProvider {
            rules: vec![Rule {
                name: "should-retry".into(),
                description: "Should retry on error".into(),
                severity: RuleSeverity::Error,
                tags: vec![],
                content: String::new(),
                source_path: None,
            }],
        });
        let gate = KnowledgeGate::new(provider);
        let verdict = gate
            .check(&make_err_result(), &make_node(), &PlanConfig::default())
            .await;
        assert!(verdict.is_retry());
    }

    #[tokio::test]
    async fn test_knowledge_gate_no_rules_always_passes() {
        let provider = Arc::new(MockProvider { rules: vec![] });
        let gate = KnowledgeGate::new(provider);
        let verdict = gate
            .check(&make_err_result(), &make_node(), &PlanConfig::default())
            .await;
        assert_eq!(verdict, GateVerdict::Pass);
    }

    #[tokio::test]
    async fn test_empty_provider_returns_empty_context() {
        let provider = EmptyKnowledgeProvider;
        let ctx = provider.format_context("anything").await;
        assert!(ctx.is_empty());
    }

    #[tokio::test]
    async fn test_empty_provider_returns_no_rules() {
        let provider = EmptyKnowledgeProvider;
        assert!(provider.get_rules("test").await.is_empty());
        assert!(provider.all_rules().await.is_empty());
    }
}
