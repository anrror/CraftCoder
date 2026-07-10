//! Query methods for [`CallGraph`] and [`DependencyGraph`].
//!
//! Provides caller/callee lookups, impact analysis, and dependency traversal.

use std::collections::{HashSet, VecDeque};

use super::{CallEdge, CallGraph, DependencyGraph, ImpactResult};

// ── CallGraph queries ─────────────────────────────────────────────────────

impl CallGraph {
    /// 获取调用方
    ///
    /// 【领域含义】查询调用指定符号的所有调用方（即该符号作为被调用方的边）。
    /// symbol 参数为被调用方名称，支持 "file::symbol" 完整键格式或仅符号名称的跨文件搜索。
    pub fn get_callers(&self, symbol: &str) -> Vec<CallEdge> {
        // Try exact key match first
        if let Some(edges) = self.callers.get(symbol) {
            return edges.clone();
        }
        // Try matching by symbol name suffix
        let suffix = format!("::{}", symbol);
        for (key, edges) in &self.callers {
            if key.ends_with(&suffix) || key == symbol {
                return edges.clone();
            }
        }
        Vec::new()
    }

    /// 获取被调用方
    ///
    /// 【领域含义】查询指定符号调用的所有被调用方（即该符号作为调用方的边）。
    /// 匹配策略与 get_callers 相同。
    pub fn get_callees(&self, symbol: &str) -> Vec<CallEdge> {
        // Try exact key match first
        if let Some(edges) = self.callees.get(symbol) {
            return edges.clone();
        }
        // Try matching by symbol name suffix
        let suffix = format!("::{}", symbol);
        for (key, edges) in &self.callees {
            if key.ends_with(&suffix) || key == symbol {
                return edges.clone();
            }
        }
        Vec::new()
    }

    /// 影响分析
    ///
    /// 【领域含义】执行变更影响分析：如果指定符号发生变化，哪些其他符号会受影响？
    /// 使用 BFS 遍历调用图，返回分层结果：
    /// - direct_callers（深度 1）：直接调用目标符号的符号
    /// - indirect_callers（深度 2+）：传递依赖的符号
    /// - affected_files：包含受影响符号的唯一文件列表
    /// - total_impact_count：唯一调用方符号总数
    /// depth 参数限制 BFS 遍历深度（0 表示不限）。
    pub fn get_impact(&self, symbol: &str, depth: u32) -> ImpactResult {
        // Find the initial caller key for this callee
        let start_keys: Vec<String> = if self.callers.contains_key(symbol) {
            vec![symbol.to_string()]
        } else {
            // Try matching by suffix
            let suffix = format!("::{}", symbol);
            self.callers
                .keys()
                .filter(|k| k.ends_with(&suffix) || k.as_str() == symbol)
                .cloned()
                .collect()
        };

        if start_keys.is_empty() {
            return ImpactResult::default();
        }

        let mut direct: Vec<String> = Vec::new();
        let mut indirect: Vec<String> = Vec::new();
        let mut affected_files: HashSet<String> = HashSet::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<(String, u32)> = VecDeque::new();

        // Seed the BFS with initial callee and its callers
        for key in &start_keys {
            if let Some(edges) = self.callers.get(key) {
                for edge in edges {
                    let caller_key =
                        super::make_symbol_key(&edge.caller_file, &edge.caller_symbol);
                    if !visited.contains(&caller_key) {
                        visited.insert(caller_key.clone());
                        direct.push(edge.caller_symbol.clone());
                        affected_files.insert(edge.caller_file.clone());
                        if depth == 0 || 1 < depth {
                            queue.push_back((caller_key, 1));
                        }
                    }
                }
            }
        }

        // BFS for indirect callers
        while let Some((current_key, current_depth)) = queue.pop_front() {
            if depth > 0 && current_depth >= depth {
                continue;
            }

            if let Some(edges) = self.callers.get(&current_key) {
                for edge in edges {
                    let caller_key =
                        super::make_symbol_key(&edge.caller_file, &edge.caller_symbol);
                    if !visited.contains(&caller_key) {
                        visited.insert(caller_key.clone());
                        indirect.push(edge.caller_symbol.clone());
                        affected_files.insert(edge.caller_file.clone());
                        queue.push_back((caller_key, current_depth + 1));
                    }
                }
            }
        }

        ImpactResult {
            direct_callers: direct,
            indirect_callers: indirect,
            affected_files: affected_files.into_iter().collect(),
            total_impact_count: visited.len(),
        }
    }
}

// ── DependencyGraph queries ───────────────────────────────────────────────

impl DependencyGraph {
    /// 获取依赖方
    ///
    /// 【领域含义】查询哪些文件依赖于（导入）指定文件。回答"谁导入了我？"的问题。
    pub fn get_dependents(&self, file: &str) -> Vec<String> {
        self.imports
            .iter()
            .filter(|(_, targets)| targets.iter().any(|t| t == file))
            .map(|(source, _)| source.clone())
            .collect()
    }

    /// 获取依赖项
    ///
    /// 【领域含义】查询指定文件依赖（导入）了哪些文件。回答"我导入了什么？"的问题。
    pub fn get_dependencies(&self, file: &str) -> Vec<String> {
        self.imports
            .get(file)
            .cloned()
            .unwrap_or_default()
    }

    /// 获取文件依赖数
    ///
    /// 【领域含义】返回指定文件的导入依赖总数。
    pub fn dependency_count(&self, file: &str) -> usize {
        self.imports
            .get(file)
            .map(|v| v.len())
            .unwrap_or(0)
    }
}

// ── Utility trait impls ───────────────────────────────────────────────────

/// 影响分析结果展示
///
/// 【领域含义】提供 ImpactResult 的格式化输出，显示直接调用方数、间接调用方数、
/// 受影响文件数和总影响符号数。
impl std::fmt::Display for ImpactResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ImpactResult {{ direct: {}, indirect: {}, files: {}, total: {} }}",
            self.direct_callers.len(),
            self.indirect_callers.len(),
            self.affected_files.len(),
            self.total_impact_count,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::EdgeConfidence;

    fn make_edge(caller_file: &str, caller_sym: &str, callee_file: &str, callee_sym: &str) -> CallEdge {
        CallEdge {
            caller_file: caller_file.to_string(),
            caller_symbol: caller_sym.to_string(),
            callee_file: callee_file.to_string(),
            callee_symbol: callee_sym.to_string(),
            call_line: 1,
            is_dynamic: false,
            confidence: EdgeConfidence::Exact,
        }
    }

    fn build_test_graph() -> CallGraph {
        //     main → helper → compute
        //     main → init
        //     app  → helper
        let mut g = CallGraph::new();
        g.add_edge(make_edge("main.rs", "main", "util.rs", "helper"));
        g.add_edge(make_edge("util.rs", "helper", "core.rs", "compute"));
        g.add_edge(make_edge("main.rs", "main", "setup.rs", "init"));
        g.add_edge(make_edge("app.rs", "run", "util.rs", "helper"));
        g
    }

    #[test]
    fn test_get_callers() {
        let g = build_test_graph();

        // helper is called by main and run
        let callers = g.get_callers("helper");
        let caller_names: Vec<&str> = callers.iter().map(|e| e.caller_symbol.as_str()).collect();
        assert!(caller_names.contains(&"main"));
        assert!(caller_names.contains(&"run"));

        // compute is called by helper
        let callers = g.get_callers("compute");
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].caller_symbol, "helper");

        // unknown symbol has no callers
        let callers = g.get_callers("nonexistent");
        assert!(callers.is_empty());
    }

    #[test]
    fn test_get_callees() {
        let g = build_test_graph();

        // main calls helper and init
        let callees = g.get_callees("main");
        let callee_names: Vec<&str> = callees.iter().map(|e| e.callee_symbol.as_str()).collect();
        assert_eq!(callee_names.len(), 2);
        assert!(callee_names.contains(&"helper"));
        assert!(callee_names.contains(&"init"));

        // helper calls compute
        let callees = g.get_callees("helper");
        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0].callee_symbol, "compute");
    }

    #[test]
    fn test_get_impact() {
        let g = build_test_graph();

        // Changing compute should affect: helper (direct), main + run (indirect via helper)
        let impact = g.get_impact("compute", 0); // depth 0 = unlimited
        assert!(
            impact.direct_callers.contains(&"helper".to_string()),
            "helper should be a direct caller, got: {:?}",
            impact.direct_callers
        );
        assert!(
            impact.indirect_callers.contains(&"main".to_string()),
            "main should be an indirect caller, got: {:?}",
            impact.indirect_callers
        );
        assert!(
            impact.indirect_callers.contains(&"run".to_string()),
            "run should be an indirect caller, got: {:?}",
            impact.indirect_callers
        );
        assert!(impact.total_impact_count >= 3);
        // affected files should include util.rs and the files that imported it
        let files: HashSet<&str> = impact.affected_files.iter().map(|s| s.as_str()).collect();
        assert!(files.contains("main.rs"));
        assert!(files.contains("app.rs"));
    }

    #[test]
    fn test_get_impact_depth_limited() {
        let g = build_test_graph();

        // Depth 1: only direct callers of helper
        let impact = g.get_impact("helper", 1);
        let dir_names: Vec<&str> = impact.direct_callers.iter().map(|s| s.as_str()).collect();
        assert!(dir_names.contains(&"main"));
        assert!(dir_names.contains(&"run"));
        assert!(
            impact.indirect_callers.is_empty(),
            "Depth 1 should have no indirect callers"
        );
    }

    #[test]
    fn test_dependency_queries() {
        let mut dg = DependencyGraph::new();
        dg.add_import("main.py".to_string(), "util.py".to_string());
        dg.add_import("main.py".to_string(), "config.py".to_string());
        dg.add_import("app.py".to_string(), "util.py".to_string());

        // Who depends on util.py?
        let dependents = dg.get_dependents("util.py");
        assert_eq!(dependents.len(), 2);
        assert!(dependents.contains(&"main.py".to_string()));
        assert!(dependents.contains(&"app.py".to_string()));

        // What does main.py depend on?
        let deps = dg.get_dependencies("main.py");
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&"util.py".to_string()));
        assert!(deps.contains(&"config.py".to_string()));

        // Empty for files not in graph
        let no_deps = dg.get_dependents("nonexistent.py");
        assert!(no_deps.is_empty());

        assert_eq!(dg.dependency_count("main.py"), 2);
    }
}
