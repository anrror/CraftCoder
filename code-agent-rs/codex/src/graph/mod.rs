//! Call graph and dependency graph construction from AST-indexed symbols.
//!
//! This module builds on the [`crate::indexer`] to produce resolved call edges
//! and import-based dependency edges between source files.

pub mod builder;
pub mod query;

use std::collections::HashMap;

use rusqlite::{params, Connection};

/// 图谱操作错误
///
/// 【领域含义】调用图和依赖图操作过程中可能出现的错误类型，涵盖索引器错误、
/// I/O 错误、SQLite 错误和索引器缺失等情况。属于"代码图谱"限界上下文的异常模型。
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("Indexer error: {0}")]
    Indexer(#[from] crate::indexer::IndexerError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("No indexer available for graph building")]
    NoIndexer,
}

/// 边置信度（值对象）
///
/// 【领域含义】调用边解析结果的置信度级别枚举。Exact 表示静态编译可验证的调用
/// （如 Rust/TypeScript 静态调用），Likely 表示高置信度但非编译器保证的调用
/// （如 Python 导入），Uncertain 表示动态分发或基于名称的解析（如 getattr、鸭子类型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeConfidence {
    /// Statically verified — e.g., Rust/TypeScript static calls.
    Exact,
    /// High confidence but not compiler-guaranteed — e.g., Python imports.
    Likely,
    /// Dynamic dispatch or name-based resolution — e.g., `getattr`, duck typing.
    Uncertain,
}

impl EdgeConfidence {
    /// 转换为字符串表示
    ///
    /// 【领域含义】将置信度枚举值转换为持久化友好的字符串形式，用于 SQLite 存储。
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeConfidence::Exact => "exact",
            EdgeConfidence::Likely => "likely",
            EdgeConfidence::Uncertain => "uncertain",
        }
    }

    /// 从字符串解析置信度
    ///
    /// 【领域含义】将 SQLite 中存储的字符串形式反序列化为 EdgeConfidence 枚举值。
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "exact" => Some(EdgeConfidence::Exact),
            "likely" => Some(EdgeConfidence::Likely),
            "uncertain" => Some(EdgeConfidence::Uncertain),
            _ => None,
        }
    }
}

/// 调用边（实体）
///
/// 【领域含义】调用图中从调用方到被调用方的有向边实体。记录调用方和被调用方的
/// 文件路径、符号名称、调用行号、是否为动态分发以及解析置信度。
/// 是"代码图谱"限界上下文的核心关系实体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallEdge {
    /// File path of the caller.
    pub caller_file: String,
    /// Symbol name of the caller.
    pub caller_symbol: String,
    /// File path of the callee.
    pub callee_file: String,
    /// Symbol name of the callee.
    pub callee_symbol: String,
    /// Line number where the call occurs (1-based).
    pub call_line: usize,
    /// Whether this is a dynamic dispatch call.
    pub is_dynamic: bool,
    /// Confidence in the resolution.
    pub confidence: EdgeConfidence,
}

/// 调用图（聚合根）
///
/// 【领域含义】完整的函数调用关系图，维护调用方到被调用方的双向映射。
/// 是"代码图谱"限界上下文的聚合根，支持边添加、按文件移除边、影响分析等
/// 核心领域行为。内部维护 callers（被调用方→调用方列表）和 callees
/// （调用方→被调用方列表）两个索引以实现高效查询。
#[derive(Debug, Clone, Default)]
pub struct CallGraph {
    /// callers[callee] = list of edges where callee is the target.
    callers: HashMap<String, Vec<CallEdge>>,
    /// callees[caller] = list of edges where caller is the source.
    callees: HashMap<String, Vec<CallEdge>>,
    /// All edges stored flat for persistence.
    all_edges: Vec<CallEdge>,
}

impl CallGraph {
    /// 创建空调用图
    ///
    /// 【领域含义】工厂方法，创建一个空的调用图实例。
    pub fn new() -> Self {
        Self::default()
    }

    /// 添加调用边
    ///
    /// 【领域含义】向调用图中添加一条调用边，同时更新调用方索引和被调用方索引。
    pub fn add_edge(&mut self, edge: CallEdge) {
        let callee_key = make_symbol_key(&edge.callee_file, &edge.callee_symbol);
        let caller_key = make_symbol_key(&edge.caller_file, &edge.caller_symbol);

        self.callers
            .entry(callee_key)
            .or_default()
            .push(edge.clone());
        self.callees
            .entry(caller_key)
            .or_default()
            .push(edge.clone());
        self.all_edges.push(edge);
    }

    /// 获取边数量
    ///
    /// 【领域含义】返回调用图中所有边的总数。
    pub fn edge_count(&self) -> usize {
        self.all_edges.len()
    }

    /// 遍历所有调用边
    ///
    /// 【领域含义】返回所有调用边的迭代器，用于遍历和统计。
    pub fn edges(&self) -> impl Iterator<Item = &CallEdge> {
        self.all_edges.iter()
    }

    /// 获取所有边
    ///
    /// 【领域含义】返回所有调用边的切片引用，用于持久化存储。
    pub fn all_edges(&self) -> &[CallEdge] {
        &self.all_edges
    }

    /// 移除指定文件的所有调用边
    ///
    /// 【领域含义】移除调用图中所有以指定文件为调用方的边。用于增量更新时
    /// 清理旧文件的调用关系。
    pub fn remove_edges_from_file(&mut self, file_path: &str) {
        let keys_to_remove: Vec<String> = self
            .callers
            .values()
            .flat_map(|edges| {
                edges
                    .iter()
                    .filter(|e| e.caller_file == file_path)
                    .map(|e| make_symbol_key(&e.caller_file, &e.caller_symbol))
            })
            .collect();

        for key in &keys_to_remove {
            self.callees.remove(key);
        }

        // Remove from callers index (edges pointing TO callees from this file)
        for edges in self.callers.values_mut() {
            edges.retain(|e| e.caller_file != file_path);
        }

        // Remove from callees index
        self.callees.retain(|k, _| {
            !self.all_edges.iter().any(|e| {
                make_symbol_key(&e.caller_file, &e.caller_symbol) == *k
                    && e.caller_file == file_path
            })
        });

        // Remove from flat list
        self.all_edges.retain(|e| e.caller_file != file_path);
    }

    // ── private accessors for query module ──

    #[allow(dead_code)]
    pub(crate) fn callers_map(&self) -> &HashMap<String, Vec<CallEdge>> {
        &self.callers
    }

    #[allow(dead_code)]
    pub(crate) fn callees_map(&self) -> &HashMap<String, Vec<CallEdge>> {
        &self.callees
    }
}

/// 依赖图（聚合根）
///
/// 【领域含义】跟踪文件间导入关系的依赖图。维护 imports（源文件→目标文件列表）
/// 和 exports（文件→导出符号列表）两个映射。是"代码图谱"限界上下文的聚合根，
/// 用于分析文件级依赖关系和变更影响范围。
#[derive(Debug, Clone, Default)]
pub struct DependencyGraph {
    /// imports[source_file] = list of target files it imports from.
    imports: HashMap<String, Vec<String>>,
    /// exports[file] = list of exported symbol names.
    exports: HashMap<String, Vec<String>>,
}

impl DependencyGraph {
    /// 创建空依赖图
    ///
    /// 【领域含义】工厂方法，创建一个空的依赖图实例。
    pub fn new() -> Self {
        Self::default()
    }

    /// 添加导入边
    ///
    /// 【领域含义】向依赖图中添加一条从源文件到目标文件的导入关系边。
    pub fn add_import(&mut self, source_file: String, target_file: String) {
        self.imports
            .entry(source_file)
            .or_default()
            .push(target_file);
    }

    /// 添加文件导出符号
    ///
    /// 【领域含义】记录指定文件导出的符号名称列表，用于跨文件符号解析。
    pub fn add_exports(&mut self, file: String, symbols: Vec<String>) {
        self.exports.entry(file).or_default().extend(symbols);
    }

    /// 获取导入边数量
    ///
    /// 【领域含义】返回依赖图中所有导入边的总数。
    pub fn import_count(&self) -> usize {
        self.imports.values().map(|v| v.len()).sum()
    }

    /// 获取所有导入边
    ///
    /// 【领域含义】返回所有导入边的 (源文件, 目标文件) 元组列表，用于持久化和遍历。
    pub fn import_edges(&self) -> Vec<(String, String)> {
        let mut edges = Vec::new();
        for (source, targets) in &self.imports {
            for target in targets {
                edges.push((source.clone(), target.clone()));
            }
        }
        edges
    }

    // ── private accessors for query module ──

    #[allow(dead_code)]
    pub(crate) fn imports_map(&self) -> &HashMap<String, Vec<String>> {
        &self.imports
    }

    #[allow(dead_code)]
    pub(crate) fn exports_map(&self) -> &HashMap<String, Vec<String>> {
        &self.exports
    }
}

/// 影响分析结果（值对象）
///
/// 【领域含义】变更影响分析查询的结果，包含直接调用方（深度 1）、间接调用方
/// （深度 2+）、受影响的文件列表和总影响符号数。用于评估修改某个符号时
/// 可能波及的范围。
#[derive(Debug, Clone, Default)]
pub struct ImpactResult {
    /// Symbols that directly call the target (depth 1).
    pub direct_callers: Vec<String>,
    /// Symbols that transitively call the target (depth 2+).
    pub indirect_callers: Vec<String>,
    /// Files affected by a change to the target.
    pub affected_files: Vec<String>,
    /// Total number of symbols in the impact set.
    pub total_impact_count: usize,
}

// ── SQLite persistence ────────────────────────────────────────────────────

/// 图谱存储（仓储）
///
/// 【领域含义】基于 SQLite 的调用图和依赖图持久化仓储，是"代码图谱"限界上下文的
/// 基础设施层组件。负责调用边和依赖边的持久化、加载和原子批量保存。
pub struct GraphStorage {
    conn: Connection,
}

impl GraphStorage {
    /// 打开或创建图谱数据库
    ///
    /// 【领域含义】在指定路径打开或创建 SQLite 数据库，并初始化调用边和依赖边表结构。
    pub fn new(db_path: &str) -> Result<Self, GraphError> {
        let conn = Connection::open(db_path)?;
        let storage = Self { conn };
        storage.init_schema()?;
        Ok(storage)
    }

    /// 创建内存数据库
    ///
    /// 【领域含义】创建基于内存的 SQLite 数据库，主要用于测试场景。
    pub fn new_in_memory() -> Result<Self, GraphError> {
        let conn = Connection::open_in_memory()?;
        let storage = Self { conn };
        storage.init_schema()?;
        Ok(storage)
    }

    fn init_schema(&self) -> Result<(), GraphError> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS call_edges (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                caller_file TEXT NOT NULL,
                caller_symbol TEXT NOT NULL,
                callee_file TEXT NOT NULL,
                callee_symbol TEXT NOT NULL,
                call_line INTEGER,
                is_dynamic INTEGER NOT NULL DEFAULT 0,
                confidence TEXT NOT NULL DEFAULT 'exact',
                created_at TEXT DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_call_callee ON call_edges(callee_symbol);
            CREATE INDEX IF NOT EXISTS idx_call_caller ON call_edges(caller_symbol);

            CREATE TABLE IF NOT EXISTS dependency_edges (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source_file TEXT NOT NULL,
                target_file TEXT NOT NULL,
                dependency_type TEXT NOT NULL DEFAULT 'import',
                created_at TEXT DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_dep_source ON dependency_edges(source_file);
            CREATE INDEX IF NOT EXISTS idx_dep_target ON dependency_edges(target_file);",
        )?;
        Ok(())
    }

    /// 持久化调用图
    ///
    /// 【领域含义】将 CallGraph 中的所有调用边持久化到 SQLite。先清空旧数据再批量插入。
    pub fn save_call_graph(&self, graph: &CallGraph) -> Result<usize, GraphError> {
        // Clear existing call edges
        self.conn.execute("DELETE FROM call_edges", [])?;

        let mut count = 0;
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO call_edges (caller_file, caller_symbol, callee_file, callee_symbol,
                 call_line, is_dynamic, confidence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;

            for edge in graph.all_edges() {
                stmt.execute(params![
                    edge.caller_file,
                    edge.caller_symbol,
                    edge.callee_file,
                    edge.callee_symbol,
                    edge.call_line as i64,
                    edge.is_dynamic as i64,
                    edge.confidence.as_str(),
                ])?;
                count += 1;
            }
        }
        tx.commit()?;
        Ok(count)
    }

    /// 从数据库加载调用图
    ///
    /// 【领域含义】从 SQLite 中读取所有调用边并重建 CallGraph 实例。
    pub fn load_call_graph(&self) -> Result<CallGraph, GraphError> {
        let mut graph = CallGraph::new();
        let mut stmt = self.conn.prepare(
            "SELECT caller_file, caller_symbol, callee_file, callee_symbol,
                    call_line, is_dynamic, confidence
             FROM call_edges
             ORDER BY id",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(CallEdge {
                caller_file: row.get(0)?,
                caller_symbol: row.get(1)?,
                callee_file: row.get(2)?,
                callee_symbol: row.get(3)?,
                call_line: row.get::<_, i64>(4)? as usize,
                is_dynamic: row.get::<_, i64>(5)? != 0,
                confidence: EdgeConfidence::from_str(&row.get::<_, String>(6)?)
                    .unwrap_or(EdgeConfidence::Exact),
            })
        })?;

        for row in rows {
            graph.add_edge(row?);
        }
        Ok(graph)
    }

    /// 持久化依赖图
    ///
    /// 【领域含义】将 DependencyGraph 中的所有依赖边持久化到 SQLite。先清空旧数据再批量插入。
    pub fn save_dependency_graph(&self, graph: &DependencyGraph) -> Result<usize, GraphError> {
        // Clear existing dependency edges
        self.conn.execute("DELETE FROM dependency_edges", [])?;

        let mut count = 0;
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO dependency_edges (source_file, target_file, dependency_type)
                 VALUES (?1, ?2, ?3)",
            )?;

            for (source, target) in graph.import_edges() {
                stmt.execute(params![source, target, "import"])?;
                count += 1;
            }
        }
        tx.commit()?;
        Ok(count)
    }

    /// 从数据库加载依赖图
    ///
    /// 【领域含义】从 SQLite 中读取所有依赖边并重建 DependencyGraph 实例。
    pub fn load_dependency_graph(&self) -> Result<DependencyGraph, GraphError> {
        let mut graph = DependencyGraph::new();
        let mut stmt = self.conn.prepare(
            "SELECT source_file, target_file FROM dependency_edges ORDER BY id",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        for row in rows {
            let (source, target) = row?;
            graph.add_import(source, target);
        }
        Ok(graph)
    }

    /// 原子保存两个图
    ///
    /// 【领域含义】在单个事务中原子性地持久化调用图和依赖图，保证数据一致性。
    pub fn save_graphs(
        &self,
        call_graph: &CallGraph,
        dep_graph: &DependencyGraph,
    ) -> Result<(usize, usize), GraphError> {
        let tx = self.conn.unchecked_transaction()?;
        // Clear both tables
        tx.execute("DELETE FROM call_edges", [])?;
        tx.execute("DELETE FROM dependency_edges", [])?;

        let mut call_count = 0;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO call_edges (caller_file, caller_symbol, callee_file, callee_symbol,
                 call_line, is_dynamic, confidence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for edge in call_graph.all_edges() {
                stmt.execute(params![
                    edge.caller_file,
                    edge.caller_symbol,
                    edge.callee_file,
                    edge.callee_symbol,
                    edge.call_line as i64,
                    edge.is_dynamic as i64,
                    edge.confidence.as_str(),
                ])?;
                call_count += 1;
            }
        }

        let mut dep_count = 0;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO dependency_edges (source_file, target_file, dependency_type)
                 VALUES (?1, ?2, ?3)",
            )?;
            for (source, target) in dep_graph.import_edges() {
                stmt.execute(params![source, target, "import"])?;
                dep_count += 1;
            }
        }

        tx.commit()?;
        Ok((call_count, dep_count))
    }

    /// 获取原始 SQLite 连接
    ///
    /// 【领域含义】暴露底层 SQLite 连接引用，用于执行高级查询和自定义操作。
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Create a unique key for a symbol from file path and symbol name.
pub(crate) fn make_symbol_key(file_path: &str, symbol_name: &str) -> String {
    format!("{}::{}", file_path, symbol_name)
}

/// Parse a symbol key back into (file_path, symbol_name).
#[allow(dead_code)]
pub(crate) fn parse_symbol_key(key: &str) -> Option<(&str, &str)> {
    let pos = key.rfind("::")?;
    Some((&key[..pos], &key[pos + 2..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_call_graph_add_and_query_edges() {
        let mut cg = CallGraph::new();

        cg.add_edge(CallEdge {
            caller_file: "main.py".to_string(),
            caller_symbol: "main".to_string(),
            callee_file: "util.py".to_string(),
            callee_symbol: "helper".to_string(),
            call_line: 10,
            is_dynamic: false,
            confidence: EdgeConfidence::Likely,
        });

        cg.add_edge(CallEdge {
            caller_file: "main.py".to_string(),
            caller_symbol: "main".to_string(),
            callee_file: "lib.rs".to_string(),
            callee_symbol: "compute".to_string(),
            call_line: 12,
            is_dynamic: false,
            confidence: EdgeConfidence::Exact,
        });

        assert_eq!(cg.edge_count(), 2);
        assert_eq!(cg.all_edges().len(), 2);

        let key = make_symbol_key("util.py", "helper");
        let callers = &cg.callers_map()[&key];
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0].caller_symbol, "main");
    }

    #[test]
    fn test_call_graph_remove_file_edges() {
        let mut cg = CallGraph::new();
        cg.add_edge(CallEdge {
            caller_file: "a.py".to_string(),
            caller_symbol: "func_a".to_string(),
            callee_file: "b.py".to_string(),
            callee_symbol: "func_b".to_string(),
            call_line: 1,
            is_dynamic: false,
            confidence: EdgeConfidence::Exact,
        });
        cg.add_edge(CallEdge {
            caller_file: "a.py".to_string(),
            caller_symbol: "func_a".to_string(),
            callee_file: "c.py".to_string(),
            callee_symbol: "func_c".to_string(),
            call_line: 2,
            is_dynamic: false,
            confidence: EdgeConfidence::Exact,
        });
        cg.add_edge(CallEdge {
            caller_file: "b.py".to_string(),
            caller_symbol: "func_b".to_string(),
            callee_file: "c.py".to_string(),
            callee_symbol: "func_c".to_string(),
            call_line: 1,
            is_dynamic: false,
            confidence: EdgeConfidence::Exact,
        });

        assert_eq!(cg.edge_count(), 3);

        // Remove all edges from a.py as caller
        cg.remove_edges_from_file("a.py");

        assert_eq!(cg.edge_count(), 1);
        assert_eq!(cg.all_edges()[0].caller_file, "b.py");
    }

    #[test]
    fn test_dependency_graph() {
        let mut dg = DependencyGraph::new();
        dg.add_import("main.py".to_string(), "util.py".to_string());
        dg.add_import("main.py".to_string(), "models.py".to_string());
        dg.add_import("util.py".to_string(), "os".to_string());

        assert_eq!(dg.import_count(), 3);

        let edges = dg.import_edges();
        assert_eq!(edges.len(), 3);
    }

    #[test]
    fn test_graph_storage_roundtrip() {
        let storage = GraphStorage::new_in_memory().unwrap();

        let mut cg = CallGraph::new();
        cg.add_edge(CallEdge {
            caller_file: "main.py".to_string(),
            caller_symbol: "main".to_string(),
            callee_file: "util.py".to_string(),
            callee_symbol: "helper".to_string(),
            call_line: 10,
            is_dynamic: true,
            confidence: EdgeConfidence::Uncertain,
        });

        let mut dg = DependencyGraph::new();
        dg.add_import("main.py".to_string(), "util.py".to_string());

        // Save and reload
        let (call_count, dep_count) = storage.save_graphs(&cg, &dg).unwrap();
        assert_eq!(call_count, 1);
        assert_eq!(dep_count, 1);

        let loaded_cg = storage.load_call_graph().unwrap();
        assert_eq!(loaded_cg.edge_count(), 1);
        let loaded_edge = &loaded_cg.all_edges()[0];
        assert_eq!(loaded_edge.caller_symbol, "main");
        assert_eq!(loaded_edge.callee_symbol, "helper");
        assert!(loaded_edge.is_dynamic);
        assert_eq!(loaded_edge.confidence, EdgeConfidence::Uncertain);

        let loaded_dg = storage.load_dependency_graph().unwrap();
        assert_eq!(loaded_dg.import_count(), 1);
    }

    #[test]
    fn test_make_parse_symbol_key() {
        let key = make_symbol_key("src/main.py", "MyClass.method");
        assert_eq!(key, "src/main.py::MyClass.method");

        let (file, sym) = parse_symbol_key(&key).unwrap();
        assert_eq!(file, "src/main.py");
        assert_eq!(sym, "MyClass.method");
    }
}
