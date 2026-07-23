//! 任务分解引擎 —— 三级分解 + LLM 驱动 + 手动构造
//!
//! 【领域含义】DecompositionEngine 是 Plan 引擎的"大脑"，负责将用户的高层
//! 需求分解为结构化的三级任务计划。它通过调用 LLM 模型，利用模型的语义理解
//! 能力完成业务识别、功能分解和文件映射。
//!
//! # 三级分解流程
//!
//! ```text
//! Level 1 (业务模块分解): "开发用户认证模块"
//!   → Auth Module, Todo Module, Data Module
//!
//! Level 2 (技术功能分解): Auth Module
//!   → register, login, JWT middleware, token refresh
//!
//! Level 3 (代码文件映射):
//!   → src/auth/handler.rs, src/auth/middleware.rs
//! ```
//!
//! # 组件
//!
//! - `DecompositionEngine` — 使用 LLM 进行自动分解
//! - `ManualDecomposer` — 用于测试和精确控制的手动构造器

use std::sync::Arc;

use async_trait::async_trait;
use serde_json;
use tracing::{debug, info};

use super::knowledge::KnowledgeProvider;
use super::types::{PlanNode, TaskPlan};

// ---------------------------------------------------------------------------
// DecompositionError
// ---------------------------------------------------------------------------

/// 分解过程中的错误。
#[derive(Debug, thiserror::Error)]
pub enum DecompositionError {
    /// LLM 调用失败
    #[error("Model error: {0}")]
    Model(String),

    /// 解析 LLM 输出失败
    #[error("Parse error: {0}")]
    Parse(String),

    /// 验证失败（如循环依赖）
    #[error("Validation error: {0}")]
    Validation(String),

    /// 不支持的输入
    #[error("Unsupported: {0}")]
    Unsupported(String),
}

// ---------------------------------------------------------------------------
// Decomposer trait
// ---------------------------------------------------------------------------

/// 任务分解器 trait —— 支持多种分解策略。
///
/// 提供统一的 decompose 接口，允许：
/// - `DecompositionEngine`（LLM 驱动）
/// - `ManualDecomposer`（手动构造）
/// - 未来可能的其他策略（如规则引擎、模板匹配）
#[async_trait]
pub trait Decomposer: Send + Sync {
    /// 将用户请求分解为三级任务计划。
    ///
    /// # Arguments
    /// * `request` — 用户的高层需求描述
    /// * `context` — 项目上下文（技术栈、目录结构等）
    ///
    /// # Returns
    /// 验证通过的任务计划（确保无循环依赖、引用完整）。
    async fn decompose(
        &self,
        request: &str,
        context: &str,
    ) -> Result<TaskPlan, DecompositionError>;
}

// ---------------------------------------------------------------------------
// ManualDecomposer
// ---------------------------------------------------------------------------

/// 手动分解器 —— 用于测试和精确控制。
///
/// 接受预定义的结构化节点列表和阶段顺序，
/// 跳过 LLM 调用直接构造 TaskPlan。
///
/// # 使用示例
///
/// ```rust,ignore
/// let decomposer = ManualDecomposer::new("用户认证模块", vec![
///     "Phase 0: 数据层",
///     "Phase 1: 认证层",
/// ]);
/// decomposer.add_task(PlanNode::new("db", "创建用户表", "Phase 0", vec![]));
/// decomposer.add_task(PlanNode::new("auth", "注册接口", "Phase 1", vec!["db"]));
/// let plan = decomposer.decompose("", "").await?;
/// ```
pub struct ManualDecomposer {
    /// 计划名称
    name: String,
    /// 节点列表
    nodes: Vec<PlanNode>,
    /// 阶段顺序
    phases: Vec<String>,
}

impl ManualDecomposer {
    /// 创建手动分解器。
    pub fn new(name: impl Into<String>, phases: Vec<impl Into<String>>) -> Self {
        Self {
            name: name.into(),
            nodes: Vec::new(),
            phases: phases.into_iter().map(|p| p.into()).collect(),
        }
    }

    /// 添加一个任务节点。
    pub fn add_task(&mut self, node: PlanNode) -> &mut Self {
        let phase = node.phase.clone();
        if !self.phases.contains(&phase) {
            self.phases.push(phase);
        }
        self.nodes.push(node);
        self
    }

    /// 批量添加任务节点。
    pub fn add_tasks(&mut self, nodes: Vec<PlanNode>) -> &mut Self {
        for node in nodes {
            self.add_task(node);
        }
        self
    }

    /// 构建 TaskPlan。
    pub fn build(&self) -> Result<TaskPlan, DecompositionError> {
        let mut plan = TaskPlan::new(&self.name);
        for node in self.nodes.clone() {
            plan.add_node(node);
        }
        for phase in &self.phases {
            plan.add_phase(phase);
        }
        plan.validate().map_err(|e| DecompositionError::Validation(e))?;
        Ok(plan)
    }
}

#[async_trait]
impl Decomposer for ManualDecomposer {
    async fn decompose(
        &self,
        _request: &str,
        _context: &str,
    ) -> Result<TaskPlan, DecompositionError> {
        self.build()
    }
}

// ---------------------------------------------------------------------------
// DecompositionEngine — LLM 驱动的自动分解
// ---------------------------------------------------------------------------

/// LLM 驱动的任务分解引擎。
///
/// 【领域含义】通过调用 LLM 模型，利用其语义理解能力将用户需求
/// 分解为结构化的三级任务计划。输出格式为 JSON，确保可被程序化解析。
///
/// # 工作流程
///
/// 1. 构建 prompt（包含请求、上下文、输出格式要求）
/// 2. 调用 LLM 获取 JSON 响应
/// 3. 解析 JSON 为 TaskPlan
/// 4. 验证计划的完整性（无循环依赖、阶段顺序正确）
/// 5. 返回验证通过的计划
pub struct DecompositionEngine {
    /// 分解用的模型客户端（通常使用推理模型）
    model: Box<dyn crate::model::ModelClient>,
    /// 知识提供者（Phase E：注入规则/技能到分解 prompt）
    knowledge: Option<Arc<dyn KnowledgeProvider>>,
}

impl DecompositionEngine {
    /// 创建 LLM 驱动的分解引擎。
    pub fn new(model: Box<dyn crate::model::ModelClient>) -> Self {
        Self {
            model,
            knowledge: None,
        }
    }

    /// 注入知识提供者（Phase E）。
    pub fn with_knowledge(mut self, provider: Arc<dyn KnowledgeProvider>) -> Self {
        self.knowledge = Some(provider);
        self
    }

    /// 构建分解 prompt。
    fn build_prompt(&self, request: &str, context: &str) -> String {
        // Phase E: inject knowledge context if available
        let knowledge_ctx = match &self.knowledge {
            Some(provider) => {
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    let ctx = handle.block_on(provider.format_context(request));
                    ctx
                } else {
                    String::new()
                }
            }
            None => String::new(),
        };

        let base = format!(
            r#"You are a software architecture decomposition engine.
Your task is to break down a user's development request into a structured three-level plan.

## User Request
{request}

## Project Context
{context}

## Output Format
Return a JSON object with this exact structure:
```json
{{
  "name": "execution plan name",
  "phases": ["Phase 0: phase name", "Phase 1: phase name", ...],
  "nodes": [
    {{
      "id": "unique-task-id",
      "label": "human readable task description",
      "phase": "Phase N: name (must match one of the phases above)",
      "depends_on": ["dependency-id-1", "dependency-id-2"],
      "spec": "detailed spec for the coder implementing this task"
    }}
  ]
}}
```

## Decomposition Rules
1. Level 1: Identify business modules (3-6 modules)
2. Level 2: Break each module into technical functions (2-4 per module)
3. Level 3: Map functions to file-level tasks
4. Use dependency IDs (depends_on) to express ordering constraints
5. Independent tasks across different modules should be parallelizable
6. Each phase represents a sequential stage; tasks within a phase can be parallel
7. Total tasks should be proportional to complexity (8-20 for typical requests)
8. The "spec" field should contain enough detail for a Coder to implement the task independently
"#,
            request = request,
            context = context,
        );

        if knowledge_ctx.is_empty() {
            base
        } else {
            format!("{}\n{}", base, knowledge_ctx)
        }
    }

    /// 从 LLM 响应中解析 JSON 为 TaskPlan。
    fn parse_response(&self, response: &str) -> Result<TaskPlan, DecompositionError> {
        // Try to extract JSON from code block
        let json_str = if let Some(start) = response.find("```json") {
            let after_start = &response[start + 7..];
            if let Some(end) = after_start.find("```") {
                after_start[..end].trim()
            } else {
                after_start.trim()
            }
        } else if let Some(start) = response.find('{') {
            let after_start = &response[start..];
            if let Some(end) = after_start.rfind('}') {
                &after_start[..=end]
            } else {
                return Err(DecompositionError::Parse(
                    "No JSON object found in response".into(),
                ));
            }
        } else {
            return Err(DecompositionError::Parse(
                "No JSON object found in response".into(),
            ));
        };

        // Parse JSON into a loosely-typed value first
        let parsed: serde_json::Value = serde_json::from_str(json_str)
            .map_err(|e| DecompositionError::Parse(format!("Invalid JSON: {e}")))?;

        // Extract fields
        let name = parsed
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("Decomposed Plan")
            .to_string();

        let phases: Vec<String> = parsed
            .get("phases")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let nodes_raw = parsed
            .get("nodes")
            .and_then(|v| v.as_array())
            .ok_or_else(|| DecompositionError::Parse("Missing 'nodes' array".into()))?;

        let mut plan = TaskPlan::new(name);
        for phase in &phases {
            plan.add_phase(phase);
        }

        for node_val in nodes_raw {
            let id = node_val
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| DecompositionError::Parse("Node missing 'id'".into()))?
                .to_string();

            let label = node_val
                .get("label")
                .and_then(|v| v.as_str())
                .unwrap_or(&id)
                .to_string();

            let phase = node_val
                .get("phase")
                .and_then(|v| v.as_str())
                .unwrap_or("Phase 0")
                .to_string();

            let depends_on: Vec<String> = node_val
                .get("depends_on")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let spec = node_val.get("spec").and_then(|v| v.as_str()).map(String::from);

            let mut node = PlanNode::new(id, label, phase, depends_on);
            if let Some(s) = spec {
                node = node.with_spec(s);
            }
            plan.add_node(node);
        }

        // Validate the plan
        plan.validate()
            .map_err(|e| DecompositionError::Validation(e))?;

        Ok(plan)
    }
}

#[async_trait]
impl Decomposer for DecompositionEngine {
    async fn decompose(
        &self,
        request: &str,
        context: &str,
    ) -> Result<TaskPlan, DecompositionError> {
        info!(
            request_len = request.len(),
            context_len = context.len(),
            "DecompositionEngine: decomposing request"
        );

        let prompt = self.build_prompt(request, context);

        // Call LLM — use complete() for non-streaming response
        let messages = vec![code_agent_protocol::Message::UserMessage { content: prompt }];

        let response = self
            .model
            .complete(&messages, &[])
            .await
            .map_err(|e| DecompositionError::Model(e.to_string()))?;

        debug!(
            response_len = response.len(),
            "DecompositionEngine: received LLM response"
        );

        let plan = self.parse_response(&response)?;

        info!(
            phases = plan.phases.len(),
            nodes = plan.nodes.len(),
            "DecompositionEngine: successfully decomposed"
        );

        Ok(plan)
    }
}

// ---------------------------------------------------------------------------
// Convenience builder
// ---------------------------------------------------------------------------

/// 构建一个简单的三阶段 Web 项目测试计划。
pub fn make_web_project_plan() -> TaskPlan {
    let mut plan = TaskPlan::new("Web Module Development");

    plan.add_phase("Phase 0: 基础层");
    plan.add_phase("Phase 1: 业务层");
    plan.add_phase("Phase 2: 集成层");

    // Phase 0 — 无依赖，可并行
    plan.add_node(
        PlanNode::new("db-schema", "创建数据库 Schema", "Phase 0: 基础层", vec![])
            .with_spec("设计 users 表和 todos 表的 DDL，包含必要的索引和约束"),
    );
    plan.add_node(
        PlanNode::new("config", "配置管理模块", "Phase 0: 基础层", vec![])
            .with_spec("实现环境变量加载、配置文件解析、日志初始化"),
    );

    // Phase 1 — 依赖 Phase 0
    plan.add_node(
        PlanNode::new("user-model", "用户数据模型", "Phase 1: 业务层", vec!["db-schema".into()])
            .with_spec("User struct + ORM 映射 + 数据库迁移脚本"),
    );
    plan.add_node(
        PlanNode::new("auth-middleware", "JWT 认证中间件", "Phase 1: 业务层", vec!["user-model".into()])
            .with_spec("JWT 生成/验证中间件，支持 Bearer Token 认证"),
    );
    plan.add_node(
        PlanNode::new("user-api", "用户 CRUD API", "Phase 1: 业务层", vec!["user-model".into()])
            .with_spec("注册、登录、获取用户信息、更新用户 API 端点"),
    );

    // Phase 2 — 依赖 Phase 1
    plan.add_node(
        PlanNode::new("integration", "集成测试", "Phase 2: 集成层", vec!["user-api".into(), "auth-middleware".into()])
            .with_spec("端到端测试：用户注册→登录→Token 认证→CRUD 操作"),
    );
    plan.add_node(
        PlanNode::new("api-docs", "API 文档生成", "Phase 2: 集成层", vec!["user-api".into()])
            .with_spec("生成 OpenAPI/Swagger 文档"),
    );

    plan
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── ManualDecomposer tests ─────────────────────────────────

    #[test]
    fn test_manual_decomposer_build() {
        let mut d = ManualDecomposer::new("test", vec!["Phase 0", "Phase 1"]);
        d.add_task(PlanNode::new("a", "Task A", "Phase 0", vec![]));
        d.add_task(PlanNode::new("b", "Task B", "Phase 1", vec!["a".into()]));

        let plan = d.build().unwrap();
        assert_eq!(plan.node_count(), 2);
        assert_eq!(plan.phases.len(), 2);
    }

    #[test]
    fn test_manual_decomposer_with_cycle() {
        let mut d = ManualDecomposer::new("test", vec!["Phase 0"]);
        d.add_task(PlanNode::new("a", "A", "Phase 0", vec!["b".into()]));
        d.add_task(PlanNode::new("b", "B", "Phase 0", vec!["a".into()]));

        let result = d.build();
        assert!(result.is_err());
    }

    // ── LLM response parsing tests ────────────────────────────

    struct NullModel;
    #[async_trait]
    impl crate::model::ModelClient for NullModel {
        fn model_name(&self) -> &str { "null-model" }
        async fn complete_stream(
            &self, _: &[code_agent_protocol::Message], _: &[crate::model::types::ToolDefinition],
        ) -> crate::model::ModelResult<Box<dyn futures::Stream<Item = code_agent_protocol::ResponseEvent> + Send + Unpin>> {
            use futures::stream;
            Ok(Box::new(stream::empty()))
        }
        fn last_token_usage(&self) -> Option<crate::model::types::TokenUsage> { None }
    }

    #[test]
    fn test_parse_simple_response() {
        let engine = DecompositionEngine {
            model: Box::new(NullModel),
            knowledge: None,
        };

        let response = r#"
Here is the decomposition:

```json
{
  "name": "Auth Module",
  "phases": ["Phase 0: Data", "Phase 1: API"],
  "nodes": [
    {
      "id": "db-user",
      "label": "Create user table",
      "phase": "Phase 0: Data",
      "depends_on": [],
      "spec": "CREATE TABLE users..."
    },
    {
      "id": "auth-register",
      "label": "Register endpoint",
      "phase": "Phase 1: API",
      "depends_on": ["db-user"],
      "spec": "POST /api/auth/register"
    }
  ]
}
```
"#;

        let plan = engine.parse_response(response).unwrap();
        assert_eq!(plan.name, "Auth Module");
        assert_eq!(plan.phases.len(), 2);
        assert_eq!(plan.nodes.len(), 2);
        assert_eq!(plan.nodes[0].id, "db-user");
        assert_eq!(plan.nodes[1].depends_on, vec!["db-user"]);
    }

    #[test]
    fn test_parse_response_without_code_block() {
        let engine = DecompositionEngine {
            model: Box::new(NullModel),
            knowledge: None,
        };

        let response = r#"{"name": "Simple Plan", "phases": ["Phase 0"], "nodes": [{"id": "t1", "label": "Task 1", "phase": "Phase 0", "depends_on": []}]}"#;

        let plan = engine.parse_response(response).unwrap();
        assert_eq!(plan.name, "Simple Plan");
        assert_eq!(plan.nodes.len(), 1);
    }

    #[test]
    fn test_parse_response_missing_nodes() {
        let engine = DecompositionEngine {
            model: Box::new(NullModel),
            knowledge: None,
        };

        let response = r#"{"name": "Bad Plan", "phases": []}"#;
        let result = engine.parse_response(response);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("nodes"));
    }

    #[test]
    fn test_parse_invalid_json() {
        let engine = DecompositionEngine {
            model: Box::new(NullModel),
            knowledge: None,
        };

        let response = "not json at all";
        let result = engine.parse_response(response);
        assert!(result.is_err());
    }

    // ── Web project plan tests ────────────────────────────────

    #[test]
    fn test_web_project_plan_valid() {
        let plan = make_web_project_plan();
        assert!(plan.validate().is_ok());
        assert!(plan.detect_cycle().is_none());
        assert_eq!(plan.node_count(), 7);
    }

    #[test]
    fn test_web_project_plan_phase_count() {
        let plan = make_web_project_plan();
        assert_eq!(plan.phases.len(), 3);
    }

    #[test]
    fn test_web_project_plan_dependencies_valid() {
        let plan = make_web_project_plan();
        // Phase 0 tasks should have no deps
        let phase0 = plan.nodes_by_phase("Phase 0: 基础层");
        for node in phase0 {
            assert!(node.depends_on.is_empty(), "Phase 0 node '{}' has deps", node.id);
        }

        // Phase 2 should depend on Phase 1
        let phase2 = plan.nodes_by_phase("Phase 2: 集成层");
        for node in phase2 {
            assert!(!node.depends_on.is_empty(), "Phase 2 node '{}' has no deps", node.id);
        }
    }

    // ── Decomposer trait tests ─────────────────────────────────

    #[tokio::test]
    async fn test_manual_decomposer_via_trait() {
        let mut d = ManualDecomposer::new("test", vec!["Phase 0"]);
        d.add_task(PlanNode::new("t1", "Task 1", "Phase 0", vec![]));

        let plan = d.decompose("request", "context").await.unwrap();
        assert_eq!(plan.node_count(), 1);
        assert_eq!(plan.nodes[0].id, "t1");
    }
}
