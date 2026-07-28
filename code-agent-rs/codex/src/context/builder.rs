//! Context assembly pipeline — the engine behind `CodeContextBuilder`.
//!
//! Implements the 6-step pipeline:
//! 1. Task embedding (via Retriever's vector search)
//! 2. Hybrid search (BM25 + vector → RRF fusion → optional reranker)
//! 3. Graph expansion (1-hop call-graph neighbours)
//! 4. Priority weighting (recency, open files, explicit mentions)
//! 5. Token budget allocation (truncate to max_context_tokens)
//! 6. XML formatting (structured prompt block)
//!
//! # Output format
//!
//! ```xml
//! <context>
//! <file path="src/auth/login.rs" lines="42-58">
//! // Function: validateToken
//! // Signature: fn validateToken(token: &str) -> Result<User, AuthError>
//! pub fn validateToken(token: &str) -> Result<User, AuthError> { ... }
//! </file>
//! ...
//! <call-graph>
//! validateToken (src/auth/login.rs:42)
//!   └─ called by: AuthMiddleware::handle (src/auth/middleware.rs:12)
//!   └─ calls: decode_token (src/auth/jwt.rs:88)
//! </call-graph>
//! </context>
//! ```

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::graph::CallGraph;
use crate::retrieval::{Retriever, ScoredResult, SearchOptions};

use super::{
    CodeContext, ContextConfig, ContextError, ContextResult, ContextSignals,
    PrioritySignal, RetrievalStats,
};

/// Approximate tokens per character (rough heuristic: 1 token ≈ 4 chars).
const CHARS_PER_TOKEN: usize = 4;

/// 上下文组装器（内部领域服务）
///
/// 【领域含义】执行完整六步管道的内部领域服务，是 CodeContextBuilder 的引擎。
/// 负责混合检索、图谱扩展、优先级加权、Token 预算分配和 XML 格式化的具体实现。
pub(crate) struct ContextAssembler {
    retriever: Arc<Retriever>,
    graph: Option<Arc<CallGraph>>,
    config: ContextConfig,
}

impl ContextAssembler {
    pub fn new(
        retriever: Arc<Retriever>,
        graph: Option<Arc<CallGraph>>,
        config: ContextConfig,
    ) -> Self {
        Self {
            retriever,
            graph,
            config,
        }
    }

    /// 执行完整组装管道
    ///
    /// 【领域含义】执行完整的六步上下文组装管道，返回包含检索结果、XML 格式块和统计信息的 CodeContext。
    pub async fn assemble(
        &self,
        task: &str,
        signals: &ContextSignals,
    ) -> ContextResult<CodeContext> {
        // Step 1+2: Hybrid search (embedding is implicit inside Retriever)
        let search_opts = SearchOptions {
            top_k: self.config.top_k_symbols,
            languages: None,
            use_reranker: self.config.enable_reranker,
            use_graph: false, // We do graph expansion ourselves
        };

        let mut results = self
            .retriever
            .search(task, search_opts)
            .await
            .map_err(ContextError::from)?;

        let bm25_count = results.iter().filter(|r| r.source == "bm25").count();
        let vector_count = results.iter().filter(|r| r.source == "vector").count();

        // Step 3: Graph expansion — 1-hop callers/callees
        let graph_expanded = if self.config.enable_graph_expansion {
            expand_graph(self.graph.as_deref(), &mut results)
        } else {
            0
        };

        // Step 4: Priority weighting
        apply_signals(&self.config, &mut results, signals);

        let total_candidates = results.len();

        // Step 5: Token budget allocation
        let selected = apply_token_budget(&self.config, &mut results);

        // Step 6: Format into XML block
        let formatted_block = format_context(
            &self.config,
            self.graph.as_deref(),
            &results,
            &selected,
        );

        let token_count = formatted_block.len() / CHARS_PER_TOKEN;

        let stats = RetrievalStats {
            bm25_results: bm25_count,
            vector_results: vector_count,
            graph_expanded,
            total_candidates,
            final_selected: selected.len(),
            latency_ms: 0, // filled by caller
        };

        Ok(CodeContext {
            symbols: results,
            formatted_block,
            token_count,
            retrieval_stats: stats,
        })
    }
}

// ── Free functions (testable without Retriever instance) ─────────────────

/// 图谱扩展
///
/// 【领域含义】将检索结果扩展为包含 1 跳调用图邻居。对每个结果符号，查询其调用方
/// 和被调用方并加入结果集。返回新增的符号数量。扩展的符号评分降为原结果的 50%。
pub(crate) fn expand_graph(
    graph: Option<&CallGraph>,
    results: &mut Vec<ScoredResult>,
) -> usize {
    let graph = match graph {
        Some(g) => g,
        None => return 0,
    };

    let mut seen: HashSet<(String, String)> = results
        .iter()
        .map(|r| (r.file_path.clone(), r.symbol_name.clone()))
        .collect();

    let mut added = 0;

    // Take a snapshot of the top results to expand from
    let top_n = results.len().min(15);
    let candidates: Vec<ScoredResult> = results[..top_n].to_vec();

    for result in &candidates {
        let key = format!("{}::{}", result.file_path, result.symbol_name);

        // Get callers (who calls this symbol?)
        for edge in graph.get_callers(&key) {
            let pair = (edge.caller_file.clone(), edge.caller_symbol.clone());
            if seen.insert(pair.clone()) {
                results.push(ScoredResult {
                    symbol_name: edge.caller_symbol,
                    symbol_kind: "function".into(),
                    file_path: edge.caller_file,
                    line_range: edge.call_line..edge.call_line + 1,
                    score: result.score * 0.5, // lower score for graph-expanded
                    source: "graph_callers".into(),
                });
                added += 1;
            }
        }

        // Get callees (what does this symbol call?)
        for edge in graph.get_callees(&key) {
            let pair = (edge.callee_file.clone(), edge.callee_symbol.clone());
            if seen.insert(pair.clone()) {
                results.push(ScoredResult {
                    symbol_name: edge.callee_symbol,
                    symbol_kind: "function".into(),
                    file_path: edge.callee_file,
                    line_range: edge.call_line..edge.call_line + 1,
                    score: result.score * 0.5,
                    source: "graph_callees".into(),
                });
                added += 1;
            }
        }
    }

    added
}

/// 应用优先级信号
///
/// 【领域含义】根据运行时信号（最近编辑、打开文件、显式提及、当前文件）对检索结果的
/// 相关性评分进行加权提升。显式提及 3 倍、打开文件 2 倍、最近编辑 1.5 倍、当前文件 1.3 倍。
pub(crate) fn apply_signals(
    config: &ContextConfig,
    results: &mut [ScoredResult],
    signals: &ContextSignals,
) {
    if config.priority_signals.is_empty() {
        return;
    }

    // Pre-compute file match sets
    let recent_set: HashSet<&str> =
        signals.recently_edited.iter().map(|s| s.as_str()).collect();
    let open_set: HashSet<&str> =
        signals.open_files.iter().map(|s| s.as_str()).collect();
    let explicit_set: HashSet<&str> =
        signals.explicit_mentions.iter().map(|s| s.as_str()).collect();

    for r in results {
        let f = r.file_path.as_str();

        if config.priority_signals.contains(&PrioritySignal::Explicit)
            && (explicit_set.contains(f)
                || explicit_set.iter().any(|m| r.symbol_name.contains(*m)))
        {
            r.score *= 3.0; // Biggest boost for explicit
            r.source = format!("{}+explicit", r.source);
        }

        if config.priority_signals.contains(&PrioritySignal::OpenFiles) && open_set.contains(f) {
            r.score *= 2.0;
            r.source = format!("{}+open", r.source);
        }

        if config.priority_signals.contains(&PrioritySignal::Recency) && recent_set.contains(f) {
            r.score *= 1.5;
            r.source = format!("{}+recent", r.source);
        }

        // Current file gets a moderate boost
        if let Some(ref cf) = signals.current_file {
            if f == cf.as_str() {
                r.score *= 1.3;
                r.source = format!("{}+current_file", r.source);
            }
        }
    }
}

/// 应用 Token 预算
///
/// 【领域含义】根据配置的最大 Token 数截断结果集。先按评分降序排序，然后逐个估算
/// 每个结果的字符消耗，超出预算则丢弃。返回选中的符号键集合 (file_path, symbol_name)。
pub(crate) fn apply_token_budget(
    config: &ContextConfig,
    results: &mut Vec<ScoredResult>,
) -> HashSet<(String, String)> {
    // Sort by score descending
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let max_chars = config.max_context_tokens * CHARS_PER_TOKEN;
    let mut used = 0;
    let mut selected: HashSet<(String, String)> = HashSet::new();
    let mut kept = Vec::new();

    for r in results.drain(..) {
        // Estimate characters this result would consume (header + content guess)
        let estimate = 100 + r.symbol_name.len() + r.file_path.len();
        if used + estimate > max_chars && !selected.is_empty() {
            // Drop this result — budget exceeded
            continue;
        }
        used += estimate;
        let key = (r.file_path.clone(), r.symbol_name.clone());
        selected.insert(key);
        kept.push(r);
    }

    *results = kept;
    selected
}

/// 格式化上下文为 XML
///
/// 【领域含义】将选中的检索结果格式化为结构化的 XML 提示块。按文件分组生成
/// `<file>` 块（包含代码片段），如果启用了图谱扩展且有调用图可用，则追加
/// `<call-graph>` 部分展示调用关系。
pub(crate) fn format_context(
    config: &ContextConfig,
    graph: Option<&CallGraph>,
    results: &[ScoredResult],
    _selected: &HashSet<(String, String)>,
) -> String {
    let mut buf = String::with_capacity(8192);
    buf.push_str("<context>\n");

    // Group by file
    let mut file_groups: HashMap<&str, Vec<&ScoredResult>> = HashMap::new();
    for r in results {
        file_groups.entry(r.file_path.as_str()).or_default().push(r);
    }

    // Sort files alphabetically for deterministic output
    let mut file_paths: Vec<&&str> = file_groups.keys().collect();
    file_paths.sort();

    for file_path in file_paths {
        let entries = &file_groups[file_path];
        if entries.is_empty() {
            continue;
        }

        // Determine the union of line ranges to read
        let line_start = entries
            .iter()
            .map(|e| e.line_range.start.saturating_sub(1))
            .min()
            .unwrap_or(1);
        let line_end = entries
            .iter()
            .map(|e| e.line_range.end + 2) // +2 lines of context
            .max()
            .unwrap_or(1);

        buf.push_str(&format!(
            "<file path=\"{}\" lines=\"{}-{}\">\n",
            file_path, line_start, line_end,
        ));

        // Read the actual source snippet
        if let Ok(content) = std::fs::read_to_string(file_path) {
            let lines: Vec<&str> = content.lines().collect();
            let start_idx = line_start.saturating_sub(1);
            let end_idx = line_end.min(lines.len());
            for (i, line) in lines[start_idx..end_idx].iter().enumerate() {
                let line_no = start_idx + i + 1;
                // Annotate lines that belong to a known symbol
                for entry in entries.iter() {
                    if entry.line_range.start <= line_no && line_no <= entry.line_range.end {
                        buf.push_str(&format!(
                            "// Function: {}\n// Signature: {}\n",
                            entry.symbol_name, entry.symbol_kind,
                        ));
                    }
                }
                buf.push_str(line);
                buf.push('\n');
            }
        }

        buf.push_str("</file>\n\n");
    }

    // Call-graph section
    if config.enable_graph_expansion {
        if let Some(g) = graph {
            buf.push_str("<call-graph>\n");
            let mut seen_edges: HashSet<String> = HashSet::new();

            for r in results {
                // Callers (who calls this symbol)
                for edge in g.get_callers(&r.symbol_name) {
                    let entry = format!(
                        "  └─ called by: {}::{} ({}:{})",
                        edge.caller_file, edge.caller_symbol,
                        edge.caller_file, edge.call_line,
                    );
                    if seen_edges.insert(entry.clone()) {
                        buf.push_str(&format!(
                            "{} ({}:{})\n",
                            r.symbol_name,
                            r.file_path, r.line_range.start,
                        ));
                        buf.push_str(&entry);
                        buf.push('\n');
                    }
                }

                // Callees (what this symbol calls)
                for edge in g.get_callees(&r.symbol_name) {
                    let entry = format!(
                        "  └─ calls: {} ({}:{})",
                        edge.callee_symbol,
                        edge.callee_file, edge.call_line,
                    );
                    if seen_edges.insert(entry.clone()) {
                        buf.push_str(&format!(
                            "{} ({}:{})\n",
                            r.symbol_name,
                            r.file_path, r.line_range.start,
                        ));
                        buf.push_str(&entry);
                        buf.push('\n');
                    }
                }
            }
            buf.push_str("</call-graph>\n");
        }
    }

    buf.push_str("</context>\n");
    buf
}

/// 估算 Token 数
///
/// 【领域含义】根据字符数粗略估算 Token 数。使用启发式规则：1 Token ≈ 4 字符
/// （适用于英文文本/代码）。
#[allow(dead_code)]
pub fn estimate_tokens(chars: usize) -> usize {
    chars / CHARS_PER_TOKEN
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{CallEdge, EdgeConfidence};
    use std::ops::Range;

    fn make_result(
        name: &str,
        file: &str,
        score: f64,
        source: &str,
        line_range: Range<usize>,
    ) -> ScoredResult {
        ScoredResult {
            symbol_name: name.to_string(),
            symbol_kind: "function".to_string(),
            file_path: file.to_string(),
            line_range,
            score,
            source: source.to_string(),
        }
    }

    // ── Priority signal tests ───────────────────────────────────────────

    #[test]
    fn test_apply_signals_recency_boost() {
        let config = ContextConfig::default();
        let mut results = vec![
            make_result("foo", "src/main.rs", 0.5, "bm25", 1..5),
            make_result("bar", "src/lib.rs", 0.5, "bm25", 10..15),
        ];

        let signals = ContextSignals {
            recently_edited: vec!["src/main.rs".to_string()],
            ..Default::default()
        };

        apply_signals(&config, &mut results, &signals);

        // main.rs result should be boosted (0.5 * 1.5 = 0.75)
        assert!(results[0].score > results[1].score);
        assert!((results[0].score - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_apply_signals_explicit_boost() {
        let config = ContextConfig::default();
        let mut results = vec![
            make_result("validateToken", "src/auth.rs", 0.5, "bm25", 1..10),
        ];

        let signals = ContextSignals {
            explicit_mentions: vec!["validateToken".to_string()],
            ..Default::default()
        };

        apply_signals(&config, &mut results, &signals);

        // Explicit mention gets 3x boost: 0.5 * 3.0 = 1.5
        assert!((results[0].score - 1.5).abs() < 0.01);
        assert!(results[0].source.contains("explicit"));
    }

    #[test]
    fn test_apply_signals_open_files_boost() {
        let config = ContextConfig::default();
        let mut results = vec![
            make_result("handler", "src/api.rs", 0.5, "vector", 5..8),
        ];

        let signals = ContextSignals {
            open_files: vec!["src/api.rs".to_string()],
            ..Default::default()
        };

        apply_signals(&config, &mut results, &signals);

        // Open file gets 2x: 0.5 * 2.0 = 1.0
        assert!((results[0].score - 1.0).abs() < 0.01);
        assert!(results[0].source.contains("open"));
    }

    #[test]
    fn test_apply_signals_current_file_boost() {
        let config = ContextConfig::default();
        let mut results = vec![
            make_result("parse", "src/parser.rs", 0.5, "bm25", 3..7),
        ];

        let signals = ContextSignals {
            current_file: Some("src/parser.rs".to_string()),
            ..Default::default()
        };

        apply_signals(&config, &mut results, &signals);

        // Current file gets 1.3x: 0.5 * 1.3 ≈ 0.65
        assert!((results[0].score - 0.65).abs() < 0.001);
        assert!(results[0].source.contains("current_file"));
    }

    #[test]
    fn test_apply_signals_no_priority_signals() {
        let config = ContextConfig {
            priority_signals: vec![],
            ..ContextConfig::default()
        };
        let mut results = vec![
            make_result("foo", "src/main.rs", 0.5, "bm25", 1..5),
        ];
        let signals = ContextSignals {
            recently_edited: vec!["src/main.rs".to_string()],
            ..Default::default()
        };

        apply_signals(&config, &mut results, &signals);

        // No change since no priority signals are configured
        assert!((results[0].score - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_apply_signals_combined() {
        let config = ContextConfig::default();
        let mut results = vec![
            make_result("auth_handler", "src/auth.rs", 0.5, "bm25", 1..5),
        ];

        let signals = ContextSignals {
            recently_edited: vec!["src/auth.rs".to_string()],
            open_files: vec!["src/auth.rs".to_string()],
            explicit_mentions: vec!["auth".to_string()],
            ..Default::default()
        };

        apply_signals(&config, &mut results, &signals);

        // Explicit (3x) + Open (2x) + Recency (1.5x) = 0.5 * 3.0 * 2.0 * 1.5 = 4.5
        assert!((results[0].score - 4.5).abs() < 0.01);
    }

    // ── Token budget tests ──────────────────────────────────────────────

    #[test]
    fn test_token_budget_enforcement() {
        let config = ContextConfig {
            max_context_tokens: 50, // Very tight budget (~200 chars)
            ..ContextConfig::default()
        };

        let mut results: Vec<ScoredResult> = (0..20)
            .map(|i| {
                make_result(
                    &format!("func_{}", i),
                    &format!("src/file_{}.rs", i % 5),
                    (20 - i) as f64 * 0.05,
                    "bm25",
                    (i * 5)..(i * 5 + 3),
                )
            })
            .collect();

        let selected = apply_token_budget(&config, &mut results);

        // After budget, we should have fewer results
        assert!(results.len() < 20, "Budget should have trimmed results");
        assert!(!results.is_empty(), "Should keep at least 1 result");

        // Selected set should match kept results
        for r in &results {
            assert!(selected.contains(&(r.file_path.clone(), r.symbol_name.clone())));
        }
    }

    #[test]
    fn test_token_budget_keeps_top_scorers() {
        let config = ContextConfig {
            max_context_tokens: 100,
            ..ContextConfig::default()
        };

        let mut results = vec![
            make_result("low", "f.rs", 0.1, "bm25", 1..2),
            make_result("high", "f.rs", 0.9, "bm25", 1..2),
            make_result("mid", "f.rs", 0.5, "bm25", 1..2),
        ];

        let _ = apply_token_budget(&config, &mut results);

        // Should be sorted by score descending
        assert_eq!(results[0].symbol_name, "high");
        assert_eq!(results[1].symbol_name, "mid");
        assert_eq!(results[2].symbol_name, "low");
    }

    #[test]
    fn test_token_budget_empty_results() {
        let config = ContextConfig::default();
        let mut results: Vec<ScoredResult> = vec![];
        let selected = apply_token_budget(&config, &mut results);

        assert!(results.is_empty());
        assert!(selected.is_empty());
    }

    // ── Formatting tests ────────────────────────────────────────────────

    #[test]
    fn test_format_context_structure() {
        let config = ContextConfig::default();
        let results = vec![
            make_result("validate", "src/auth.rs", 0.9, "hybrid", 5..10),
        ];
        let mut selected = HashSet::new();
        selected.insert(("src/auth.rs".to_string(), "validate".to_string()));

        let formatted = format_context(&config, None, &results, &selected);

        assert!(formatted.contains("<context>"));
        assert!(formatted.contains("</context>"));
        assert!(formatted.contains("<file path="));
        assert!(formatted.contains("</file>"));
    }

    #[test]
    fn test_format_context_empty_results() {
        let config = ContextConfig::default();
        let formatted = format_context(&config, None, &[], &HashSet::new());

        assert!(formatted.contains("<context>"));
        assert!(formatted.contains("</context>"));
        assert!(!formatted.contains("<file"));
    }

    #[test]
    fn test_format_context_groups_by_file() {
        let config = ContextConfig::default();
        let results = vec![
            make_result("func_a", "src/x.rs", 0.8, "vector", 10..12),
            make_result("func_b", "src/x.rs", 0.6, "bm25", 20..22),
            make_result("func_c", "src/y.rs", 0.4, "bm25", 5..7),
        ];
        let mut selected = HashSet::new();
        selected.insert(("src/x.rs".to_string(), "func_a".to_string()));
        selected.insert(("src/x.rs".to_string(), "func_b".to_string()));
        selected.insert(("src/y.rs".to_string(), "func_c".to_string()));

        let formatted = format_context(&config, None, &results, &selected);

        // Both func_a and func_b should be under the same file block
        let x_idx = formatted.find("src/x.rs").unwrap();
        let y_idx = formatted.find("src/y.rs").unwrap();
        assert!(x_idx < y_idx, "Files should appear in alphabetical order");
    }

    #[test]
    fn test_format_context_with_call_graph() {
        let config = ContextConfig {
            enable_graph_expansion: true,
            ..ContextConfig::default()
        };

        let mut cg = CallGraph::new();
        cg.add_edge(CallEdge {
            caller_file: "src/main.rs".into(),
            caller_symbol: "main".into(),
            callee_file: "src/auth.rs".into(),
            callee_symbol: "validate".into(),
            call_line: 10,
            is_dynamic: false,
            confidence: EdgeConfidence::Exact,
        });

        let results = vec![
            make_result("validate", "src/auth.rs", 0.9, "hybrid", 5..10),
        ];
        let mut selected = HashSet::new();
        selected.insert(("src/auth.rs".to_string(), "validate".to_string()));

        let formatted = format_context(&config, Some(&cg), &results, &selected);

        assert!(formatted.contains("<call-graph>"));
        assert!(formatted.contains("</call-graph>"));
        assert!(formatted.contains("main"));
    }

    #[test]
    fn test_format_context_call_graph_disabled() {
        let config = ContextConfig {
            enable_graph_expansion: false,
            ..ContextConfig::default()
        };

        let mut cg = CallGraph::new();
        cg.add_edge(CallEdge {
            caller_file: "src/main.rs".into(),
            caller_symbol: "main".into(),
            callee_file: "src/auth.rs".into(),
            callee_symbol: "validate".into(),
            call_line: 10,
            is_dynamic: false,
            confidence: EdgeConfidence::Exact,
        });

        let results = vec![
            make_result("validate", "src/auth.rs", 0.9, "hybrid", 5..10),
        ];
        let mut selected = HashSet::new();
        selected.insert(("src/auth.rs".to_string(), "validate".to_string()));

        let formatted = format_context(&config, Some(&cg), &results, &selected);

        // No call-graph section when disabled
        assert!(!formatted.contains("<call-graph>"));
    }

    // ── Graph expansion tests ───────────────────────────────────────────

    #[test]
    fn test_expand_graph_without_graph() {
        let mut results = vec![
            make_result("main", "src/main.rs", 0.9, "bm25", 1..5),
        ];

        let added = expand_graph(None, &mut results);
        assert_eq!(added, 0);
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_expand_graph_adds_callers() {
        let mut cg = CallGraph::new();
        cg.add_edge(CallEdge {
            caller_file: "src/main.rs".into(),
            caller_symbol: "main".into(),
            callee_file: "src/util.rs".into(),
            callee_symbol: "helper".into(),
            call_line: 10,
            is_dynamic: false,
            confidence: EdgeConfidence::Exact,
        });

        let mut results = vec![
            make_result("helper", "src/util.rs", 0.9, "bm25", 5..10),
        ];

        let added = expand_graph(Some(&cg), &mut results);

        assert!(added > 0, "Should have added caller(s)");
        assert!(results.len() > 1);
        assert!(
            results.iter().any(|r| r.symbol_name == "main"),
            "main should be added as a caller of helper"
        );
    }

    #[test]
    fn test_expand_graph_adds_callees() {
        let mut cg = CallGraph::new();
        cg.add_edge(CallEdge {
            caller_file: "src/main.rs".into(),
            caller_symbol: "main".into(),
            callee_file: "src/util.rs".into(),
            callee_symbol: "helper".into(),
            call_line: 10,
            is_dynamic: false,
            confidence: EdgeConfidence::Exact,
        });

        let mut results = vec![
            make_result("main", "src/main.rs", 0.9, "bm25", 1..5),
        ];

        let added = expand_graph(Some(&cg), &mut results);

        assert!(added > 0, "Should have added callee(s)");
        assert!(
            results.iter().any(|r| r.symbol_name == "helper"),
            "helper should be added as a callee of main"
        );
    }

    #[test]
    fn test_expand_graph_no_duplicates() {
        let mut cg = CallGraph::new();
        cg.add_edge(CallEdge {
            caller_file: "src/a.rs".into(),
            caller_symbol: "caller".into(),
            callee_file: "src/b.rs".into(),
            callee_symbol: "target".into(),
            call_line: 1,
            is_dynamic: false,
            confidence: EdgeConfidence::Exact,
        });

        // Target is already in results
        let mut results = vec![
            make_result("target", "src/b.rs", 0.9, "bm25", 1..5),
            make_result("caller", "src/a.rs", 0.5, "bm25", 1..5),
        ];

        let before = results.len();
        let _added = expand_graph(Some(&cg), &mut results);

        // Should not add duplicates — caller is already present
        assert_eq!(results.len(), before, "No duplicates expected");
    }

    // ── Utility tests ───────────────────────────────────────────────────

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(4), 1);
        assert_eq!(estimate_tokens(100), 25);
        assert_eq!(estimate_tokens(8196), 2049);
    }
}
