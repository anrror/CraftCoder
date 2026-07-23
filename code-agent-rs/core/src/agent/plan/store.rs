//! PlanStore —— Plan 执行数据持久化与飞轮分析
//!
//! 【领域含义】PlanStore 负责将 Plan 引擎的执行结果（ExecutionReport）持久化到
//! SQLite 数据库，并提供转换为 Flywheel ErrorTrace 的桥梁函数，实现"数据飞轮"
//! 闭环：执行 → 存储 → 分析 → 改进。
//!
//! # Architecture
//!
//! ```text
//! PlanExecutor.execute()
//!      ↓ ExecutionReport
//! PlanStore::store_execution()
//!      ↓ PlanExecutionRecord (SQLite)
//! PlanExecutionRecord::to_error_traces()
//!      ↓ Vec<ErrorTrace>
//! NightlyPipeline::run()
//!      ↓ Vec<Suggestion>
//! KnowledgeProvider::add_rules()
//!      ↓ (闭环：规则/技能注入到下一轮执行)
//! ```
//!
//! # 与 Flywheel 的关系
//!
//! PlanStore 是 Plan 引擎和 Flywheel 分析模块之间的桥梁：
//! - Collection: `store_execution()` 持久化执行报告
//! - Transformation: `to_error_traces()` 将失败节点转为 ErrorTrace
//! - Analysis: `store_and_run_flywheel()` 一站式执行全链路

use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, Result as SqliteResult};

use crate::agent::plan::types::ExecutionReport;
use crate::flywheel::pipeline::{NightlyPipeline, PipelineReport};
use crate::flywheel::ErrorTrace;

// ---------------------------------------------------------------------------
// PlanExecutionRecord
// ---------------------------------------------------------------------------

/// 一次计划执行的完整记录 —— 用于持久化分析和飞轮反馈。
///
/// 【领域含义】对应一次 PlanExecutor::execute() 调用的完整快照。
/// 包含计划级别摘要和每个节点的详细结果，支持转换为 ErrorTrace
/// 以接入 Flywheel 分析管道。
#[derive(Clone, Debug)]
pub struct PlanExecutionRecord {
    /// 数据库 ID
    pub id: i64,
    /// 计划名称
    pub plan_name: String,
    /// 关联的会话 ID（可选）
    pub session_id: Option<String>,
    /// 是否全部成功
    pub success: bool,
    /// 总节点数
    pub total_nodes: u32,
    /// 成功节点数
    pub completed_nodes: u32,
    /// 失败节点数
    pub failed_nodes: u32,
    /// 取消节点数
    pub cancelled_nodes: u32,
    /// 执行耗时（毫秒）
    pub duration_ms: u64,
    /// PlanConfig 的 JSON 快照（用于复现分析）
    pub plan_config_json: Option<String>,
    /// 记录创建时间
    pub created_at: DateTime<Utc>,
    /// 节点级详细记录
    pub nodes: Vec<PlanNodeRecord>,
}

/// 单个计划节点的执行记录。
#[derive(Clone, Debug)]
pub struct PlanNodeRecord {
    /// 数据库 ID
    pub id: i64,
    /// 节点标识
    pub node_id: String,
    /// 节点可读名称
    pub label: String,
    /// 所属阶段
    pub phase: String,
    /// 终态（Completed/Failed/Cancelled）
    pub status: String,
    /// 错误消息
    pub error: Option<String>,
    /// 重试次数
    pub retry_count: u32,
    /// 门禁裁决
    pub gate_verdict: Option<String>,
    /// 最终消息摘要
    pub final_message_summary: Option<String>,
}

impl PlanExecutionRecord {
    /// 将执行记录中的失败/取消节点转换为 Flywheel ErrorTrace 列表。
    ///
    /// 每条 ErrorTrace 对应一个失败或被取消的节点，error_type 使用
    /// "plan_node_failed"/"plan_node_cancelled" 前缀，便于 Flywheel 聚类。
    pub fn to_error_traces(&self) -> Vec<ErrorTrace> {
        let mut traces = Vec::new();
        for node in &self.nodes {
            let error_type = match node.status.as_str() {
                "Failed" => "plan_node_failed",
                "Cancelled" => "plan_node_cancelled",
                _ => continue, // 只关注失败/取消节点
            };
            let message = node
                .error
                .clone()
                .unwrap_or_else(|| format!("Node '{}' {}", node.label, node.status));
            let gate_ctx = node
                .gate_verdict
                .as_ref()
                .map(|v| format!(" (gate: {v})"))
                .unwrap_or_default();

            traces.push(ErrorTrace {
                error_type: error_type.to_string(),
                tool_name: "plan_executor".to_string(),
                language: "unknown".to_string(),
                file_pattern: String::new(),
                turn_count: 0,
                message: format!(
                    "[{}] Node '{}' (phase: {}) failed after {} retries{}: {}",
                    self.plan_name,
                    node.label,
                    node.phase,
                    node.retry_count,
                    gate_ctx,
                    message,
                ),
                timestamp: self.created_at,
            });
        }
        traces
    }
}

// ---------------------------------------------------------------------------
// PlanStore
// ---------------------------------------------------------------------------

/// Plan 执行数据存储 —— 基于 SQLite 的持久化 + Flywheel 分析入口。
///
/// 【领域含义】PlanStore 是数据飞轮的"记录层"。每个 Plan 执行完成后，
/// 调用 `store_execution()` 持久化结果，后续可通过 `run_flywheel()`
/// 触发聚类分析和改进建议生成。
///
/// # 使用示例
///
/// ```rust,ignore
/// let store = PlanStore::open_in_memory().unwrap();
/// let record_id = store.store_execution(&report, Some("session-1")).unwrap();
///
/// // 一站式飞轮分析
/// let pipeline = NightlyPipeline::new("plan_traces.json");
/// let pr = store.run_flywheel(&pipeline).unwrap();
/// println!("{} suggestions", pr.suggestions_count);
/// ```
pub struct PlanStore {
    conn: Connection,
}

impl PlanStore {
    /// 打开或创建 SQLite 数据库文件。
    pub fn open<P: AsRef<Path>>(path: P) -> SqliteResult<Self> {
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.initialize_schema()?;
        Ok(store)
    }

    /// 创建内存数据库（用于测试）。
    pub fn open_in_memory() -> SqliteResult<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.initialize_schema()?;
        Ok(store)
    }

    /// 初始化数据库表结构。
    fn initialize_schema(&self) -> SqliteResult<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS plan_executions (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                plan_name       TEXT NOT NULL,
                session_id      TEXT,
                success         INTEGER NOT NULL,
                total_nodes     INTEGER NOT NULL,
                completed_nodes INTEGER NOT NULL,
                failed_nodes    INTEGER NOT NULL,
                cancelled_nodes INTEGER NOT NULL,
                duration_ms     INTEGER NOT NULL,
                plan_config_json TEXT,
                created_at      TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS plan_nodes (
                id                   INTEGER PRIMARY KEY AUTOINCREMENT,
                execution_id         INTEGER NOT NULL REFERENCES plan_executions(id),
                node_id              TEXT NOT NULL,
                label                TEXT NOT NULL,
                phase                TEXT NOT NULL,
                status               TEXT NOT NULL,
                error                TEXT,
                retry_count          INTEGER NOT NULL DEFAULT 0,
                gate_verdict         TEXT,
                final_message_summary TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_plan_nodes_exec
                ON plan_nodes(execution_id);
            CREATE INDEX IF NOT EXISTS idx_plan_exec_time
                ON plan_executions(created_at);
            CREATE INDEX IF NOT EXISTS idx_plan_exec_plan_name
                ON plan_executions(plan_name);
            CREATE INDEX IF NOT EXISTS idx_plan_exec_session
                ON plan_executions(session_id);
            ",
        )?;
        Ok(())
    }

    /// 存储一次计划执行记录。
    ///
    /// 将 ExecutionReport 转换为 PlanExecutionRecord 并写入 SQLite。
    /// 返回新记录的 ID。
    pub fn store_execution(
        &self,
        report: &ExecutionReport,
        session_id: Option<&str>,
    ) -> SqliteResult<i64> {
        let now = Utc::now().to_rfc3339();

        // 插入 plan_executions 主记录
        self.conn.execute(
            "INSERT INTO plan_executions
                (plan_name, session_id, success, total_nodes, completed_nodes,
                 failed_nodes, cancelled_nodes, duration_ms, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                report.plan_name,
                session_id,
                report.success as i32,
                report.total_nodes as i64,
                report.completed_nodes as i64,
                report.failed_nodes as i64,
                report.cancelled_nodes as i64,
                report.duration_ms as i64,
                now,
            ],
        )?;

        let execution_id = self.conn.last_insert_rowid();

        // 插入 plan_nodes 明细
        for node in &report.node_summaries {
            self.conn.execute(
                "INSERT INTO plan_nodes
                    (execution_id, node_id, label, phase, status, error,
                     retry_count, gate_verdict, final_message_summary)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    execution_id,
                    node.id,
                    node.label,
                    node.phase,
                    format!("{:?}", node.status),
                    node.error,
                    node.retry_count as i64,
                    node.gate_verdict,
                    node.final_message_summary,
                ],
            )?;
        }

        Ok(execution_id)
    }

    /// 按 ID 查询执行记录。
    pub fn get_execution(&self, id: i64) -> SqliteResult<Option<PlanExecutionRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, plan_name, session_id, success, total_nodes, completed_nodes,
                    failed_nodes, cancelled_nodes, duration_ms, plan_config_json, created_at
             FROM plan_executions WHERE id = ?1",
        )?;

        let mut rows = stmt.query_map(params![id], |row| {
            Ok(PlanExecutionRecord {
                id: row.get(0)?,
                plan_name: row.get(1)?,
                session_id: row.get(2)?,
                success: row.get::<_, i32>(3)? != 0,
                total_nodes: row.get::<_, i64>(4)? as u32,
                completed_nodes: row.get::<_, i64>(5)? as u32,
                failed_nodes: row.get::<_, i64>(6)? as u32,
                cancelled_nodes: row.get::<_, i64>(7)? as u32,
                duration_ms: row.get::<_, i64>(8)? as u64,
                plan_config_json: row.get(9)?,
                created_at: {
                    let s: String = row.get(10)?;
                    DateTime::parse_from_rfc3339(&s)
                        .map(|d| d.with_timezone(&Utc))
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
                },
                nodes: Vec::new(), // 下面单独加载
            })
        })?;

        if let Some(record) = rows.next() {
            let mut record = record?;
            record.nodes = self.load_nodes(record.id)?;
            Ok(Some(record))
        } else {
            Ok(None)
        }
    }

    /// 加载指定执行记录的节点列表。
    fn load_nodes(&self, execution_id: i64) -> SqliteResult<Vec<PlanNodeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, node_id, label, phase, status, error, retry_count, gate_verdict,
                    final_message_summary
             FROM plan_nodes WHERE execution_id = ?1 ORDER BY id",
        )?;

        let nodes = stmt
            .query_map(params![execution_id], |row| {
                Ok(PlanNodeRecord {
                    id: row.get(0)?,
                    node_id: row.get(1)?,
                    label: row.get(2)?,
                    phase: row.get(3)?,
                    status: row.get(4)?,
                    error: row.get(5)?,
                    retry_count: row.get::<_, i64>(6)? as u32,
                    gate_verdict: row.get(7)?,
                    final_message_summary: row.get(8)?,
                })
            })?
            .collect::<SqliteResult<Vec<_>>>()?;

        Ok(nodes)
    }

    /// 列出最近的执行记录。
    pub fn list_executions(&self, limit: usize) -> SqliteResult<Vec<PlanExecutionRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, plan_name, session_id, success, total_nodes, completed_nodes,
                    failed_nodes, cancelled_nodes, duration_ms, plan_config_json, created_at
             FROM plan_executions ORDER BY id DESC LIMIT ?1",
        )?;

        let records = stmt
            .query_map(params![limit as i64], |row| {
                let id: i64 = row.get(0)?;
                let plan_name: String = row.get(1)?;
                let session_id: Option<String> = row.get(2)?;
                let success: bool = row.get::<_, i32>(3)? != 0;
                let total_nodes: u32 = row.get::<_, i64>(4)? as u32;
                let completed_nodes: u32 = row.get::<_, i64>(5)? as u32;
                let failed_nodes: u32 = row.get::<_, i64>(6)? as u32;
                let cancelled_nodes: u32 = row.get::<_, i64>(7)? as u32;
                let duration_ms: u64 = row.get::<_, i64>(8)? as u64;
                let plan_config_json: Option<String> = row.get(9)?;
                let created_at: String = row.get(10)?;
                let created_at = DateTime::parse_from_rfc3339(&created_at)
                    .map(|d| d.with_timezone(&Utc))
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

                Ok((id, plan_name, session_id, success, total_nodes, completed_nodes,
                    failed_nodes, cancelled_nodes, duration_ms, plan_config_json, created_at))
            })?
            .collect::<SqliteResult<Vec<_>>>()?;

        let mut result = Vec::with_capacity(records.len());
        for (id, plan_name, session_id, success, total_nodes, completed_nodes,
             failed_nodes, cancelled_nodes, duration_ms, plan_config_json, created_at) in records
        {
            let nodes = self.load_nodes(id)?;
            result.push(PlanExecutionRecord {
                id,
                plan_name,
                session_id,
                success,
                total_nodes,
                completed_nodes,
                failed_nodes,
                cancelled_nodes,
                duration_ms,
                plan_config_json,
                created_at,
                nodes,
            });
        }
        Ok(result)
    }

    /// 获取失败的执行记录（有 Failed 节点的计划）。
    pub fn get_failed_executions(&self, limit: usize) -> SqliteResult<Vec<PlanExecutionRecord>> {
        // 先找失败的 plan_executions ID
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT e.id FROM plan_executions e
             INNER JOIN plan_nodes n ON n.execution_id = e.id
             WHERE n.status = 'Failed'
             ORDER BY e.id DESC
             LIMIT ?1",
        )?;

        let ids: Vec<i64> = stmt
            .query_map(params![limit as i64], |row| row.get(0))?
            .collect::<SqliteResult<Vec<_>>>()?;

        let mut records = Vec::new();
        for id in ids {
            if let Some(record) = self.get_execution(id)? {
                records.push(record);
            }
        }
        Ok(records)
    }

    /// 一站式飞轮分析：加载所有失败记录 → 转为 ErrorTrace → 运行 Flywheel 管道。
    ///
    /// 返回 PipelineReport，包含聚类结果和改进建议。
    /// 这是"数据飞轮闭环"的核心入口 —— 将 Plan 执行数据送入 Flywheel 分析。
    pub fn run_flywheel(&self, pipeline: &NightlyPipeline) -> SqliteResult<PipelineReport> {
        let failed = self.get_failed_executions(100)?;

        let mut all_traces = Vec::new();
        for record in &failed {
            all_traces.extend(record.to_error_traces());
        }

        let report = pipeline.run(all_traces);
        Ok(report)
    }

    /// 获取存储中的执行记录总数。
    pub fn count(&self) -> SqliteResult<u64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM plan_executions", [], |row| row.get(0))
    }
}

// ---------------------------------------------------------------------------
// Bridge: ExecutionReport → PlanExecutionRecord（不持久化，仅转换）
// ---------------------------------------------------------------------------

/// 将 ExecutionReport 转换为 PlanExecutionRecord（内存级，不写入数据库）。
///
/// 可用于实时分析或自定义持久化。
pub fn execution_report_to_record(
    report: &ExecutionReport,
    session_id: Option<&str>,
) -> PlanExecutionRecord {
    let nodes = report
        .node_summaries
        .iter()
        .map(|ns| PlanNodeRecord {
            id: 0, // 未持久化
            node_id: ns.id.clone(),
            label: ns.label.clone(),
            phase: ns.phase.clone(),
            status: format!("{:?}", ns.status),
            error: ns.error.clone(),
            retry_count: ns.retry_count,
            gate_verdict: ns.gate_verdict.clone(),
            final_message_summary: ns.final_message_summary.clone(),
        })
        .collect();

    PlanExecutionRecord {
        id: 0,
        plan_name: report.plan_name.clone(),
        session_id: session_id.map(String::from),
        success: report.success,
        total_nodes: report.total_nodes as u32,
        completed_nodes: report.completed_nodes as u32,
        failed_nodes: report.failed_nodes as u32,
        cancelled_nodes: report.cancelled_nodes as u32,
        duration_ms: report.duration_ms as u64,
        plan_config_json: None,
        created_at: Utc::now(),
        nodes,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::plan::types::{ExecutionReport, NodeStatus, NodeSummary};

    fn sample_report() -> ExecutionReport {
        ExecutionReport {
            plan_name: "test-plan".into(),
            total_nodes: 3,
            completed_nodes: 1,
            failed_nodes: 1,
            cancelled_nodes: 1,
            duration_ms: 1500,
            node_summaries: vec![
                NodeSummary {
                    id: "task-1".into(),
                    label: "Setup".into(),
                    phase: "Phase 0".into(),
                    status: NodeStatus::Completed,
                    error: None,
                    final_message_summary: Some("Done".into()),
                    retry_count: 0,
                    gate_verdict: None,
                },
                NodeSummary {
                    id: "task-2".into(),
                    label: "Build".into(),
                    phase: "Phase 1".into(),
                    status: NodeStatus::Failed,
                    error: Some("Compilation error".into()),
                    final_message_summary: None,
                    retry_count: 2,
                    gate_verdict: Some("Fail { reason: \"spec mismatch\" }".into()),
                },
                NodeSummary {
                    id: "task-3".into(),
                    label: "Deploy".into(),
                    phase: "Phase 2".into(),
                    status: NodeStatus::Cancelled,
                    error: Some("Dependency failed".into()),
                    final_message_summary: None,
                    retry_count: 0,
                    gate_verdict: None,
                },
            ],
            success: false,
        }
    }

    // ── PlanStore tests ──────────────────────────────────────────────

    #[test]
    fn test_store_open_in_memory() {
        let store = PlanStore::open_in_memory().unwrap();
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn test_store_execution_and_count() {
        let store = PlanStore::open_in_memory().unwrap();
        let report = sample_report();

        let id = store
            .store_execution(&report, Some("session-1"))
            .unwrap();
        assert!(id > 0);
        assert_eq!(store.count().unwrap(), 1);
    }

    #[test]
    fn test_store_get_execution() {
        let store = PlanStore::open_in_memory().unwrap();
        let report = sample_report();

        let id = store.store_execution(&report, Some("session-1")).unwrap();
        let loaded = store.get_execution(id).unwrap().unwrap();

        assert_eq!(loaded.plan_name, "test-plan");
        assert!(!loaded.success);
        assert_eq!(loaded.total_nodes, 3);
        assert_eq!(loaded.completed_nodes, 1);
        assert_eq!(loaded.failed_nodes, 1);
        assert_eq!(loaded.cancelled_nodes, 1);
        assert_eq!(loaded.duration_ms, 1500);
        assert_eq!(loaded.session_id.as_deref(), Some("session-1"));
        assert_eq!(loaded.nodes.len(), 3);
    }

    #[test]
    fn test_store_get_execution_not_found() {
        let store = PlanStore::open_in_memory().unwrap();
        let loaded = store.get_execution(999).unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn test_store_list_executions() {
        let store = PlanStore::open_in_memory().unwrap();
        let report = sample_report();

        store.store_execution(&report, None).unwrap();
        store.store_execution(&report, None).unwrap();

        let list = store.list_executions(10).unwrap();
        assert_eq!(list.len(), 2);

        let list_one = store.list_executions(1).unwrap();
        assert_eq!(list_one.len(), 1);
    }

    #[test]
    fn test_store_get_failed_executions() {
        let store = PlanStore::open_in_memory().unwrap();

        // Store a success report
        let success_report = ExecutionReport {
            plan_name: "success".into(),
            total_nodes: 1,
            completed_nodes: 1,
            failed_nodes: 0,
            cancelled_nodes: 0,
            duration_ms: 100,
            node_summaries: vec![NodeSummary {
                id: "t1".into(),
                label: "Task 1".into(),
                phase: "P0".into(),
                status: NodeStatus::Completed,
                error: None,
                final_message_summary: Some("ok".into()),
                retry_count: 0,
                gate_verdict: None,
            }],
            success: true,
        };
        store.store_execution(&success_report, None).unwrap();
        assert_eq!(store.get_failed_executions(10).unwrap().len(), 0);

        // Store a failure report
        let fail_report = sample_report();
        store.store_execution(&fail_report, None).unwrap();
        assert_eq!(store.get_failed_executions(10).unwrap().len(), 1);
    }

    // ── Bridge tests ────────────────────────────────────────────────

    #[test]
    fn test_execution_report_to_record() {
        let report = sample_report();
        let record = execution_report_to_record(&report, Some("ses-1"));

        assert_eq!(record.plan_name, "test-plan");
        assert_eq!(record.nodes.len(), 3);
        assert_eq!(record.session_id.as_deref(), Some("ses-1"));
        assert_eq!(record.id, 0); // not persisted
    }

    #[test]
    fn test_execution_report_to_record_no_session() {
        let report = sample_report();
        let record = execution_report_to_record(&report, None);
        assert!(record.session_id.is_none());
    }

    // ── ErrorTrace bridge tests ─────────────────────────────────────

    #[test]
    fn test_to_error_traces_skips_completed() {
        let report = sample_report();
        let record = execution_report_to_record(&report, None);
        let traces = record.to_error_traces();

        // Only failed + cancelled nodes (task-2 and task-3)
        assert_eq!(traces.len(), 2);
    }

    #[test]
    fn test_to_error_traces_full_success_yields_empty() {
        let report = ExecutionReport {
            plan_name: "all-good".into(),
            total_nodes: 2,
            completed_nodes: 2,
            failed_nodes: 0,
            cancelled_nodes: 0,
            duration_ms: 500,
            node_summaries: vec![
                NodeSummary {
                    id: "t1".into(),
                    label: "Task 1".into(),
                    phase: "P0".into(),
                    status: NodeStatus::Completed,
                    error: None,
                    final_message_summary: Some("ok".into()),
                    retry_count: 0,
                    gate_verdict: None,
                },
                NodeSummary {
                    id: "t2".into(),
                    label: "Task 2".into(),
                    phase: "P0".into(),
                    status: NodeStatus::Completed,
                    error: None,
                    final_message_summary: Some("done".into()),
                    retry_count: 0,
                    gate_verdict: None,
                },
            ],
            success: true,
        };
        let record = execution_report_to_record(&report, None);
        let traces = record.to_error_traces();
        assert!(traces.is_empty());
    }

    #[test]
    fn test_to_error_traces_has_plan_context() {
        let report = sample_report();
        let record = execution_report_to_record(&report, None);
        let traces = record.to_error_traces();

        // First trace should be the Failed node
        let failed_trace = traces.iter().find(|t| t.error_type == "plan_node_failed").unwrap();
        assert!(failed_trace.message.contains("test-plan"));
        assert!(failed_trace.message.contains("Build"));
        assert!(failed_trace.message.contains("Compilation error"));
        assert!(failed_trace.message.contains("gate: Fail"));

        // Second trace should be Cancelled
        let cancelled_trace = traces.iter().find(|t| t.error_type == "plan_node_cancelled").unwrap();
        assert!(cancelled_trace.message.contains("Deploy"));
        assert!(cancelled_trace.message.contains("Dependency failed"));
    }

    #[test]
    fn test_to_error_traces_fields() {
        let report = sample_report();
        let record = execution_report_to_record(&report, None);
        let traces = record.to_error_traces();

        for trace in &traces {
            assert_eq!(trace.tool_name, "plan_executor");
            assert_eq!(trace.language, "unknown");
            assert_eq!(trace.turn_count, 0);
        }
    }

    // ── Integration: store + flywheel ───────────────────────────────

    #[test]
    fn test_store_run_flywheel() {
        let store = PlanStore::open_in_memory().unwrap();
        let report = sample_report();
        store.store_execution(&report, Some("session-flywheel")).unwrap();

        let pipeline = NightlyPipeline::new("test_traces.json");
        let pr = store.run_flywheel(&pipeline).unwrap();

        // Should find the 2 error traces from the failed report
        assert_eq!(pr.traces_processed, 2);
        assert!(pr.clusters_found >= 1);
        assert!(pr.suggestions_count > 0);
    }

    #[test]
    fn test_store_run_flywheel_no_failures() {
        let store = PlanStore::open_in_memory().unwrap();
        let success_report = ExecutionReport {
            plan_name: "all-pass".into(),
            total_nodes: 1,
            completed_nodes: 1,
            failed_nodes: 0,
            cancelled_nodes: 0,
            duration_ms: 50,
            node_summaries: vec![NodeSummary {
                id: "t1".into(),
                label: "Task 1".into(),
                phase: "P0".into(),
                status: NodeStatus::Completed,
                error: None,
                final_message_summary: Some("ok".into()),
                retry_count: 0,
                gate_verdict: None,
            }],
            success: true,
        };
        store.store_execution(&success_report, None).unwrap();

        let pipeline = NightlyPipeline::new("empty.json");
        let pr = store.run_flywheel(&pipeline).unwrap();
        assert_eq!(pr.traces_processed, 0);
        assert_eq!(pr.suggestions_count, 0);
    }
}
