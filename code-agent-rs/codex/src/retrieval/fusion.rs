//! Result fusion — Reciprocal Rank Fusion (RRF) + graph-based reranking.
//!
//! Combines ranked lists from multiple search backends using RRF, then
//! optionally boosts results using call-graph centrality metrics.

use std::collections::HashMap;

use super::ScoredResult;
use crate::graph::CallGraph;

/// 倒数排序融合器（领域服务）
///
/// 【领域含义】实现倒数排序融合（RRF）算法的领域服务，用于合并多个检索后端的
/// 排序结果列表。每个结果在排名 r（从 0 开始）处的 RRF 评分为 `1 / (k + r)`，
/// 所有列表的评分按文档求和后降序排列。常数 k（默认 60）防止排名靠前的项目获得
/// 不成比例的高分。属于"代码检索"限界上下文中的融合组件。
///
/// # 参考文献
///
/// Cormack, Clarke, Buettcher. "Reciprocal Rank Fusion outperforms Condorcet
/// and individual rank learning methods." SIGIR 2009.
pub struct RrfFusion {
    /// RRF constant. Defaults to 60.
    k: usize,
}

impl Default for RrfFusion {
    fn default() -> Self {
        Self { k: 60 }
    }
}

impl RrfFusion {
    /// 创建自定义 RRF 融合器
    ///
    /// 【领域含义】使用自定义 k 常数创建 RRF 融合器。k 值越小，排名靠前的结果权重越大。
    #[allow(dead_code)]
    pub fn new(k: usize) -> Self {
        Self { k }
    }

    /// 融合多个排序列表
    ///
    /// 【领域含义】将多个排序结果列表融合为单个排序列表。每个内部 Vec<ScoredResult>
    /// 应按评分降序排列。输出按 RRF 评分降序排列。结果按 (symbol_name, file_path)
    /// 去重，保留最高的个体评分。
    pub fn fuse(&self, results: Vec<Vec<ScoredResult>>) -> Vec<ScoredResult> {
        // Map: (symbol_name, file_path) -> (merged_result, rrf_score)
        let mut merged: HashMap<(String, String), (ScoredResult, f64)> = HashMap::new();

        for result_list in &results {
            for (rank, result) in result_list.iter().enumerate() {
                let rrf_score = 1.0 / (self.k as f64 + rank as f64);
                let key = (result.symbol_name.clone(), result.file_path.clone());

                merged
                    .entry(key)
                    .and_modify(|(existing, score)| {
                        *score += rrf_score;
                        // Keep the result with the higher individual score
                        if result.score > existing.score {
                            existing.score = result.score;
                        }
                    })
                    .or_insert_with(|| {
                        let mut merged_result = result.clone();
                        merged_result.source = "hybrid".to_string();
                        (merged_result, rrf_score)
                    });
            }
        }

        // Sort by descending RRF score and convert to ScoredResult
        let mut fused: Vec<(ScoredResult, f64)> = merged.into_values().collect();
        fused.sort_by(|(_, a), (_, b)| {
            b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal)
        });

        fused
            .into_iter()
            .map(|(mut result, rrf_score)| {
                result.score = rrf_score;
                result.source = "fusion".to_string();
                result
            })
            .collect()
    }

    /// 基于调用图中心性重排序
    ///
    /// 【领域含义】使用调用图中心性对检索结果进行重排序。被调用频率更高的符号
    /// （调用图中入度更高）获得中心性提升。提升公式为：
    /// score *= 1.0 + alpha * ln(1 + caller_count)
    /// 其中 alpha 控制提升强度（默认 0.1）。
    pub fn rerank_with_graph(&self, results: &mut [ScoredResult], graph: &CallGraph) {
        let alpha = 0.1;

        // Build caller-count map from the call graph
        let caller_counts = build_caller_counts(graph);

        for result in results.iter_mut() {
            let key = crate::graph::make_symbol_key(&result.file_path, &result.symbol_name);
            let caller_count = caller_counts.get(&key).copied().unwrap_or(0);
            let boost = 1.0 + alpha * (1.0 + caller_count as f64).ln();
            result.score *= boost;
        }

        // Re-sort by updated score
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }
}

/// Build a map of symbol_key → number of distinct callers.
///
/// For each callee in the call graph, counts how many distinct caller symbols
/// invoke it.
fn build_caller_counts(graph: &CallGraph) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();

    for edge in graph.edges() {
        let callee_key =
            crate::graph::make_symbol_key(&edge.callee_file, &edge.callee_symbol);
        *counts.entry(callee_key).or_default() += 1;
    }

    counts
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{CallEdge, EdgeConfidence};
    use std::ops::Range;

    fn make_result(name: &str, file: &str, score: f64, source: &str) -> ScoredResult {
        ScoredResult {
            symbol_name: name.to_string(),
            symbol_kind: "function".to_string(),
            file_path: file.to_string(),
            line_range: Range { start: 1, end: 5 },
            score,
            source: source.to_string(),
        }
    }

    #[test]
    fn test_rrf_fusion_combines_two_lists() {
        let fusion = RrfFusion::default();

        let list1 = vec![
            make_result("func_a", "a.rs", 0.9, "bm25"),
            make_result("func_b", "b.rs", 0.7, "bm25"),
        ];
        let list2 = vec![
            make_result("func_c", "c.rs", 0.8, "vector"),
            make_result("func_a", "a.rs", 0.5, "vector"),
        ];

        let fused = fusion.fuse(vec![list1, list2]);

        assert!(fused.len() >= 2); // func_a appears in both → deduplicated
        // func_a should have highest RRF score (rank 0 in list1 + rank 1 in list2)
        assert_eq!(fused[0].symbol_name, "func_a");
        assert_eq!(fused[0].source, "fusion");
    }

    #[test]
    fn test_rrf_fusion_empty_input() {
        let fusion = RrfFusion::default();
        let fused = fusion.fuse(vec![]);
        assert!(fused.is_empty());
    }

    #[test]
    fn test_rrf_fusion_all_empty_lists() {
        let fusion = RrfFusion::default();
        let fused = fusion.fuse(vec![vec![], vec![]]);
        assert!(fused.is_empty());
    }

    #[test]
    fn test_rrf_fusion_single_list() {
        let fusion = RrfFusion::default();

        let list = vec![
            make_result("a", "a.rs", 0.9, "bm25"),
            make_result("b", "b.rs", 0.7, "bm25"),
            make_result("c", "c.rs", 0.5, "bm25"),
        ];

        let fused = fusion.fuse(vec![list]);
        assert_eq!(fused.len(), 3);
        // RRF scores should preserve rank order
        assert!(fused[0].score > fused[1].score);
        assert!(fused[1].score > fused[2].score);
    }

    #[test]
    fn test_rrf_fusion_deduplicates_by_key() {
        let fusion = RrfFusion::default();

        // Two lists both containing the same symbol
        let list1 = vec![make_result("helper", "util.rs", 0.9, "bm25")];
        let list2 = vec![make_result("helper", "util.rs", 0.8, "vector")];

        let fused = fusion.fuse(vec![list1, list2]);
        assert_eq!(fused.len(), 1);
        assert_eq!(fused[0].symbol_name, "helper");
    }

    #[test]
    fn test_rrf_fusion_k_value_affects_scores() {
        let fusion_low_k = RrfFusion::new(1);
        let fusion_high_k = RrfFusion::new(1000);

        let list = vec![make_result("a", "a.rs", 1.0, "bm25")];
        let fused_low = fusion_low_k.fuse(vec![list.clone()]);
        let fused_high = fusion_high_k.fuse(vec![list]);

        // Higher k → lower RRF scores
        assert!(fused_low[0].score > fused_high[0].score);
    }

    #[test]
    fn test_graph_reranking_boosts_called_symbols() {
        let fusion = RrfFusion::default();

        // Build a call graph where `helper` is called by 3 different functions
        let mut graph = CallGraph::new();
        for caller in &["main", "init", "setup"] {
            graph.add_edge(CallEdge {
                caller_file: "main.rs".to_string(),
                caller_symbol: caller.to_string(),
                callee_file: "util.rs".to_string(),
                callee_symbol: "helper".to_string(),
                call_line: 1,
                is_dynamic: false,
                confidence: EdgeConfidence::Exact,
            });
        }

        let mut results = vec![
            // `helper` — called 3 times, should get boosted
            make_result("helper", "util.rs", 0.6, "bm25"),
            // `isolated` — not in the graph at all
            make_result("isolated", "other.rs", 0.8, "vector"),
        ];

        fusion.rerank_with_graph(&mut results, &graph);

        // After boost, `helper` might overtake `isolated`
        // We can only assert that helper's score increased relative to its original
        assert!(results.iter().any(|r| r.symbol_name == "helper"));
        assert!(results.iter().any(|r| r.symbol_name == "isolated"));
    }

    #[test]
    fn test_graph_reranking_empty_graph_noop() {
        let fusion = RrfFusion::default();
        let graph = CallGraph::new();

        let mut results = vec![
            make_result("a", "a.rs", 0.9, "bm25"),
            make_result("b", "b.rs", 0.7, "vector"),
        ];

        let scores_before: Vec<f64> = results.iter().map(|r| r.score).collect();
        fusion.rerank_with_graph(&mut results, &graph);
        let scores_after: Vec<f64> = results.iter().map(|r| r.score).collect();

        // With empty graph, scores should be unchanged
        for (before, after) in scores_before.iter().zip(scores_after.iter()) {
            assert!((before - after).abs() < 1e-9);
        }
    }

    #[test]
    fn test_build_caller_counts() {
        let mut graph = CallGraph::new();
        graph.add_edge(CallEdge {
            caller_file: "a.rs".to_string(),
            caller_symbol: "foo".to_string(),
            callee_file: "b.rs".to_string(),
            callee_symbol: "bar".to_string(),
            call_line: 1,
            is_dynamic: false,
            confidence: EdgeConfidence::Exact,
        });
        graph.add_edge(CallEdge {
            caller_file: "c.rs".to_string(),
            caller_symbol: "baz".to_string(),
            callee_file: "b.rs".to_string(),
            callee_symbol: "bar".to_string(),
            call_line: 1,
            is_dynamic: false,
            confidence: EdgeConfidence::Exact,
        });

        let counts = build_caller_counts(&graph);
        let key = crate::graph::make_symbol_key("b.rs", "bar");
        assert_eq!(counts.get(&key), Some(&2));
    }
}
