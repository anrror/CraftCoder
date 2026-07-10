/// 上下文模块
///
/// 【领域含义】Agent 引擎的代码上下文检索管道，负责将用户任务描述转化为结构化的 XML 提示块，
/// 供 Agent 注入系统消息。属于"上下文聚合"限界上下文。
pub mod context;
/// 调用图与依赖图模块
///
/// 【领域含义】基于 AST 索引构建的函数调用关系和文件依赖关系图，
/// 支持影响分析（变更影响范围评估）。属于"代码图谱"限界上下文。
pub mod graph;
/// 索引器模块
///
/// 【领域含义】AST 驱动的代码符号索引引擎，将源代码解析为结构化符号条目并持久化到 SQLite。
/// 属于"代码解析与索引"限界上下文。
pub mod indexer;
/// 检索模块
///
/// 【领域含义】混合检索管道，融合 BM25 词法搜索、向量语义搜索和图谱重排序，
/// 为代码检索提供统一的查询接口。属于"代码检索"限界上下文。
pub mod retrieval;
/// 文件监听器模块
///
/// 【领域含义】增量式文件系统监听器，检测源代码变更并自动更新符号索引和调用图。
/// 属于"增量同步"限界上下文。
pub mod watcher;

/// 调用边
///
/// 【领域含义】调用图中从调用方到被调用方的有向边，记录调用位置、动态分发状态和置信度。
pub use graph::{CallEdge, CallGraph, DependencyGraph, EdgeConfidence, ImpactResult};
/// 代码索引器
///
/// 【领域含义】AST 驱动的代码符号索引引擎，将源代码解析为结构化符号条目并持久化到 SQLite。
pub use indexer::CodeIndexer;
/// 符号条目与符号类型
///
/// 【领域含义】从源代码中提取的代码符号（函数、类、结构体等）及其分类枚举。
pub use indexer::symbol::{SymbolEntry, SymbolKind};
/// 检索错误、检索结果、检索器、评分结果、搜索选项
///
/// 【领域含义】混合检索管道的核心类型：检索器聚合 BM25+向量+图谱，搜索选项控制检索行为。
pub use retrieval::{RetrievalError, RetrievalResult, Retriever, ScoredResult, SearchOptions};
/// 文件变更类型与文件事件
///
/// 【领域含义】文件系统变更事件的分类枚举，用于增量索引的变更检测。
pub use watcher::event::{FileChangeKind, FileEvent};
/// 索引同步器、同步动作、同步结果
///
/// 【领域含义】增量同步引擎的核心类型，协调索引器和图谱构建器处理文件变更事件。
pub use watcher::sync::{IndexSync, SyncAction, SyncResult};
/// 文件监听器
///
/// 【领域含义】跨平台文件系统监听器，封装 notify crate 提供防抖、哈希去重的增量索引能力。
pub use watcher::FileWatcher;
