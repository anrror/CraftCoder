//! BM25 lexical search via SQLite FTS5.
//!
//! Builds a full-text search index over the `symbols` table using FTS5's
//! content-sync mode, then queries it with BM25 ranking.

use rusqlite::{params, Connection, Result as SqlResult};

use super::{RetrievalResult, ScoredResult};

/// BM25 词法搜索器（领域服务）
///
/// 【领域含义】基于 SQLite FTS5 的 BM25 关键词搜索领域服务。FTS5 虚拟表通过
/// `content='symbols'` 模式与符号表保持同步，对 symbols 表的插入/删除会自动反映。
/// 属于"代码检索"限界上下文中的词法检索组件。
pub struct Bm25Searcher {
    db: Connection,
}

impl Bm25Searcher {
    /// 创建 BM25 搜索器
    ///
    /// 【领域含义】基于已有 SQLite 连接创建 BM25 搜索器。连接应已包含索引器创建的
    /// symbols 表。首次搜索前需调用 create_fts_index 初始化 FTS5 索引。
    pub fn new(db: Connection) -> RetrievalResult<Self> {
        Ok(Self { db })
    }

    /// 创建 FTS5 全文索引
    ///
    /// 【领域含义】在 symbols 表上创建 FTS5 虚拟表。使用 IF NOT EXISTS 保证幂等性。
    /// 首次创建时会自动从 symbols 表重建索引数据。
    pub fn create_fts_index(&self) -> RetrievalResult<()> {
        self.db.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS symbols_fts USING fts5(
                symbol_name,
                doc_comment,
                file_path,
                content='symbols',
                content_rowid='id'
            );",
        )?;

        // Populate the FTS index from existing data (idempotent for new tables)
        self.db.execute_batch(
            "INSERT INTO symbols_fts(symbols_fts) VALUES('rebuild');",
        )?;

        Ok(())
    }

    /// 执行 BM25 搜索
    ///
    /// 【领域含义】使用 BM25 排名算法搜索匹配查询的符号。返回按 BM25 评分降序排列的结果。
    pub fn search(&self, query: &str, limit: usize) -> RetrievalResult<Vec<ScoredResult>> {
        self.search_internal(query, None, limit)
    }

    /// 按语言过滤搜索
    ///
    /// 【领域含义】在 BM25 搜索基础上增加语言过滤条件，只返回指定语言的符号。
    pub fn search_filtered(
        &self,
        query: &str,
        language: &str,
        limit: usize,
    ) -> RetrievalResult<Vec<ScoredResult>> {
        self.search_internal(query, Some(language), limit)
    }

    /// Shared implementation for search / search_filtered.
    fn search_internal(
        &self,
        query: &str,
        language: Option<&str>,
        limit: usize,
    ) -> RetrievalResult<Vec<ScoredResult>> {
        // Escape FTS5 special characters: surround the query with double-quotes
        // for phrase matching OR use simple syntax.
        let fts_query = Self::sanitize_fts_query(query);

        let sql = match language {
            Some(_) => {
                "SELECT s.symbol_name, s.symbol_kind, s.file_path, s.line_start, s.line_end,
                        s.language, bm25(symbols_fts) AS score
                 FROM symbols_fts f
                 JOIN symbols s ON s.id = f.rowid
                 WHERE symbols_fts MATCH ?1
                   AND s.language = ?2
                 ORDER BY score
                 LIMIT ?3"
            }
            None => {
                "SELECT s.symbol_name, s.symbol_kind, s.file_path, s.line_start, s.line_end,
                        s.language, bm25(symbols_fts) AS score
                 FROM symbols_fts f
                 JOIN symbols s ON s.id = f.rowid
                 WHERE symbols_fts MATCH ?1
                 ORDER BY score
                 LIMIT ?2"
            }
        };

        let rows: Vec<ScoredResult> = if let Some(lang) = language {
            let mut stmt = self.db.prepare(sql)?;
            let mapped = stmt.query_map(params![fts_query, lang, limit as i64], |row| {
                Self::row_to_result(row, "bm25")
            })?;
            mapped.filter_map(|r| r.ok()).collect()
        } else {
            let mut stmt = self.db.prepare(sql)?;
            let mapped = stmt.query_map(params![fts_query, limit as i64], |row| {
                Self::row_to_result(row, "bm25")
            })?;
            mapped.filter_map(|r| r.ok()).collect()
        };

        Ok(rows)
    }

    /// Map a SQLite row to a [`ScoredResult`].
    fn row_to_result(row: &rusqlite::Row<'_>, source: &str) -> SqlResult<ScoredResult> {
        let score: f64 = row.get(6)?;
        // BM25 returns negative scores in FTS5; negate for intuitive ordering
        let score = -score;
        Ok(ScoredResult {
            symbol_name: row.get(0)?,
            symbol_kind: row.get(1)?,
            file_path: row.get(2)?,
            line_range: {
                let start: i64 = row.get(3)?;
                let end: i64 = row.get(4)?;
                (start as usize)..(end as usize)
            },
            score,
            source: source.to_string(),
        })
    }

    /// Sanitize a user query for FTS5 MATCH syntax.
    ///
    /// Escapes double-quotes and wraps multi-word queries in quotes for
    /// phrase matching. Single words are prefixed with `*` for prefix
    /// matching.
    fn sanitize_fts_query(query: &str) -> String {
        let escaped = query.replace('"', "\"\"");
        let trimmed = escaped.trim();
        if trimmed.is_empty() {
            return "\"\"".to_string();
        }
        if trimmed.contains(' ') {
            // Multi-word: phrase match
            format!("\"{}\"", trimmed)
        } else {
            // Single word: prefix match
            format!("\"{}\"*", trimmed)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE symbols (
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
            INSERT INTO symbols (symbol_name, symbol_kind, file_path, line_start, line_end,
                                col_start, col_end, doc_comment, language, file_hash)
            VALUES
                ('find_user_by_email', 'function', 'src/auth.py', 10, 15, 0, 4,
                 'Find a user by their email address', 'python', 'abc'),
                ('authenticate', 'function', 'src/auth.py', 1, 8, 0, 4,
                 'Authenticate the user credentials', 'python', 'abc'),
                ('User', 'class', 'src/models.py', 1, 20, 0, 4,
                 'User model class', 'python', 'def'),
                ('findUser', 'function', 'src/auth.ts', 5, 12, 0, 4,
                 'Find user by ID', 'typescript', 'ghi'),
                ('renderHtml', 'function', 'src/view.ts', 1, 10, 0, 4,
                 'Render HTML template', 'typescript', 'jkl');",
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_bm25_search_returns_results() {
        let conn = setup_test_db();
        let searcher = Bm25Searcher::new(conn).unwrap();
        searcher.create_fts_index().unwrap();

        let results = searcher.search("user", 10).unwrap();
        assert!(!results.is_empty(), "Should find results for 'user'");

        let names: Vec<&str> = results.iter().map(|r| r.symbol_name.as_str()).collect();
        assert!(names.contains(&"find_user_by_email"));
        assert!(names.contains(&"User"));
        assert!(names.contains(&"findUser"));
    }

    #[test]
    fn test_bm25_search_scores_are_positive() {
        let conn = setup_test_db();
        let searcher = Bm25Searcher::new(conn).unwrap();
        searcher.create_fts_index().unwrap();

        let results = searcher.search("auth", 10).unwrap();
        for r in &results {
            assert!(r.score >= 0.0, "Score should be non-negative: {}", r.score);
            assert_eq!(r.source, "bm25");
        }
    }

    #[test]
    fn test_bm25_filtered_by_language() {
        let conn = setup_test_db();
        let searcher = Bm25Searcher::new(conn).unwrap();
        searcher.create_fts_index().unwrap();

        let results = searcher.search_filtered("user", "typescript", 10).unwrap();
        assert!(!results.is_empty());
        for r in &results {
            // All results should be from typescript files
            assert!(
                r.file_path.ends_with(".ts"),
                "Expected .ts file, got: {}",
                r.file_path
            );
        }
    }

    #[test]
    fn test_bm25_empty_corpus() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE symbols (
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
                file_hash TEXT NOT NULL
            );",
        )
        .unwrap();

        let searcher = Bm25Searcher::new(conn).unwrap();
        searcher.create_fts_index().unwrap();

        let results = searcher.search("anything", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_bm25_fts_rebuild_idempotent() {
        let conn = setup_test_db();
        let searcher = Bm25Searcher::new(conn).unwrap();

        // Call twice — should not error
        searcher.create_fts_index().unwrap();
        searcher.create_fts_index().unwrap();

        let results = searcher.search("user", 10).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn test_sanitize_single_word() {
        let q = Bm25Searcher::sanitize_fts_query("findUser");
        assert_eq!(q, "\"findUser\"*");
    }

    #[test]
    fn test_sanitize_multi_word() {
        let q = Bm25Searcher::sanitize_fts_query("find user email");
        assert_eq!(q, "\"find user email\"");
    }

    #[test]
    fn test_sanitize_empty() {
        let q = Bm25Searcher::sanitize_fts_query("");
        assert_eq!(q, "\"\"");
    }

    #[test]
    fn test_search_with_no_match() {
        let conn = setup_test_db();
        let searcher = Bm25Searcher::new(conn).unwrap();
        searcher.create_fts_index().unwrap();

        let results = searcher.search("zzz_nonexistent_xyz", 10).unwrap();
        assert!(results.is_empty());
    }
}
