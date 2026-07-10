use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use rusqlite::{params, Connection, Result as SqlResult};

use super::symbol::{SymbolEntry, SymbolKind};

/// 符号存储（仓储）
///
/// 【领域含义】基于 SQLite 的代码符号仓储，是"代码解析与索引"限界上下文中的
/// 基础设施层组件。负责符号条目的持久化、查询和增量更新，提供原子替换、
/// 内容哈希去重等能力。
pub struct SymbolStorage {
    conn: Connection,
}

impl SymbolStorage {
    /// 打开或创建 SQLite 数据库
    ///
    /// 【领域含义】工厂方法，在指定路径打开或创建 SQLite 数据库，并初始化符号表结构。
    pub fn new(db_path: &str) -> Result<Self, crate::indexer::IndexerError> {
        let conn = Connection::open(db_path)?;
        let storage = Self { conn };
        storage.init_schema()?;
        Ok(storage)
    }

    /// 创建内存数据库
    ///
    /// 【领域含义】创建基于内存的 SQLite 数据库，主要用于测试场景。
    /// 数据不会持久化到磁盘。
    pub fn new_in_memory() -> Result<Self, crate::indexer::IndexerError> {
        let conn = Connection::open_in_memory()?;
        let storage = Self { conn };
        storage.init_schema()?;
        Ok(storage)
    }

    fn init_schema(&self) -> Result<(), crate::indexer::IndexerError> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS symbols (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                symbol_name TEXT NOT NULL,
                symbol_kind TEXT NOT NULL,
                file_path TEXT NOT NULL,
                line_start INTEGER NOT NULL,
                line_end INTEGER NOT NULL,
                col_start INTEGER NOT NULL,
                col_end INTEGER NOT NULL,
                doc_comment TEXT,
                language TEXT NOT NULL,
                signature TEXT,
                file_hash TEXT NOT NULL,
                created_at TEXT DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(symbol_name);
            CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_path);
            CREATE INDEX IF NOT EXISTS idx_symbols_lang ON symbols(language);",
        )?;
        Ok(())
    }

    /// 计算文件内容哈希
    ///
    /// 【领域含义】计算文件内容的哈希值，用于增量索引时的内容去重。
    /// 如果文件内容未变化则跳过重新索引，避免不必要的 I/O 操作。
    pub fn hash_content(content: &str) -> String {
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        format!("{:x}", hasher.finish())
    }

    /// 原子替换文件符号
    ///
    /// 【领域含义】先删除指定文件的所有旧符号，再批量插入新符号。通过事务保证原子性，
    /// 实现无重复的增量重新索引。这是增量更新的核心操作。
    pub fn replace_file_symbols(
        &self,
        file_path: &str,
        file_hash: &str,
        symbols: &[SymbolEntry],
    ) -> Result<usize, crate::indexer::IndexerError> {
        // Delete existing symbols for this file
        self.conn.execute(
            "DELETE FROM symbols WHERE file_path = ?1",
            params![file_path],
        )?;

        // Insert new symbols
        let mut count = 0;
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO symbols (symbol_name, symbol_kind, file_path, line_start, line_end,
                 col_start, col_end, doc_comment, language, signature, file_hash)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            )?;

            for sym in symbols {
                stmt.execute(params![
                    sym.symbol_name,
                    sym.symbol_kind.as_str(),
                    file_path,
                    sym.line_range.start as i64,
                    sym.line_range.end as i64,
                    sym.column_range.start as i64,
                    sym.column_range.end as i64,
                    sym.doc_comment,
                    sym.language,
                    sym.signature,
                    file_hash,
                ])?;
                count += 1;
            }
        }
        tx.commit()?;
        Ok(count)
    }

    /// 移除文件的所有符号
    ///
    /// 【领域含义】从索引中删除指定文件的所有符号条目。返回被删除的行数。
    /// 用于文件删除或重命名时的索引清理。
    pub fn remove_file(&self, file_path: &str) -> Result<usize, crate::indexer::IndexerError> {
        let deleted = self
            .conn
            .execute("DELETE FROM symbols WHERE file_path = ?1", params![file_path])?;
        Ok(deleted)
    }

    /// 按名称搜索符号
    ///
    /// 【领域含义】基于 SQL LIKE 操作符的符号名称模糊搜索（不区分大小写）。
    /// 返回匹配的符号条目列表，按名称排序，支持 limit 限制返回数量。
    pub fn search_symbols(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SymbolEntry>, crate::indexer::IndexerError> {
        let pattern = format!("%{}%", query);
        let mut stmt = self.conn.prepare(
            "SELECT symbol_name, symbol_kind, file_path, line_start, line_end,
                    col_start, col_end, doc_comment, language, signature
             FROM symbols
             WHERE symbol_name LIKE ?1
             ORDER BY symbol_name
             LIMIT ?2",
        )?;

        let rows = stmt.query_map(params![pattern, limit as i64], |row| {
            Ok(SymbolEntry {
                symbol_name: row.get(0)?,
                symbol_kind: SymbolKind::from_str(&row.get::<_, String>(1)?)
                    .unwrap_or(SymbolKind::Function),
                file_path: row.get(2)?,
                line_range: (row.get::<_, i64>(3)? as usize)
                    ..(row.get::<_, i64>(4)? as usize),
                column_range: (row.get::<_, i64>(5)? as usize)
                    ..(row.get::<_, i64>(6)? as usize),
                doc_comment: row.get(7)?,
                language: row.get(8)?,
                signature: row.get(9)?,
            })
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// 获取所有符号
    ///
    /// 【领域含义】返回索引中所有符号条目的完整列表，按符号名称排序。
    pub fn get_all_symbols(
        &self,
    ) -> Result<Vec<SymbolEntry>, crate::indexer::IndexerError> {
        let mut stmt = self.conn.prepare(
            "SELECT symbol_name, symbol_kind, file_path, line_start, line_end,
                    col_start, col_end, doc_comment, language, signature
             FROM symbols
             ORDER BY symbol_name",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(SymbolEntry {
                symbol_name: row.get(0)?,
                symbol_kind: SymbolKind::from_str(&row.get::<_, String>(1)?)
                    .unwrap_or(SymbolKind::Function),
                file_path: row.get(2)?,
                line_range: (row.get::<_, i64>(3)? as usize)
                    ..(row.get::<_, i64>(4)? as usize),
                column_range: (row.get::<_, i64>(5)? as usize)
                    ..(row.get::<_, i64>(6)? as usize),
                doc_comment: row.get(7)?,
                language: row.get(8)?,
                signature: row.get(9)?,
            })
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// 获取指定文件的所有符号
    ///
    /// 【领域含义】根据文件路径查询该文件中所有已索引的符号条目，按符号名称排序。
    pub fn get_file_symbols(
        &self,
        file_path: &str,
    ) -> Result<Vec<SymbolEntry>, crate::indexer::IndexerError> {
        let mut stmt = self.conn.prepare(
            "SELECT symbol_name, symbol_kind, file_path, line_start, line_end,
                    col_start, col_end, doc_comment, language, signature
             FROM symbols
             WHERE file_path = ?1
             ORDER BY symbol_name",
        )?;

        let rows = stmt.query_map(params![file_path], |row| {
            Ok(SymbolEntry {
                symbol_name: row.get(0)?,
                symbol_kind: SymbolKind::from_str(&row.get::<_, String>(1)?)
                    .unwrap_or(SymbolKind::Function),
                file_path: row.get(2)?,
                line_range: (row.get::<_, i64>(3)? as usize)
                    ..(row.get::<_, i64>(4)? as usize),
                column_range: (row.get::<_, i64>(5)? as usize)
                    ..(row.get::<_, i64>(6)? as usize),
                doc_comment: row.get(7)?,
                language: row.get(8)?,
                signature: row.get(9)?,
            })
        })?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// 获取索引符号总数
    ///
    /// 【领域含义】返回当前索引中所有符号的总数量。
    pub fn symbol_count(&self) -> Result<usize, crate::indexer::IndexerError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// 获取文件哈希值
    ///
    /// 【领域含义】查询指定文件在索引中存储的内容哈希值，用于增量更新时判断
    /// 文件内容是否发生变化。
    pub fn get_file_hash(
        &self,
        file_path: &str,
    ) -> Result<Option<String>, crate::indexer::IndexerError> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT file_hash FROM symbols WHERE file_path = ?1 LIMIT 1")?;
        let result: SqlResult<String> =
            stmt.query_row(params![file_path], |row| row.get(0));
        match result {
            Ok(hash) => Ok(Some(hash)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::indexer::symbol::{SymbolEntry, SymbolKind};

    fn make_entry(name: &str, kind: SymbolKind) -> SymbolEntry {
        SymbolEntry {
            symbol_name: name.to_string(),
            symbol_kind: kind,
            file_path: "test.py".to_string(),
            line_range: 1..2,
            column_range: 0..5,
            doc_comment: None,
            language: "python".to_string(),
            signature: None,
        }
    }

    #[test]
    fn test_insert_and_count() {
        let storage = SymbolStorage::new_in_memory().unwrap();
        let symbols = vec![
            make_entry("foo", SymbolKind::Function),
            make_entry("Bar", SymbolKind::Class),
            make_entry("baz", SymbolKind::Method),
        ];
        storage
            .replace_file_symbols("test.py", "hash1", &symbols)
            .unwrap();
        assert_eq!(storage.symbol_count().unwrap(), 3);
    }

    #[test]
    fn test_replace_avoids_duplicates() {
        let storage = SymbolStorage::new_in_memory().unwrap();
        let symbols1 = vec![make_entry("foo", SymbolKind::Function)];
        storage
            .replace_file_symbols("test.py", "hash1", &symbols1)
            .unwrap();

        // Re-index with different hash → should replace, not duplicate
        let symbols2 = vec![
            make_entry("foo", SymbolKind::Function),
            make_entry("bar", SymbolKind::Class),
        ];
        storage
            .replace_file_symbols("test.py", "hash2", &symbols2)
            .unwrap();
        assert_eq!(storage.symbol_count().unwrap(), 2);
    }

    #[test]
    fn test_search_symbols() {
        let storage = SymbolStorage::new_in_memory().unwrap();
        let symbols = vec![
            make_entry("hello_world", SymbolKind::Function),
            make_entry("HelloClass", SymbolKind::Class),
            make_entry("goodbye", SymbolKind::Function),
        ];
        storage
            .replace_file_symbols("test.py", "hash1", &symbols)
            .unwrap();

        let results = storage.search_symbols("hello", 10).unwrap();
        assert_eq!(results.len(), 2);
        let names: Vec<&str> = results.iter().map(|s| s.symbol_name.as_str()).collect();
        assert!(names.contains(&"hello_world"));
        assert!(names.contains(&"HelloClass"));
    }

    #[test]
    fn test_remove_file() {
        let storage = SymbolStorage::new_in_memory().unwrap();
        let symbols = vec![make_entry("foo", SymbolKind::Function)];
        storage
            .replace_file_symbols("test.py", "hash1", &symbols)
            .unwrap();
        assert_eq!(storage.symbol_count().unwrap(), 1);

        storage.remove_file("test.py").unwrap();
        assert_eq!(storage.symbol_count().unwrap(), 0);
    }
}
