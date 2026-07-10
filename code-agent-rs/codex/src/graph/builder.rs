//! Graph construction from the AST symbol index.
//!
//! The [`GraphBuilder`] re-parses indexed source files to discover call sites
//! and import relationships, then resolves them against the symbol index to
//! produce a [`CallGraph`] and [`DependencyGraph`].

use std::collections::{HashMap, HashSet};
use std::path::Path;

use log::warn;
use tree_sitter::{Node, Parser};

use crate::indexer::{CodeIndexer, IndexerError};
use crate::indexer::parser::CodeParser;
use crate::indexer::symbol::{SymbolEntry, SymbolKind};

use super::{CallEdge, CallGraph, DependencyGraph, EdgeConfidence, GraphError};

/// 图谱构建器（领域服务）
///
/// 【领域含义】从已有符号索引构建调用图和依赖图的领域服务。重新解析已索引的源文件
/// 以发现调用表达式和导入关系，然后对照符号索引解析被调用方，生成 CallGraph 和
/// DependencyGraph。属于"代码图谱"限界上下文的核心领域服务。
pub struct GraphBuilder {
    indexer: CodeIndexer,
    parser: CodeParser,
}

impl GraphBuilder {
    /// 创建图谱构建器
    ///
    /// 【领域含义】工厂方法，创建包装给定 CodeIndexer 的 GraphBuilder 实例。
    /// 内部初始化 CodeParser 用于重新解析源文件。
    pub fn new(indexer: CodeIndexer) -> Self {
        Self {
            indexer,
            parser: CodeParser::new(),
        }
    }

    /// 构建完整调用图
    ///
    /// 【领域含义】从所有已索引符号构建完整的调用图。重新解析每个已索引文件以发现
    /// 调用表达式，然后对照符号索引解析被调用方名称。返回包含所有调用边的 CallGraph。
    pub fn build_call_graph(&self) -> Result<CallGraph, GraphError> {
        let symbols = self.indexer.get_all_symbols()?;
        if symbols.is_empty() {
            return Ok(CallGraph::new());
        }

        // Build fast lookup: symbol_name → list of SymbolEntry (across all files)
        let name_index = build_name_index(&symbols);

        // Collect unique file paths
        let files: HashSet<&str> = symbols.iter().map(|s| s.file_path.as_str()).collect();

        let mut graph = CallGraph::new();

        for file_path in files {
            let path = Path::new(file_path);
            let edges = self.extract_call_edges_from_file(path, &name_index)?;
            for edge in edges {
                graph.add_edge(edge);
            }
        }

        Ok(graph)
    }

    /// 构建依赖图
    ///
    /// 【领域含义】从索引中的导入符号构建文件级依赖图。读取所有 Import 类型的符号，
    /// 将其解析为已知的已索引文件路径，生成 DependencyGraph。
    pub fn build_dependency_graph(&self) -> Result<DependencyGraph, GraphError> {
        let symbols = self.indexer.get_all_symbols()?;
        if symbols.is_empty() {
            return Ok(DependencyGraph::new());
        }

        // Build index of known file paths for resolution
        let known_files: HashSet<&str> = symbols.iter().map(|s| s.file_path.as_str()).collect();

        let mut graph = DependencyGraph::new();

        // Group symbols by file
        let mut file_symbols: HashMap<&str, Vec<&SymbolEntry>> = HashMap::new();
        let mut file_exports: HashMap<&str, Vec<String>> = HashMap::new();

        for sym in &symbols {
            file_symbols
                .entry(sym.file_path.as_str())
                .or_default()
                .push(sym);

            if sym.symbol_kind != SymbolKind::Import {
                file_exports
                    .entry(sym.file_path.as_str())
                    .or_default()
                    .push(sym.symbol_name.clone());
            }
        }

        // Add exports to graph
        for (file, exports) in &file_exports {
            graph.add_exports(file.to_string(), exports.clone());
        }

        // Process imports
        for sym in &symbols {
            if sym.symbol_kind != SymbolKind::Import {
                continue;
            }

            let resolved = resolve_import_to_file(&sym.symbol_name, &known_files);
            if let Some(target) = resolved {
                graph.add_import(sym.file_path.clone(), target.to_string());
            }
        }

        Ok(graph)
    }

    /// 增量更新图谱
    ///
    /// 【领域含义】当单个文件变更时增量更新调用图和依赖图。重新索引该文件后
    /// 重建完整的调用图和依赖图（为正确性采用全量重建策略）。
    pub fn patch_file(&mut self, path: &Path) -> Result<(CallGraph, DependencyGraph), GraphError> {
        // Re-index the changed file
        self.indexer
            .index_file(path)
            .map_err(|e| {
                warn!("Failed to re-index {}: {}", path.display(), e);
                GraphError::Indexer(e)
            })?;

        // Rebuild full graphs (simpler than selective edge removal for correctness)
        let call_graph = self.build_call_graph()?;
        let dep_graph = self.build_dependency_graph()?;

        Ok((call_graph, dep_graph))
    }

    // ── private helpers ────────────────────────────────────────────────

    /// Parse a single file and extract all call edges by resolving callee names
    /// against the symbol index.
    fn extract_call_edges_from_file(
        &self,
        path: &Path,
        name_index: &HashMap<String, Vec<&SymbolEntry>>,
    ) -> Result<Vec<CallEdge>, GraphError> {
        let language_name = match self.parser.detect_language(path) {
            Some(l) => l.to_string(),
            None => return Ok(Vec::new()),
        };

        let ts_language = match self.parser.get_language(&language_name) {
            Some(l) => l.clone(),
            None => return Ok(Vec::new()),
        };

        let source = std::fs::read_to_string(path)?;
        let file_path = path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .display()
            .to_string();

        let mut parser = Parser::new();
        parser.set_language(&ts_language).map_err(|e| {
            GraphError::Indexer(IndexerError::Parse {
                path: file_path.clone(),
                message: format!("Failed to set language: {}", e),
            })
        })?;

        let tree = parser.parse(&source, None).ok_or_else(|| {
            GraphError::Indexer(IndexerError::Parse {
                path: file_path.clone(),
                message: "Parse returned None".to_string(),
            })
        })?;

        // Get the indexed caller symbols for this file
        let file_symbols = self
            .indexer
            .get_file_symbols(&file_path)
            .unwrap_or_default();

        let mut edges = Vec::new();
        find_call_expressions(
            tree.root_node(),
            source.as_bytes(),
            &language_name,
            &file_path,
            &file_symbols,
            name_index,
            &mut edges,
        );

        Ok(edges)
    }
}

// ── Call expression extraction ────────────────────────────────────────────

/// Recursively walk the AST and extract call edges from call expressions.
#[allow(clippy::too_many_arguments)]
fn find_call_expressions(
    node: Node,
    source: &[u8],
    language: &str,
    file_path: &str,
    file_symbols: &[SymbolEntry],
    name_index: &HashMap<String, Vec<&SymbolEntry>>,
    edges: &mut Vec<CallEdge>,
) {
    let kind = node.kind();

    if is_call_node(language, kind) {
        if let Some(edge) = try_resolve_call(
            node,
            source,
            language,
            file_path,
            file_symbols,
            name_index,
        ) {
            edges.push(edge);
        }
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        find_call_expressions(
            child,
            source,
            language,
            file_path,
            file_symbols,
            name_index,
            edges,
        );
    }
}

/// Check if a node kind represents a function/method call.
fn is_call_node(language: &str, kind: &str) -> bool {
    match language {
        "python" => kind == "call",
        "typescript" | "tsx" => kind == "call_expression",
        "rust" => kind == "call_expression",
        "go" => kind == "call_expression",
        "java" => kind == "method_invocation",
        _ => false,
    }
}

/// Try to resolve a call expression node to a known symbol.
#[allow(clippy::too_many_arguments)]
fn try_resolve_call(
    node: Node,
    source: &[u8],
    language: &str,
    caller_file: &str,
    file_symbols: &[SymbolEntry],
    name_index: &HashMap<String, Vec<&SymbolEntry>>,
) -> Option<CallEdge> {
    let callee_name = extract_callee_name(node, source, language)?;
    let call_line = node.start_position().row + 1; // 1-based

    // Determine the caller symbol: find the nearest enclosing function/method/class
    let caller_symbol = find_enclosing_symbol(node, file_symbols);

    // Skip if the callee is a builtin or unresolvable
    if is_builtin(&callee_name, language) {
        return None;
    }

    // Determine confidence based on language
    let (confidence, is_dynamic) = match language {
        "rust" | "typescript" | "tsx" | "go" | "java" => (EdgeConfidence::Exact, false),
        "python" => {
            // Check if the callee contains '.' (method call on object) or
            // uses getattr/setattr patterns
            let is_dyn = callee_name.contains('.')
                || callee_name.contains("getattr")
                || callee_name.contains("setattr");
            if is_dyn {
                (EdgeConfidence::Uncertain, true)
            } else {
                (EdgeConfidence::Likely, false)
            }
        }
        _ => (EdgeConfidence::Likely, false),
    };

    // Try to resolve the callee to a file
    if let Some(entries) = name_index.get(&callee_name) {
        // If callee is in the same file, use direct file reference
        let same_file = entries.iter().find(|e| e.file_path == caller_file);
        let callee_file = if let Some(entry) = same_file {
            entry.file_path.clone()
        } else {
            // Use the first match from any file
            entries[0].file_path.clone()
        };

        return Some(CallEdge {
            caller_file: caller_file.to_string(),
            caller_symbol,
            callee_file,
            callee_symbol: callee_name,
            call_line,
            is_dynamic,
            confidence,
        });
    }

    // Unresolved: mark as Uncertain
    Some(CallEdge {
        caller_file: caller_file.to_string(),
        caller_symbol,
        callee_file: "unknown".to_string(),
        callee_symbol: callee_name,
        call_line,
        is_dynamic: true,
        confidence: EdgeConfidence::Uncertain,
    })
}

/// Extract the function/method name from a call expression node.
fn extract_callee_name(node: Node, source: &[u8], language: &str) -> Option<String> {
    match language {
        "python" => {
            // Python: call → primary (identifier) or call → primary → attribute
            let func = node.child_by_field_name("function")?;
            if func.kind() == "identifier" {
                return node_text(func, source);
            }
            if func.kind() == "attribute" {
                // obj.method() → extract the full dotted name
                let obj = func.child_by_field_name("object");
                let attr = func.child_by_field_name("attribute");
                if let (Some(obj_node), Some(attr_node)) = (obj, attr) {
                    let obj_name = node_text(obj_node, source).unwrap_or_default();
                    let attr_name = node_text(attr_node, source)?;
                    return Some(format!("{}.{}", obj_name, attr_name));
                }
            }
            None
        }
        "typescript" | "tsx" => {
            let func = node.child_by_field_name("function")?;
            match func.kind() {
                "identifier" => node_text(func, source),
                "member_expression" => {
                    let obj = func.child_by_field_name("object");
                    let prop = func.child_by_field_name("property");
                    if let (Some(o), Some(p)) = (obj, prop) {
                        let o_name = node_text(o, source).unwrap_or_default();
                        let p_name = node_text(p, source)?;
                        Some(format!("{}.{}", o_name, p_name))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        "rust" => {
            let func = node.child_by_field_name("function")?;
            match func.kind() {
                "identifier" => node_text(func, source),
                "scoped_identifier" => node_text(func, source), // std::io::Read etc.
                "field_expression" => {
                    // obj.method()
                    let value = func.child_by_field_name("value");
                    let field = func.child_by_field_name("field");
                    if let (Some(v), Some(f)) = (value, field) {
                        let v_name = node_text(v, source).unwrap_or_default();
                        let f_name = node_text(f, source)?;
                        Some(format!("{}.{}", v_name, f_name))
                    } else {
                        None
                    }
                }
                _ => {
                    // Fallback: just get the first identifier child
                    first_child_text_by_kind(func, source, "identifier")
                }
            }
        }
        "go" => {
            let func = node.child_by_field_name("function")?;
            match func.kind() {
                "identifier" => node_text(func, source),
                "selector_expression" => {
                    let operand = func.child_by_field_name("operand");
                    let field = func.child_by_field_name("field");
                    if let (Some(op), Some(f)) = (operand, field) {
                        let op_name = simple_node_text(op, source);
                        let f_name = node_text(f, source)?;
                        Some(format!("{}.{}", op_name, f_name))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        "java" => {
            // Java method_invocation: name is an identifier child,
            // possibly preceded by an object or class reference
            let name_node = node.child_by_field_name("name");
            if let Some(name) = name_node {
                return node_text(name, source);
            }
            // Fallback: find identifier children
            let obj = node.child_by_field_name("object");
            if let Some(obj_node) = obj {
                return node_text(obj_node, source);
            }
            first_child_text_by_kind(node, source, "identifier")
        }
        _ => None,
    }
}

/// Find the nearest enclosing function/method/class symbol for a node,
/// which acts as the "caller" of the call expression.
fn find_enclosing_symbol(node: Node, file_symbols: &[SymbolEntry]) -> String {
    let line = node.start_position().row + 1;

    // Find the closest symbol whose line range contains this call's line
    let mut best: Option<&SymbolEntry> = None;

    for sym in file_symbols {
        if sym.line_range.start <= line && line <= sym.line_range.end {
            match best {
                None => best = Some(sym),
                Some(current) => {
                    // Prefer the innermost (narrowest) range
                    let current_span = current.line_range.end - current.line_range.start;
                    let sym_span = sym.line_range.end - sym.line_range.start;
                    if sym_span < current_span {
                        best = Some(sym);
                    }
                }
            }
        }
    }

    best.map(|s| s.symbol_name.clone())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Simple node text extraction.
fn node_text(node: Node, source: &[u8]) -> Option<String> {
    node.utf8_text(source).ok().map(|s| s.to_string())
}

/// Get the text of the first child with the given kind.
fn first_child_text_by_kind(node: Node, source: &[u8], kind: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == kind {
            return node_text(child, source);
        }
    }
    None
}

/// Extract a simple node text, falling back to empty string.
fn simple_node_text(node: Node, source: &[u8]) -> String {
    node.utf8_text(source).ok().map(|s| s.to_string()).unwrap_or_default()
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Build a name → entries lookup from all symbols.
fn build_name_index(
    symbols: &[SymbolEntry],
) -> HashMap<String, Vec<&SymbolEntry>> {
    let mut index: HashMap<String, Vec<&SymbolEntry>> = HashMap::new();
    for sym in symbols {
        index.entry(sym.symbol_name.clone()).or_default().push(sym);
    }
    index
}

/// Check if a name is a language builtin.
fn is_builtin(name: &str, language: &str) -> bool {
    match language {
        "python" => matches!(
            name,
            "print" | "len" | "range" | "int" | "str" | "float" | "bool"
                | "list" | "dict" | "set" | "tuple" | "type" | "isinstance"
                | "super" | "open" | "input" | "enumerate" | "zip" | "map"
                | "filter" | "sorted" | "reversed" | "abs" | "min" | "max"
                | "sum" | "any" | "all" | "chr" | "ord" | "hex" | "oct"
                | "bin" | "round" | "hasattr" | "getattr" | "setattr"
                | "delattr" | "vars" | "dir" | "id" | "__import__"
        ),
        "typescript" | "tsx" => matches!(
            name,
            "console.log" | "console.error" | "console.warn" | "console.info"
                | "parseInt" | "parseFloat" | "JSON.parse" | "JSON.stringify"
                | "setTimeout" | "setInterval" | "clearTimeout" | "clearInterval"
        ),
        _ => false,
    }
}

/// Try to resolve an import path to a known indexed file.
fn resolve_import_to_file<'a>(
    import_name: &str,
    known_files: &HashSet<&'a str>,
) -> Option<&'a str> {
    // Clean the import name (strip quotes, leading dots, etc.)
    let cleaned = import_name
        .trim_matches('"')
        .trim_matches('\'')
        .trim_start_matches('.');

    // Try exact match first
    for file in known_files.iter() {
        let file_stem = Path::new(file)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if file_stem == cleaned {
            return Some(file);
        }
        // Check if the file path contains the module name
        if file.contains(cleaned) {
            return Some(file);
        }
    }

    // Try matching module path segments (e.g., "os.path" → file containing "os/path")
    let segments: Vec<&str> = cleaned.split('.').collect();
    for file in known_files.iter() {
        let path_lower = file.to_lowercase();
        if segments.iter().all(|seg| path_lower.contains(&seg.to_lowercase())) {
            return Some(file);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_temp_file(dir: &TempDir, name: &str, content: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    #[allow(dead_code)]
    fn setup_indexer(
        dir: &TempDir,
        files: &[(&str, &str)],
    ) -> (CodeIndexer, PathBuf) {
        for (name, content) in files {
            create_temp_file(dir, name, content);
        }

        let db_path = dir.path().join("index.db");
        let mut indexer = CodeIndexer::new(&db_path.display().to_string()).unwrap();
        indexer.index_directory(dir.path()).unwrap();
        (indexer, db_path)
    }

    #[test]
    fn test_build_call_graph_python() {
        let dir = TempDir::new().unwrap();
        create_temp_file(
            &dir,
            "util.py",
            "def helper(x):\n    return x * 2\n",
        );
        create_temp_file(
            &dir,
            "main.py",
            "from util import helper\n\ndef main():\n    result = helper(5)\n    print(result)\n",
        );

        let db_path = dir.path().join("index.db");
        let mut indexer = CodeIndexer::new(&db_path.display().to_string()).unwrap();
        indexer.index_directory(dir.path()).unwrap();

        let builder = GraphBuilder::new(indexer);
        let call_graph = builder.build_call_graph().unwrap();

        // Should have at least one edge: main → helper
        assert!(
            call_graph.edge_count() > 0,
            "Expected at least one call edge"
        );

        // Check that main calls helper
        let util_path = dir.path().join("util.py")
            .canonicalize()
            .unwrap_or_else(|_| dir.path().join("util.py"));
        let helper_key = crate::graph::make_symbol_key(
            &util_path.display().to_string(),
            "helper",
        );
        let callers = call_graph.callers_map().get(&helper_key);
        assert!(
            callers.is_some(),
            "Expected callers for helper, found none. Keys: {:?}",
            call_graph.callers_map().keys().collect::<Vec<_>>()
        );
        let callers = callers.unwrap();
        assert!(!callers.is_empty());
        assert_eq!(callers[0].caller_symbol, "main");
    }

    #[test]
    fn test_build_call_graph_rust() {
        let dir = TempDir::new().unwrap();
        create_temp_file(
            &dir,
            "util.rs",
            "pub fn compute() -> i32 { 42 }\n",
        );
        create_temp_file(
            &dir,
            "main.rs",
            "mod util;\nuse util::compute;\n\nfn main() {\n    let x = compute();\n}\n",
        );

        let db_path = dir.path().join("index.db");
        let mut indexer = CodeIndexer::new(&db_path.display().to_string()).unwrap();
        indexer.index_directory(dir.path()).unwrap();

        let builder = GraphBuilder::new(indexer);
        let call_graph = builder.build_call_graph().unwrap();

        // Verify edges exist
        assert!(
            call_graph.edge_count() > 0,
            "Expected call edges, got {}",
            call_graph.edge_count()
        );

        // Check confidence is Exact for Rust
        for edge in call_graph.edges() {
            assert_eq!(edge.confidence, EdgeConfidence::Exact);
        }
    }

    #[test]
    fn test_build_dependency_graph() {
        let dir = TempDir::new().unwrap();
        create_temp_file(
            &dir,
            "util.py",
            "def helper():\n    pass\n",
        );
        create_temp_file(
            &dir,
            "main.py",
            "from util import helper\nimport os\n\ndef main():\n    helper()\n",
        );

        let db_path = dir.path().join("index.db");
        let mut indexer = CodeIndexer::new(&db_path.display().to_string()).unwrap();
        indexer.index_directory(dir.path()).unwrap();

        let builder = GraphBuilder::new(indexer);
        let dep_graph = builder.build_dependency_graph().unwrap();

        assert!(
            dep_graph.import_count() > 0,
            "Expected import edges, got {}",
            dep_graph.import_count()
        );
    }

    #[test]
    fn test_dynamic_dispatch_detection() {
        let dir = TempDir::new().unwrap();
        create_temp_file(
            &dir,
            "dynamic.py",
            r#"
class MyClass:
    def method(self):
        pass

def caller(obj):
    obj.method()  # dynamic dispatch
    getattr(obj, 'attr')  # dynamic access
"#,
        );

        let db_path = dir.path().join("index.db");
        let mut indexer = CodeIndexer::new(&db_path.display().to_string()).unwrap();
        indexer.index_directory(dir.path()).unwrap();

        let builder = GraphBuilder::new(indexer);
        let call_graph = builder.build_call_graph().unwrap();

        // Check for dynamic edges
        let dynamic_edges: Vec<&CallEdge> = call_graph
            .edges()
            .filter(|e| e.is_dynamic)
            .collect();
        // obj.method() should produce a dynamic edge since callee is "obj.method"
        // (which contains a dot, marking it as dynamic dispatch)
        assert!(
            !dynamic_edges.is_empty(),
            "Expected at least one dynamic call edge, got edges: {:?}",
            call_graph
                .edges()
                .map(|e| format!("{} → {} (dynamic:{})", e.caller_symbol, e.callee_symbol, e.is_dynamic))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_patch_file() {
        let dir = TempDir::new().unwrap();
        let py_path = create_temp_file(
            &dir,
            "lib.py",
            "def old_func():\n    pass\n",
        );
        create_temp_file(
            &dir,
            "app.py",
            "from lib import old_func\n\ndef run():\n    old_func()\n",
        );

        let db_path = dir.path().join("index.db");
        let mut indexer = CodeIndexer::new(&db_path.display().to_string()).unwrap();
        indexer.index_directory(dir.path()).unwrap();

        let mut builder = GraphBuilder::new(indexer);

        // Modify lib.py to add a new function
        std::fs::write(
            &py_path,
            "def old_func():\n    pass\n\ndef new_func():\n    pass\n",
        )
        .unwrap();

        let (call_graph, _dep_graph) = builder.patch_file(&py_path).unwrap();

        // After patching, the graph should include the new file state
        assert!(call_graph.edge_count() > 0);
    }

    #[test]
    fn test_confidence_tracking() {
        let dir = TempDir::new().unwrap();
        // Python → Likely
        create_temp_file(
            &dir,
            "mod.py",
            "def py_func():\n    pass\n",
        );
        create_temp_file(
            &dir,
            "caller.py",
            "from mod import py_func\n\ndef caller():\n    py_func()\n",
        );
        // Rust → Exact
        create_temp_file(
            &dir,
            "utils.rs",
            "pub fn rs_func() {}\n",
        );
        create_temp_file(
            &dir,
            "runner.rs",
            "mod utils;\nuse utils::rs_func;\n\nfn runner() {\n    rs_func();\n}\n",
        );

        let db_path = dir.path().join("index.db");
        let mut indexer = CodeIndexer::new(&db_path.display().to_string()).unwrap();
        indexer.index_directory(dir.path()).unwrap();

        let builder = GraphBuilder::new(indexer);
        let call_graph = builder.build_call_graph().unwrap();

        // Check that Python edges are Likely, Rust edges are Exact
        let py_edges: Vec<&CallEdge> = call_graph
            .edges()
            .filter(|e| e.callee_symbol == "py_func")
            .collect();
        let rs_edges: Vec<&CallEdge> = call_graph
            .edges()
            .filter(|e| e.callee_symbol == "rs_func")
            .collect();

        for edge in &py_edges {
            assert_eq!(
                edge.confidence,
                EdgeConfidence::Likely,
                "Python edges should be Likely, got {:?}",
                edge.confidence
            );
        }

        for edge in &rs_edges {
            assert_eq!(
                edge.confidence,
                EdgeConfidence::Exact,
                "Rust edges should be Exact, got {:?}",
                edge.confidence
            );
        }
    }
}
