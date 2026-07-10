pub mod parser;
pub mod storage;
pub mod symbol;

use std::path::Path;

use log::warn;

use parser::CodeParser;
use storage::SymbolStorage;
use symbol::SymbolEntry;

/// 索引器错误
///
/// 【领域含义】代码索引过程中可能出现的错误类型，涵盖不支持的语言、文件读取失败、
/// 语法解析错误、存储层错误和文件遍历错误。属于"代码解析与索引"限界上下文的异常模型。
#[derive(Debug, thiserror::Error)]
pub enum IndexerError {
    #[error("Unsupported language for file: {path}")]
    UnsupportedLanguage { path: String },

    #[error("Failed to read file {path}: {source}")]
    FileRead {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Parse error in {path}: {message}")]
    Parse { path: String, message: String },

    #[error("Storage error: {0}")]
    Storage(#[from] rusqlite::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Walk error: {0}")]
    Walk(#[from] ignore::Error),
}

/// 索引统计
///
/// 【领域含义】一次索引操作的统计信息，记录处理的文件数、成功索引数、跳过数、
/// 发现的符号数和错误列表。属于"代码解析与索引"限界上下文的值对象。
#[derive(Debug, Clone, Default)]
pub struct IndexStats {
    /// Total number of files processed
    pub files_processed: usize,
    /// Number of files successfully indexed
    pub files_indexed: usize,
    /// Number of files skipped (unsupported language or errors)
    pub files_skipped: usize,
    /// Total number of symbols extracted
    pub symbols_found: usize,
    /// Errors encountered during indexing
    pub errors: Vec<String>,
}

/// 代码索引器（聚合根）
///
/// 【领域含义】AST 驱动的代码符号索引引擎，作为"代码解析与索引"限界上下文的聚合根。
/// 协调 CodeParser（解析器）和 SymbolStorage（存储）两个领域对象，提供文件索引、
/// 目录递归索引、符号搜索和增量更新等核心领域能力。
pub struct CodeIndexer {
    parser: CodeParser,
    storage: SymbolStorage,
}

impl CodeIndexer {
    /// 创建代码索引器
    ///
    /// 【领域含义】工厂方法，创建代码索引器聚合根实例。接受 SQLite 数据库路径，
    /// 内部初始化 CodeParser（AST 解析器）和 SymbolStorage（符号存储）两个领域对象。
    pub fn new(db_path: &str) -> Result<Self, IndexerError> {
        let parser = CodeParser::new();
        let storage = SymbolStorage::new(db_path)?;
        Ok(Self { parser, storage })
    }

    /// 索引单个源文件
    ///
    /// 【领域含义】索引单个源文件的领域行为。先通过 CodeParser 解析 AST 提取符号，
    /// 再通过 SymbolStorage 将符号持久化到 SQLite。返回提取的符号列表。
    /// 不支持的语言或解析错误会返回空列表而非错误。
    pub fn index_file(&mut self, path: &Path) -> Result<Vec<SymbolEntry>, IndexerError> {
        let file_path = path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .display()
            .to_string();

        let symbols = match self.parser.parse_file(path) {
            Ok(syms) => syms,
            Err(IndexerError::UnsupportedLanguage { .. }) => {
                return Ok(Vec::new());
            }
            Err(IndexerError::Parse { ref message, .. }) => {
                warn!("Parse warning for {}: {}", file_path, message);
                // Return empty on parse errors; still try to parse what we can
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };

        if symbols.is_empty() {
            return Ok(Vec::new());
        }

        // Compute file hash from content
        let content = std::fs::read_to_string(path)?;
        let file_hash = SymbolStorage::hash_content(&content);

        self.storage
            .replace_file_symbols(&file_path, &file_hash, &symbols)?;

        Ok(symbols)
    }

    /// 递归索引整个目录
    ///
    /// 【领域含义】递归遍历目录并索引所有支持的源文件。使用 ignore crate 自动
    /// 遵循 .gitignore 规则。返回索引统计信息（IndexStats），包括处理数、成功数、
    /// 跳过数和发现的符号总数。
    pub fn index_directory(&mut self, dir: &Path) -> Result<IndexStats, IndexerError> {
        let mut stats = IndexStats::default();

        let walker = ignore::WalkBuilder::new(dir)
            .git_ignore(true)
            .git_global(true)
            .hidden(false)
            .build();

        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(err) => {
                    stats.errors.push(format!("Walk error: {}", err));
                    continue;
                }
            };

            // Skip non-files and directories
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                continue;
            }

            let path = entry.path();
            stats.files_processed += 1;

            // Skip unsupported files
            if self.parser.detect_language(path).is_none() {
                stats.files_skipped += 1;
                continue;
            }

            match self.index_file(path) {
                Ok(symbols) => {
                    stats.files_indexed += 1;
                    stats.symbols_found += symbols.len();
                }
                Err(e) => {
                    stats.files_skipped += 1;
                    stats.errors.push(format!("{}: {}", path.display(), e));
                }
            }
        }

        Ok(stats)
    }

    /// 从索引中移除文件
    ///
    /// 【领域含义】从符号索引中移除指定文件的所有符号条目。用于文件删除或重命名时的
    /// 增量更新场景。
    pub fn remove_file(&mut self, path: &Path) -> Result<(), IndexerError> {
        let file_path = path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .display()
            .to_string();

        self.storage.remove_file(&file_path)?;
        Ok(())
    }

    /// 按名称搜索符号
    ///
    /// 【领域含义】基于符号名称的子串匹配搜索（不区分大小写）。返回匹配的符号条目列表，
    /// 可通过 limit 参数控制返回数量。用于 IDE 风格的符号补全和快速跳转。
    pub fn search_symbols(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SymbolEntry>, IndexerError> {
        self.storage.search_symbols(query, limit)
    }

    /// 获取所有已索引的符号
    ///
    /// 【领域含义】返回索引中所有符号条目的完整列表。用于图谱构建、全量导出等场景。
    pub fn get_all_symbols(&self) -> Result<Vec<SymbolEntry>, IndexerError> {
        self.storage.get_all_symbols()
    }

    /// 获取指定文件的所有符号
    ///
    /// 【领域含义】根据文件路径查询该文件中所有已索引的符号条目。用于增量更新时
    /// 比较变更前后的符号差异。
    pub fn get_file_symbols(&self, file_path: &str) -> Result<Vec<SymbolEntry>, IndexerError> {
        self.storage.get_file_symbols(file_path)
    }

    /// 获取索引符号总数
    ///
    /// 【领域含义】返回当前索引中所有符号的总数量。用于监控和调试目的。
    pub fn symbol_count(&self) -> Result<usize, IndexerError> {
        self.storage.symbol_count()
    }
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

    #[test]
    fn test_index_python_file() {
        let dir = TempDir::new().unwrap();
        let py_path = create_temp_file(
            &dir,
            "module.py",
            r#"
def hello():
    '''Say hello.'''
    pass

class Greeter:
    def greet(self):
        return "Hi"

import os
"#,
        );

        let db_path = dir.path().join("index.db");
        let mut indexer = CodeIndexer::new(&db_path.display().to_string()).unwrap();
        let symbols = indexer.index_file(&py_path).unwrap();

        assert!(!symbols.is_empty());
        let names: Vec<String> = symbols.iter().map(|s| s.symbol_name.clone()).collect();
        assert!(names.contains(&"hello".to_string()));
        assert!(names.contains(&"Greeter".to_string()));
        assert!(names.contains(&"greet".to_string()));
    }

    #[test]
    fn test_index_directory() {
        let dir = TempDir::new().unwrap();
        create_temp_file(
            &dir,
            "a.py",
            "def foo():\n    pass\n\nclass Bar:\n    pass\n",
        );
        create_temp_file(
            &dir,
            "b.py",
            "def baz():\n    pass\n\nimport sys\n",
        );
        create_temp_file(
            &dir,
            "lib.rs",
            "pub struct Point { x: f64, y: f64 }\n\npub fn distance() -> f64 { 0.0 }\n",
        );
        // Unsupported file
        create_temp_file(&dir, "README.md", "# Hello\n");

        // Create db OUTSIDE the indexed directory so it's not walked
        let db_dir = TempDir::new().unwrap();
        let db_path = db_dir.path().join("index.db");
        let mut indexer = CodeIndexer::new(&db_path.display().to_string()).unwrap();
        let stats = indexer.index_directory(dir.path()).unwrap();

        assert_eq!(stats.files_processed, 4);
        assert_eq!(stats.files_indexed, 3);
        assert_eq!(stats.files_skipped, 1);
        assert!(stats.symbols_found > 0);
    }

    #[test]
    fn test_search_symbols() {
        let dir = TempDir::new().unwrap();
        create_temp_file(
            &dir,
            "auth.py",
            "def authenticate():\n    pass\n\ndef authorize():\n    pass\n",
        );
        create_temp_file(
            &dir,
            "data.py",
            "def query_data():\n    pass\n",
        );

        let db_path = dir.path().join("index.db");
        let mut indexer = CodeIndexer::new(&db_path.display().to_string()).unwrap();
        indexer.index_directory(dir.path()).unwrap();

        let results = indexer.search_symbols("auth", 10).unwrap();
        assert_eq!(results.len(), 2);
        let names: Vec<String> = results.iter().map(|s| s.symbol_name.clone()).collect();
        assert!(names.contains(&"authenticate".to_string()));
        assert!(names.contains(&"authorize".to_string()));
    }

    #[test]
    fn test_remove_file() {
        let dir = TempDir::new().unwrap();
        let py_path = create_temp_file(
            &dir,
            "temp.py",
            "def temp_func():\n    pass\n",
        );

        let db_path = dir.path().join("index.db");
        let mut indexer = CodeIndexer::new(&db_path.display().to_string()).unwrap();
        indexer.index_file(&py_path).unwrap();
        assert!(indexer.symbol_count().unwrap() > 0);

        indexer.remove_file(&py_path).unwrap();
        assert_eq!(indexer.symbol_count().unwrap(), 0);
    }

    #[test]
    fn test_incremental_reindex() {
        let dir = TempDir::new().unwrap();
        let py_path = create_temp_file(
            &dir,
            "module.py",
            "def func_a():\n    pass\n",
        );

        let db_path = dir.path().join("index.db");
        let mut indexer = CodeIndexer::new(&db_path.display().to_string()).unwrap();

        // First index
        indexer.index_file(&py_path).unwrap();
        let count1 = indexer.symbol_count().unwrap();
        assert_eq!(count1, 1);

        // Re-index after file changes
        std::fs::write(
            &py_path,
            "def func_a():\n    pass\n\ndef func_b():\n    pass\n",
        )
        .unwrap();
        indexer.index_file(&py_path).unwrap();
        let count2 = indexer.symbol_count().unwrap();
        assert_eq!(count2, 2, "Re-index should update symbols without duplicates");
    }

    #[test]
    fn test_unsupported_file_skipped() {
        let dir = TempDir::new().unwrap();
        let path = create_temp_file(&dir, "notes.txt", "some text");

        let db_path = dir.path().join("index.db");
        let mut indexer = CodeIndexer::new(&db_path.display().to_string()).unwrap();
        let symbols = indexer.index_file(&path).unwrap();

        // Should return empty Vec, not error
        assert!(symbols.is_empty());
    }
}
