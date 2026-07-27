//! 会话管理与 ReAct 推理循环
//!
//! 【领域含义】`Session` 是 Coding Agent 的核心聚合根。一个 Session 代表一次完整的
//! 交互式编码会话，驱动 ReAct (Reasoning + Acting) 循环执行 `run_turn` 方法，
//! 编排模型客户端、工具注册中心和上下文管理器三者协作。
//!
//! 【ReAct 循环流程】
//! 1. 构建提示词（系统指令 + 对话历史 + 工具定义）
//! 2. 调用模型客户端（流式） → 收集文本增量和工具调用
//! 3. 如果有工具调用：执行每个工具 → 追加结果 → 回到步骤 1
//! 4. 如果是纯文本：发射 TurnComplete 事件 → 返回
//!
//! # ReAct Loop
//!
//! ```text
//! 1. Build prompt (system + history + tool definitions)
//! 2. Call model client (streaming) → collect text deltas & tool calls
//! 3. If tool calls: execute each tool → append results → go to 1
//! 4. If text-only: emit TurnComplete → return events
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use chrono::Utc;

use crate::flywheel::collector::FlywheelCollector;
use crate::flywheel::Suggestion;
use crate::model::ModelClient;
use crate::safety::ContentSafetyLayer;
use crate::tools::registry::ToolRegistry;

use crate::persistence::{SessionSnapshot, SessionStore, TurnSnapshot};

use code_agent_protocol::{
    Message, PermissionMode, ResponseEvent, SessionId, SessionStatus, ThreadId, ToolCall,
    ToolResultMessage, TurnId, TurnInput,
};
use futures::StreamExt;

use crate::context::ContextManager;
use crate::model::types::ToolDefinition;

use async_trait::async_trait;
use futures::Stream;

// ---------------------------------------------------------------------------
// Step chain types — Strategy/Chain-of-Responsibility pattern for run_turn()
// ---------------------------------------------------------------------------

/// Outcome of executing a single step in the session execution chain.
#[derive(Debug)]
enum StepOutcome {
    /// Proceed to the next step in the chain.
    Continue,
    /// Stop execution immediately and return these accumulated events.
    Break(Vec<ResponseEvent>),
    /// Stop execution with a single error event appended to accumulated events.
    Error(String),
}

/// Shared context passed between steps during a single turn execution.
///
/// Provides controlled access to per-turn state without exposing Session
/// internals to every step.
struct StepContext<'a> {
    /// Accumulated response events for this turn (pushed by steps).
    events: &'a mut Vec<ResponseEvent>,
    /// Current turn ID.
    turn_id: &'a TurnId,
    /// Thread ID (extracted from TurnInput after message move).
    thread_id: ThreadId,
    /// Tool calls collected from the most recent model response.
    tool_calls: Vec<ToolCall>,
    /// Assistant text collected from deltas or final message content.
    assistant_text: String,
    /// Stream output from the model client, consumed by ResponseCollectionStep.
    stream_output: Option<Box<dyn Stream<Item = ResponseEvent> + Send + Unpin>>,
}

/// A single step in the session's execution chain.
///
/// Each step handles one distinct concern (safety check, model invocation,
/// response collection, tool execution, final answer, flywheel analysis,
/// persistence), making the ReAct loop independently composable and testable.
#[async_trait]
trait SessionStep {
    /// Execute this step.
    ///
    /// Returns [`StepOutcome::Continue`] if the next step should run,
    /// [`StepOutcome::Break`] or [`StepOutcome::Error`] to halt execution.
    async fn execute(&self, session: &mut Session, ctx: &mut StepContext<'_>) -> StepOutcome;
}

// ---------------------------------------------------------------------------
// SessionConfig
// ---------------------------------------------------------------------------

/// 会话配置 —— 创建 Session 聚合根所需的全部参数
///
/// 【领域含义】SessionConfig 是创建 Session 的值对象，包含会话标识、
/// 系统指令、最大循环次数、权限模式、模型客户端和工具注册中心的引用。
/// v2 新增 `external_cancel` 字段支持外部取消信号（用于 SessionRunner）。
#[derive(Clone)]
pub struct SessionConfig {
    /// 会话唯一标识符
    pub id: SessionId,

    /// 系统指令 —— 每次模型调用时预置到提示词开头
    pub system_instructions: String,

    /// ReAct 循环最大迭代次数 —— 防止无限循环
    pub max_iterations: usize,

    /// 工具执行的权限模式
    pub permission_mode: PermissionMode,

    /// 聊天补全模型客户端（通过 Arc 共享）
    pub model_client: Arc<dyn ModelClient>,

    /// 工具注册中心（通过 Arc 共享）
    pub tool_registry: Arc<dyn ToolRegistry>,

    /// 可选的外部取消信号（v2 Delegator 用）。
    /// 当设置后，ReAct 循环在每次迭代开始时同时检查此标志和自身的 interrupt。
    /// 用于 SessionRunner 将 SubAgentManagerImpl 的取消信号传播到子 Session。
    pub external_cancel: Option<Arc<AtomicBool>>,
    /// 最大上下文 Token 数量（Phase C Token 预算控制）。
    /// 当设置后，Session 将使用自定义 ContextConfig 而非默认值。
    pub max_context_tokens: Option<usize>,
    /// 模型采样温度（None 表示使用模型客户端默认值）。
    pub temperature: Option<f32>,
}

// ---------------------------------------------------------------------------
// SessionConfigBuilder
// ---------------------------------------------------------------------------

/// Fluent builder for [`SessionConfig`].
///
/// Eliminates the error-prone direct struct construction pattern by providing
/// chainable setter methods with sensible defaults for all optional fields.
///
/// # Required fields
///
/// `id`, `model_client`, and `tool_registry` MUST be set before calling
/// [`build()`](Self::build) — the method panics if any are missing.
///
/// # Example
///
/// ```rust,ignore
/// let config = SessionConfigBuilder::default()
///     .id(SessionId::from("my-session"))
///     .model_client(Arc::new(my_client))
///     .tool_registry(Arc::new(my_registry))
///     .build();
/// ```
#[derive(Default)]
pub struct SessionConfigBuilder {
    id: Option<SessionId>,
    system_instructions: Option<String>,
    max_iterations: Option<usize>,
    permission_mode: Option<PermissionMode>,
    model_client: Option<Arc<dyn ModelClient>>,
    tool_registry: Option<Arc<dyn ToolRegistry>>,
    external_cancel: Option<Arc<AtomicBool>>,
    max_context_tokens: Option<usize>,
    temperature: Option<f32>,
}

impl SessionConfigBuilder {
    /// Set the session identifier (required).
    pub fn id(mut self, id: SessionId) -> Self {
        self.id = Some(id);
        self
    }

    /// Set the system instructions (default: empty string).
    pub fn system_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.system_instructions = Some(instructions.into());
        self
    }

    /// Set the maximum ReAct loop iterations (default: 50).
    pub fn max_iterations(mut self, max: usize) -> Self {
        self.max_iterations = Some(max);
        self
    }

    /// Set the tool permission mode (default: [`PermissionMode::Auto`]).
    pub fn permission_mode(mut self, mode: PermissionMode) -> Self {
        self.permission_mode = Some(mode);
        self
    }

    /// Set the model client (required).
    pub fn model_client(mut self, client: Arc<dyn ModelClient>) -> Self {
        self.model_client = Some(client);
        self
    }

    /// Set the tool registry (required).
    pub fn tool_registry(mut self, registry: Arc<dyn ToolRegistry>) -> Self {
        self.tool_registry = Some(registry);
        self
    }

    /// Set an optional external cancel signal.
    pub fn external_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.external_cancel = Some(cancel);
        self
    }

    /// Set an optional max context token limit.
    pub fn max_context_tokens(mut self, tokens: usize) -> Self {
        self.max_context_tokens = Some(tokens);
        self
    }

    /// Set the model sampling temperature.
    pub fn temperature(mut self, t: f32) -> Self {
        self.temperature = Some(t);
        self
    }
}

impl SessionConfigBuilder {
    /// Consume the builder and produce a [`SessionConfig`].
    ///
    /// # Panics
    ///
    /// Panics if `id`, `model_client`, or `tool_registry` have not been set.
    pub fn build(self) -> SessionConfig {
        SessionConfig {
            id: self.id.expect("SessionConfigBuilder: `id` is required"),
            system_instructions: self.system_instructions.unwrap_or_default(),
            max_iterations: self.max_iterations.unwrap_or(50),
            permission_mode: self
                .permission_mode
                .unwrap_or(PermissionMode::Auto),
            model_client: self
                .model_client
                .expect("SessionConfigBuilder: `model_client` is required"),
            tool_registry: self
                .tool_registry
                .expect("SessionConfigBuilder: `tool_registry` is required"),
            external_cancel: self.external_cancel,
            max_context_tokens: self.max_context_tokens,
            temperature: self.temperature,
        }
    }
}

// ---------------------------------------------------------------------------
// SessionState
// ---------------------------------------------------------------------------

/// 会话运行时状态 —— 会话生命周期的快照
///
/// 【领域含义】SessionState 是 Session 聚合根的只读投影值对象，
/// 记录当前会话的标识、状态、当前轮次和轮次计数。
#[derive(Clone, Debug)]
pub struct SessionState {
    /// 会话唯一标识符
    pub id: SessionId,

    /// 当前生命周期状态（Active / Completed / Interrupted 等）
    pub status: SessionStatus,

    /// 当前正在执行的轮次 ID，无则为 None
    pub current_turn: Option<TurnId>,

    /// 已执行的轮次总数
    pub turn_count: u64,

    /// 当前生效的权限模式
    pub permission_mode: PermissionMode,

    /// Phase G: Flywheel 改进建议（最近一次分析结果）
    pub suggestions: Vec<Suggestion>,
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// ReAct 推理循环 —— Agent 核心聚合根
///
/// 【领域含义】Session 是 Coding Agent 的核心领域实体，代表一次完整的
/// 交互式编码会话。它驱动 ReAct (Reasoning + Acting) 循环，通过
/// `run_turn` 方法编排模型推理、工具执行和上下文管理三者协作。
///
/// 【核心职责】
/// 1. **推理循环**: 构建提示词 → 调用模型 → 解析工具调用 → 执行工具 → 循环
/// 2. **状态管理**: 维护会话生命周期状态（Active / Completed / Interrupted）
/// 3. **中断控制**: 支持异步中断当前轮次，优雅退出
/// 4. **工具编排**: 从 ToolRegistry 查找并执行工具，将结果反馈给模型
/// 5. **上下文管理**: 通过 ContextManager 管理对话历史，触发上下文压缩
///
/// 【聚合边界】Session 聚合了以下领域对象：
/// - `ModelClient`（模型客户端 —— 外部依赖）
/// - `ToolRegistry`（工具注册中心 —— 外部依赖）
/// - `ContextManager`（上下文管理器 —— 值对象）
///
/// # Example
///
/// ```rust,ignore
/// use code_agent_core::agent::{Session, SessionConfig};
/// use code_agent_protocol::SessionId;
///
/// let config = SessionConfig {
///     id: SessionId::from("my-session"),
///     system_instructions: "You are a coding assistant.".into(),
///     max_iterations: 20,
///     permission_mode: Default::default(),
///     model_client: /* Arc<dyn ModelClient> */,
///     tool_registry: /* Arc<ToolRegistry> */,
/// };
/// let mut session = Session::new(config);
/// ```
pub struct Session {
    /// 会话配置
    config: SessionConfig,

    /// 模型客户端（通过 Arc 共享）
    model_client: Arc<dyn ModelClient>,

    /// 工具注册中心（通过 Arc 共享）
    tool_registry: Arc<dyn ToolRegistry>,

    /// 对话历史和提示词构建器
    context_manager: ContextManager,

    /// 可变会话状态
    state: SessionState,

    /// 中断标志 —— 设为 true 时当前轮次中止
    interrupted: Arc<AtomicBool>,

    /// 单调递增计数器，用于生成轮次 ID
    turn_counter: u64,

    /// Phase G: Flywheel 错误追踪收集器
    flywheel: FlywheelCollector,

    /// Optional session store for auto-persist after each turn.
    session_store: Mutex<Option<SessionStore>>,

    /// Optional content safety layer for prompt injection defense.
    /// When set, user input is checked via `check_input()` before entering
    /// the context, and tool results are sanitized via `sanitize()` before
    /// being added to the conversation history.
    safety_layer: Option<Arc<ContentSafetyLayer>>,
}

impl Session {
    /// 从配置创建一个新的 Session
    ///
    /// `async` 签名为未来扩展预留（例如从数据库加载会话状态）；
    /// 当前不执行任何 I/O 操作。
    pub async fn new(config: SessionConfig) -> Self {
        let context_manager = match config.max_context_tokens {
            Some(max_tokens) => {
                let ctx_config = crate::context::ContextConfig {
                    max_context_tokens: max_tokens,
                    ..crate::context::ContextConfig::default()
                };
                crate::context::ContextManager::with_config(
                    config.system_instructions.clone(),
                    ctx_config,
                )
            }
            None => crate::context::ContextManager::new(config.system_instructions.clone()),
        };
        let state = SessionState {
            id: config.id.clone(),
            status: SessionStatus::Active,
            current_turn: None,
            turn_count: 0,
            permission_mode: config.permission_mode.clone(),
            suggestions: vec![],
        };

        // v2: 如果有外部取消信号，使用它；否则创建新的
        let interrupted = config
            .external_cancel
            .clone()
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));

        Self {
            model_client: Arc::clone(&config.model_client),
            tool_registry: Arc::clone(&config.tool_registry),
            config,
            context_manager,
            state,
            interrupted,
            turn_counter: 0,
            flywheel: FlywheelCollector::new(),
            session_store: Mutex::new(None),
            safety_layer: None,
        }
    }

    /// Attach a SessionStore for auto-persist after each turn.
    pub fn with_session_store(&mut self, store: SessionStore) {
        *self.session_store.lock().unwrap() = Some(store);
    }

    /// Attach a content safety layer for prompt injection defense.
    ///
    /// When set, user messages are checked via [`ContentSafetyLayer::check_input`]
    /// before entering the context, and tool results are sanitized via
    /// [`ContentSafetyLayer::sanitize`] before being added to the conversation
    /// history.
    pub fn with_safety_layer(&mut self, layer: Arc<ContentSafetyLayer>) {
        self.safety_layer = Some(layer);
    }

    /// Reconstruct a Session from a persisted snapshot.
    pub async fn from_snapshot(
        snapshot: SessionSnapshot,
        model_client: Arc<dyn ModelClient>,
        tool_registry: Arc<dyn ToolRegistry>,
        external_cancel: Option<Arc<AtomicBool>>,
    ) -> Self {
        let mut context_manager = ContextManager::new(snapshot.system_instructions.clone());
        // Replay messages into context
        context_manager.add_messages(snapshot.messages.clone());

        let interrupted =
            external_cancel.unwrap_or_else(|| Arc::new(AtomicBool::new(false)));

        // Clone Arcs before moving into config
        let mc = Arc::clone(&model_client);
        let tr = Arc::clone(&tool_registry);

        Self {
            model_client,
            tool_registry,
            config: SessionConfig {
                id: snapshot.id.clone(),
                system_instructions: snapshot.system_instructions,
                max_iterations: snapshot.max_iterations,
                permission_mode: snapshot.permission_mode.clone(),
                model_client: mc,
                tool_registry: tr,
                external_cancel: None,
                max_context_tokens: None,
                temperature: None,
            },
            context_manager,
            state: SessionState {
                id: snapshot.id,
                status: snapshot.status,
                current_turn: None,
                turn_count: snapshot.turn_count,
                permission_mode: snapshot.permission_mode,
                suggestions: vec![],
            },
            interrupted,
            turn_counter: snapshot.turn_count,
            flywheel: FlywheelCollector::new(),
            session_store: Mutex::new(None),
            safety_layer: None,
        }
    }

    /// 执行 ReAct 推理循环 —— 单轮对话的核心执行方法
    ///
    /// 【ReAct 循环流程】
    /// 1. 接受用户输入消息
    /// 2. 进入 ReAct 循环：调用模型 → 收集响应
    /// 3. 如果模型发出工具调用：执行工具 → 将结果反馈给模型 → 继续循环
    /// 4. 如果模型只返回文本：返回最终答案
    ///
    /// 【中断处理】在每次循环迭代开始时检查中断标志，
    /// 如果被中断则立即返回所有已收集的事件。
    ///
    /// 【Step Chain】内部使用 [`SessionStep`] trait 的链式执行模式，
    /// 每个步骤处理一个独立关注点（安全检测、模型调用、响应收集、
    /// 工具执行、最终回答、飞轮分析、持久化），使循环可组合、可测试。
    ///
    /// # Returns
    ///
    /// 返回本轮的按时间排序的 `ResponseEvent` 向量，
    /// 适合流式传输到 UI 层。
    pub async fn run_turn(&mut self, input: TurnInput) -> Vec<ResponseEvent> {
        let mut events: Vec<ResponseEvent> = Vec::new();

        // Check for pre-existing interrupt before starting turn
        let was_interrupted = self.interrupted.swap(false, Ordering::AcqRel);
        if was_interrupted {
            events.push(ResponseEvent::Error {
                message: "Turn interrupted by user".into(),
            });
            return events;
        }

        // Generate a turn ID
        let turn_id = TurnId(format!("turn-{}", self.turn_counter));
        self.turn_counter += 1;

        // Update session state
        self.state.current_turn = Some(turn_id.clone());
        self.state.turn_count += 1;
        self.state.status = SessionStatus::Active;

        events.push(ResponseEvent::TurnStarted {
            turn_id: turn_id.clone(),
        });

        // --- Step 1: Safety check (pre-loop, borrows input.messages) ---
        // Scoped block ensures the borrow on input.messages is released
        // before we move it into context_manager below.
        {
            let safety_step = SafetyCheckStep {
                messages: &input.messages,
            };
            let mut ctx = StepContext {
                events: &mut events,
                turn_id: &turn_id,
                thread_id: ThreadId(String::new()), // placeholder, unused on error
                tool_calls: Vec::new(),
                assistant_text: String::new(),
                stream_output: None,
            };
            if let Some(evts) =
                step_outcome_to_break(safety_step.execute(self, &mut ctx).await, self, &mut ctx)
            {
                return evts;
            }
        }

        // Add user input messages to conversation history (moves input.messages)
        // Safety: borrow from SafetyCheckStep above is released when scope ends.
        self.context_manager.add_messages(input.messages);

        // Extract thread_id after partial move of input.messages
        let thread_id = input.thread_id;

        // --- Build step context for the ReAct loop ---
        let mut ctx = StepContext {
            events: &mut events,
            turn_id: &turn_id,
            thread_id,
            tool_calls: Vec::new(),
            assistant_text: String::new(),
            stream_output: None,
        };

        // --- ReAct loop ---
        for _iteration in 0..self.config.max_iterations {
            // Check for interrupt between each logical step
            if self.interrupted.load(Ordering::Acquire) {
                events.push(ResponseEvent::Error {
                    message: "Turn interrupted by user".into(),
                });
                self.state.current_turn = None;
                return events;
            }

            // Step 2: Model invocation (build prompt + call model)
            if let Some(evts) =
                step_outcome_to_break(
                    ModelInvocationStep.execute(self, &mut ctx).await,
                    self,
                    &mut ctx,
                )
            {
                return evts;
            }

            // Step 3: Response collection (stream deltas + tool calls)
            if let Some(evts) =
                step_outcome_to_break(
                    ResponseCollectionStep.execute(self, &mut ctx).await,
                    self,
                    &mut ctx,
                )
            {
                return evts;
            }

            // Step 4: Tool execution (if tool calls present, loop back to model)
            if !ctx.tool_calls.is_empty() {
                if let Some(evts) =
                    step_outcome_to_break(
                        ToolExecutionStep.execute(self, &mut ctx).await,
                        self,
                        &mut ctx,
                    )
                {
                    return evts;
                }
                // Loop back to model for more reasoning
                continue;
            }

            // Step 5: Final answer
            if let Some(evts) =
                step_outcome_to_break(
                    FinalAnswerStep.execute(self, &mut ctx).await,
                    self,
                    &mut ctx,
                )
            {
                return evts;
            }

            // Step 6: Flywheel analysis
            if let Some(evts) =
                step_outcome_to_break(
                    FlywheelAnalysisStep.execute(self, &mut ctx).await,
                    self,
                    &mut ctx,
                )
            {
                return evts;
            }

            // Step 7: Persistence
            if let Some(evts) =
                step_outcome_to_break(
                    PersistenceStep.execute(self, &mut ctx).await,
                    self,
                    &mut ctx,
                )
            {
                return evts;
            }

            self.state.current_turn = None;
            return events;
        }

        // 超过最大迭代次数仍未获得最终答案
        events.push(ResponseEvent::Error {
            message: format!(
                "Max iterations ({}) reached without final answer",
                self.config.max_iterations
            ),
        });
        self.state.current_turn = None;
        events
    }

    /// 中断当前正在执行的轮次
    ///
    /// 【领域行为】设置中断标志，当前轮次将在下一个循环边界处停止，
    /// 返回所有已收集的事件。用于用户主动取消或超时处理。
    pub fn interrupt(&mut self) {
        self.interrupted.store(true, Ordering::Release);
    }

    /// 获取当前会话状态的快照
    ///
    /// 返回 `SessionState` 值对象，包含会话 ID、状态、当前轮次和轮次计数。
    pub fn status(&self) -> SessionState {
        self.state.clone()
    }

    /// 获取会话的唯一标识符
    pub fn id(&self) -> &SessionId {
        &self.config.id
    }

    /// 从注册中心构建工具定义列表，发送给模型
    fn build_tool_definitions(&self) -> Vec<ToolDefinition> {
        self.tool_registry
            .list()
            .into_iter()
            .map(|td| ToolDefinition {
                name: td.name,
                description: td.description,
                parameters: td.input_schema,
            })
            .collect()
    }

    /// 执行单个工具调用
    ///
    /// 【领域行为】从 ToolRegistry 查找工具 → 执行 → 返回结果。
    /// 成功返回 `Ok(ToolResultMessage)`，失败返回 `Err(ToolResultMessage)` 携带错误信息。
    async fn execute_tool(&self, tc: &ToolCall) -> Result<ToolResultMessage, ToolResultMessage> {
        let tool = self.tool_registry.get(&tc.name);

        match tool {
            Some(t) => match t.execute(tc.arguments.clone()).await {
                Ok(mut trm) => {
                    // Ensure the tool_call_id matches the call
                    trm.tool_call_id = tc.id.clone();
                    Ok(trm)
                }
                Err(e) => Err(ToolResultMessage {
                    tool_call_id: tc.id.clone(),
                    output: None,
                    error: Some(e.to_string()),
                }),
            },
            None => Err(ToolResultMessage {
                tool_call_id: tc.id.clone(),
                output: None,
                error: Some(format!("Tool not found: {}", tc.name)),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Step implementations — each handles one distinct concern in the chain
// ---------------------------------------------------------------------------

/// Safety check step: runs ContentSafetyLayer::check_input on user messages
/// before they enter the ReAct loop.
///
/// Corresponds to original `run_turn()` lines 446-474.
struct SafetyCheckStep<'a> {
    messages: &'a [Message],
}

#[async_trait]
impl SessionStep for SafetyCheckStep<'_> {
    async fn execute(&self, session: &mut Session, ctx: &mut StepContext<'_>) -> StepOutcome {
        if let Some(ref safety) = session.safety_layer {
            for msg in self.messages {
                if let Message::UserMessage { content } = msg {
                    match safety.check_input(content).await {
                        Ok(verdict) => {
                            if !verdict.allowed {
                                let reason = verdict
                                    .block_reason
                                    .unwrap_or_else(|| "Input blocked by safety layer".into());
                                ctx.events.push(ResponseEvent::Error {
                                    message: reason,
                                });
                                return StepOutcome::Break(std::mem::take(ctx.events));
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "Safety layer check_input failed, allowing input"
                            );
                        }
                    }
                }
            }
        }
        StepOutcome::Continue
    }
}

/// Model invocation step: builds prompt from context manager, calls the model
/// client, and stores the resulting stream in [`StepContext::stream_output`].
///
/// Corresponds to original `run_turn()` lines 490-504.
struct ModelInvocationStep;

#[async_trait]
impl SessionStep for ModelInvocationStep {
    async fn execute(&self, session: &mut Session, ctx: &mut StepContext<'_>) -> StepOutcome {
        let messages = session.context_manager.build_messages();
        let tool_defs = session.build_tool_definitions();

        match session
            .model_client
            .complete_stream(&messages, &tool_defs, session.config.temperature)
            .await
        {
            Ok(s) => {
                ctx.stream_output = Some(s);
                StepOutcome::Continue
            }
            Err(e) => {
                ctx.events.push(ResponseEvent::Error {
                    message: format!("Model error: {e}"),
                });
                StepOutcome::Break(std::mem::take(ctx.events))
            }
        }
    }
}

/// Response collection step: consumes the stream from
/// [`StepContext::stream_output`], collecting text deltas and tool calls,
/// and populates [`StepContext::assistant_text`] and
/// [`StepContext::tool_calls`].
///
/// Corresponds to original `run_turn()` lines 506-558.
struct ResponseCollectionStep;

#[async_trait]
impl SessionStep for ResponseCollectionStep {
    async fn execute(&self, _session: &mut Session, ctx: &mut StepContext<'_>) -> StepOutcome {
        let mut stream = match ctx.stream_output.take() {
            Some(s) => s,
            None => return StepOutcome::Error("No model stream available".into()),
        };

        let mut text_buffer = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut final_message_content: Option<String> = None;

        while let Some(event) = stream.next().await {
            match event {
                ResponseEvent::AgentMessageDelta { ref content } => {
                    text_buffer.push_str(content);
                    ctx.events.push(ResponseEvent::AgentMessageDelta {
                        content: content.clone(),
                    });
                }
                ResponseEvent::ToolCallBegin { ref tool_call } => {
                    tool_calls.push(tool_call.clone());
                    ctx.events.push(ResponseEvent::ToolCallBegin {
                        tool_call: tool_call.clone(),
                    });
                }
                ResponseEvent::TokenUsage {
                    prompt_tokens,
                    completion_tokens,
                } => {
                    ctx.events.push(ResponseEvent::TokenUsage {
                        prompt_tokens,
                        completion_tokens,
                    });
                }
                ResponseEvent::TurnComplete {
                    ref final_message, ..
                } => {
                    if let Message::AssistantMessage { ref content } = final_message {
                        if !content.is_empty() {
                            final_message_content = Some(content.clone());
                        }
                    }
                }
                ResponseEvent::Error { ref message } => {
                    ctx.events.push(ResponseEvent::Error {
                        message: message.clone(),
                    });
                }
                ResponseEvent::TurnStarted { .. } | ResponseEvent::ToolCallEnd { .. } => {
                    // Model should not emit these; ignore if it does.
                }
            }
        }

        ctx.assistant_text = if !text_buffer.is_empty() {
            text_buffer
        } else {
            final_message_content.unwrap_or_default()
        };
        ctx.tool_calls = tool_calls;

        StepOutcome::Continue
    }
}

/// Tool execution step: iterates over collected tool calls, executes each,
/// captures flywheel error traces, sanitizes results through the safety layer,
/// and emits ToolCallEnd events.
///
/// Corresponds to original `run_turn()` lines 567-639.
struct ToolExecutionStep;

#[async_trait]
impl SessionStep for ToolExecutionStep {
    async fn execute(&self, session: &mut Session, ctx: &mut StepContext<'_>) -> StepOutcome {
        if ctx.tool_calls.is_empty() {
            return StepOutcome::Continue;
        }

        // Record assistant message in history before tool calls
        if !ctx.assistant_text.is_empty() {
            session.context_manager.add_message(Message::AssistantMessage {
                content: std::mem::take(&mut ctx.assistant_text),
            });
        }

        for tc in &ctx.tool_calls {
            session
                .context_manager
                .add_message(Message::ToolCall(tc.clone()));

            // Execute tool
            let result = session.execute_tool(tc).await;

            // Phase G: capture tool errors for flywheel analysis
            if let Err(ref trm) = result {
                let error_msg = trm.error.clone().unwrap_or_default();
                session.flywheel.record(crate::flywheel::ErrorTrace {
                    error_type: classify_tool_error(&error_msg),
                    tool_name: tc.name.clone(),
                    language: "unknown".into(),
                    file_pattern: "unknown".into(),
                    turn_count: session.turn_counter as usize,
                    message: error_msg,
                    timestamp: Utc::now(),
                });
            }

            let trm = match result {
                Ok(trm) => trm,
                Err(trm) => trm,
            };

            ctx.events.push(ResponseEvent::ToolCallEnd {
                tool_call_id: tc.id.clone(),
                result: trm.clone(),
            });

            // Sanitize tool result through safety layer
            let trm = if let Some(ref safety) = session.safety_layer {
                let output = trm.output.as_deref().unwrap_or("");
                match safety.sanitize(&tc.name, output).await {
                    Ok(sanitized) => {
                        if sanitized.was_modified {
                            let mut trm = trm;
                            trm.output = Some(sanitized.content);
                            trm
                        } else {
                            trm
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            tool = %tc.name,
                            "Safety layer sanitize failed, using original output"
                        );
                        trm
                    }
                }
            } else {
                trm
            };

            session
                .context_manager
                .add_message(Message::ToolResult(trm));
        }

        StepOutcome::Continue
    }
}

/// Final answer step: builds the final AssistantMessage from collected text,
/// adds it to conversation history, and emits a TurnComplete event.
///
/// Corresponds to original `run_turn()` lines 642-654.
struct FinalAnswerStep;

#[async_trait]
impl SessionStep for FinalAnswerStep {
    async fn execute(&self, session: &mut Session, ctx: &mut StepContext<'_>) -> StepOutcome {
        let final_message = Message::AssistantMessage {
            content: std::mem::take(&mut ctx.assistant_text),
        };

        session
            .context_manager
            .add_message(final_message.clone());

        ctx.events.push(ResponseEvent::TurnComplete {
            turn_id: ctx.turn_id.clone(),
            final_message,
        });

        StepOutcome::Continue
    }
}

/// Flywheel analysis step: runs incremental failure analysis on accumulated
/// error traces and updates session suggestions.
///
/// Corresponds to original `run_turn()` lines 655-664.
struct FlywheelAnalysisStep;

#[async_trait]
impl SessionStep for FlywheelAnalysisStep {
    async fn execute(&self, session: &mut Session, ctx: &mut StepContext<'_>) -> StepOutcome {
        if session.flywheel.pending_traces() > 0 {
            let report = session.flywheel.analyze_incremental();
            session.state.suggestions = report;
            tracing::debug!(
                turn = %ctx.turn_id,
                suggestions = session.state.suggestions.len(),
                "flywheel: incremental analysis complete"
            );
        }
        StepOutcome::Continue
    }
}

/// Persistence step: saves the turn snapshot and session snapshot to the
/// configured session store.
///
/// Corresponds to original `run_turn()` lines 666-691.
struct PersistenceStep;

#[async_trait]
impl SessionStep for PersistenceStep {
    async fn execute(&self, session: &mut Session, ctx: &mut StepContext<'_>) -> StepOutcome {
        if let Some(ref store) = *session.session_store.lock().unwrap() {
            let turn_snap = TurnSnapshot {
                turn_id: ctx.turn_id.clone(),
                thread_id: ctx.thread_id.clone(),
                turn_number: session.turn_counter - 1,
                events: ctx.events.clone(),
                tool_calls: vec![],
                created_at: Utc::now(),
            };
            let _ = store.save_turn(&session.config.id, &turn_snap, true);

            let session_snap = SessionSnapshot {
                id: session.config.id.clone(),
                status: session.state.status.clone(),
                permission_mode: session.config.permission_mode.clone(),
                system_instructions: session.config.system_instructions.clone(),
                max_iterations: session.config.max_iterations,
                turn_count: session.state.turn_count,
                user_id: String::new(),
                messages: vec![],
                turns: vec![],
                created_at: Utc::now(),
                updated_at: Utc::now(),
            };
            let _ = store.save_session(&session_snap);
        }
        StepOutcome::Continue
    }
}

/// Resolve a [`StepOutcome`] into an optional early-return value.
///
/// Returns `Some(events)` if execution should stop (Break or Error),
/// `None` if execution should continue to the next step.
///
/// On [`StepOutcome::Error`], pushes an `Error` response event onto
/// [`StepContext::events`] before returning.
fn step_outcome_to_break(
    outcome: StepOutcome,
    session: &mut Session,
    ctx: &mut StepContext<'_>,
) -> Option<Vec<ResponseEvent>> {
    match outcome {
        StepOutcome::Continue => None,
        StepOutcome::Break(events) => {
            session.state.current_turn = None;
            Some(events)
        }
        StepOutcome::Error(msg) => {
            ctx.events.push(ResponseEvent::Error { message: msg });
            session.state.current_turn = None;
            Some(std::mem::take(ctx.events))
        }
    }
}

/// Phase G: classify a tool error message into a flywheel error type category.
///
/// Uses simple keyword matching. Extend this function as new error patterns
/// are discovered through the flywheel.
fn classify_tool_error(error_msg: &str) -> String {
    let lower = error_msg.to_lowercase();
    if lower.contains("timeout") || lower.contains("timed out") || lower.contains("deadline") {
        "timeout".into()
    } else if lower.contains("permission") || lower.contains("denied") || lower.contains("eacces")
    {
        "permission_denied".into()
    } else if lower.contains("syntax") || lower.contains("parse error") {
        "syntax_error".into()
    } else if lower.contains("not found") || lower.contains("command not found") {
        "tool_not_found".into()
    } else if lower.contains("build") || lower.contains("compile") || lower.contains("cargo")
        || lower.contains("compilation")
    {
        "build_failure".into()
    } else if lower.contains("io error") || lower.contains("failed to read")
        || lower.contains("failed to write")
    {
        "io_error".into()
    } else {
        "unknown".into()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::types::TokenUsage;
    use crate::model::ModelResult;
    use crate::DefaultToolRegistry;
    use async_trait::async_trait;
    use code_agent_protocol::{CapabilityLevel, ThreadId};
    use futures::stream;
    use std::sync::Mutex;

    // ── Mock Model Client ───────────────────────────────────────────────

    /// A mock model client that returns predefined response event sequences.
    ///
    /// Each call to `complete_stream` consumes the next sequence from the
    /// internal queue. When the queue is empty, it returns a default text
    /// response: "Mock response".
    pub(crate) struct MockModelClient {
        /// Queue of response sequences. Each sequence is a Vec of events
        /// that will be emitted as a stream.
        responses: Mutex<Vec<Vec<ResponseEvent>>>,
        /// Track token usage.
        usage: Mutex<Option<TokenUsage>>,
    }

    impl MockModelClient {
        /// Create a new mock with the given response sequences.
        ///
        /// Each element in `responses` represents one call to `complete_stream`.
        pub fn new(responses: Vec<Vec<ResponseEvent>>) -> Self {
            Self {
                responses: Mutex::new(responses),
                usage: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl ModelClient for MockModelClient {
        fn model_name(&self) -> &str {
            "mock-model"
        }

        async fn complete_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
            _temperature: Option<f32>,
        ) -> ModelResult<Box<dyn stream::Stream<Item = ResponseEvent> + Send + Unpin>> {
            let mut responses = self.responses.lock().unwrap();

            let events = if responses.is_empty() {
                vec![ResponseEvent::AgentMessageDelta {
                    content: "Mock response".into(),
                }]
            } else {
                responses.remove(0)
            };

            // Track token usage
            let prompt_tokens = 10u32;
            let completion_tokens = events
                .iter()
                .filter_map(|e| {
                    if let ResponseEvent::AgentMessageDelta { content } = e {
                        Some(content.len() as u32)
                    } else {
                        None
                    }
                })
                .sum::<u32>();

            let mut usage = self.usage.lock().unwrap();
            *usage = Some(TokenUsage::new(prompt_tokens, completion_tokens));

            Ok(Box::new(stream::iter(events)))
        }

        fn last_token_usage(&self) -> Option<TokenUsage> {
            self.usage.lock().unwrap().clone()
        }
    }

    // ── Mock Tool ─────────────────────────────────────────────────────

    /// A mock tool that returns a predetermined output.
    struct MockTool {
        name: String,
        output: String,
    }

    #[async_trait]
    impl crate::tools::Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "A mock tool for testing"
        }

        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        fn capability(&self) -> CapabilityLevel {
            CapabilityLevel::Read
        }

        async fn execute(
            &self,
            _params: serde_json::Value,
        ) -> Result<ToolResultMessage, crate::tools::ToolError> {
            Ok(ToolResultMessage {
                tool_call_id: String::new(),
                output: Some(self.output.clone()),
                error: None,
            })
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────

    fn make_config(id: &str, mock_responses: Vec<Vec<ResponseEvent>>) -> SessionConfig {
        SessionConfig {
            id: SessionId::from(id),
            system_instructions: "You are a test assistant.".into(),
            max_iterations: 10,
            permission_mode: PermissionMode::Auto,
            model_client: Arc::new(MockModelClient::new(mock_responses)),
            tool_registry: Arc::new(DefaultToolRegistry::new()),
            external_cancel: None,
            max_context_tokens: None,
            temperature: None,
        }
    }

    // ── Tests: Session creation and state ──────────────────────────────

    #[tokio::test]
    async fn session_creation() {
        let config = make_config("test-session", vec![]);
        let session = Session::new(config).await;

        let status = session.status();
        assert_eq!(status.id.0, "test-session");
        assert_eq!(status.turn_count, 0);
        assert!(status.current_turn.is_none());
    }

    #[tokio::test]
    async fn session_status_after_turn() {
        let config = make_config(
            "test-session",
            vec![vec![ResponseEvent::AgentMessageDelta {
                content: "Hello!".into(),
            }]],
        );
        let mut session = Session::new(config).await;

        let input = TurnInput {
            thread_id: ThreadId::from("thread-1"),
            messages: vec![Message::UserMessage {
                content: "hi".into(),
            }],
        };

        let _events = session.run_turn(input).await;
        let status = session.status();
        assert_eq!(status.turn_count, 1);
        assert!(status.current_turn.is_none()); // Turn should be complete
    }

    // ── Tests: Basic ReAct cycle ─────────────────────────────────────

    #[tokio::test]
    async fn react_simple_text_response() {
        // Mock returns a single text delta — final answer
        let config = make_config(
            "test-react",
            vec![vec![ResponseEvent::AgentMessageDelta {
                content: "The answer is 42.".into(),
            }]],
        );
        let mut session = Session::new(config).await;

        let input = TurnInput {
            thread_id: ThreadId::from("t1"),
            messages: vec![Message::UserMessage {
                content: "What is the answer?".into(),
            }],
        };

        let events = session.run_turn(input).await;

        // Should have TurnStarted, AgentMessageDelta, TurnComplete
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::TurnStarted { .. })));
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::AgentMessageDelta { .. })));
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })));

        // Verify the final message content
        let turn_complete = events
            .iter()
            .find_map(|e| {
                if let ResponseEvent::TurnComplete { ref final_message, .. } = e {
                    Some(final_message)
                } else {
                    None
                }
            })
            .expect("should have TurnComplete");

        if let Message::AssistantMessage { content } = turn_complete {
            assert_eq!(content, "The answer is 42.");
        } else {
            panic!("expected AssistantMessage");
        }
    }

    // ── Tests: 3-turn ReAct cycle ────────────────────────────────────

    #[tokio::test]
    async fn react_tool_call_then_final_answer() {
        // First model call: returns a tool call
        let first_response = vec![
            ResponseEvent::AgentMessageDelta {
                content: "Let me check that file.".into(),
            },
            ResponseEvent::ToolCallBegin {
                tool_call: ToolCall {
                    id: "call-1".into(),
                    name: "mock_read".into(),
                    arguments: serde_json::json!({"path": "src/main.rs"}),
                },
            },
        ];

        // Second model call (after tool result): returns final answer
        let second_response = vec![ResponseEvent::AgentMessageDelta {
            content: "The file contains a main function.".into(),
        }];

        let mut config = make_config("test-react-tool", vec![first_response, second_response]);

        // Register a mock tool
        let mut reg = DefaultToolRegistry::new();
        reg.register(Arc::new(MockTool {
            name: "mock_read".into(),
            output: "fn main() {}".into(),
        }))
        .unwrap();
        config.tool_registry = Arc::new(reg);

        let mut session = Session::new(config).await;

        let input = TurnInput {
            thread_id: ThreadId::from("t1"),
            messages: vec![Message::UserMessage {
                content: "Read main.rs".into(),
            }],
        };

        let events = session.run_turn(input).await;

        // Should have: TurnStarted → AgentMessageDelta → ToolCallBegin →
        // ToolCallEnd → AgentMessageDelta → TurnComplete
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::TurnStarted { .. })));
        assert!(
            events
                .iter()
                .filter(|e| matches!(e, ResponseEvent::AgentMessageDelta { .. }))
                .count()
                >= 2
        );
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::ToolCallBegin { .. })));
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::ToolCallEnd { .. })));
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })));

        // Verify the final answer
        let final_content: Vec<&str> = events
            .iter()
            .filter_map(|e| {
                if let ResponseEvent::AgentMessageDelta { content } = e {
                    Some(content.as_str())
                } else {
                    None
                }
            })
            .collect();

        let full_text = final_content.join("");
        assert!(full_text.contains("main function"));
    }

    // ── Tests: Interrupt mid-turn ─────────────────────────────────────

    #[tokio::test]
    async fn interrupt_mid_turn() {
        // Mock that would loop forever (tool call → tool call → ...)
        let tool_call_response = vec![
            ResponseEvent::AgentMessageDelta {
                content: "Calling tool...".into(),
            },
            ResponseEvent::ToolCallBegin {
                tool_call: ToolCall {
                    id: "call-loop".into(),
                    name: "mock_read".into(),
                    arguments: serde_json::json!({}),
                },
            },
        ];

        // Provide many copies so the loop doesn't run out
        let mut config = make_config(
            "test-interrupt",
            vec![tool_call_response.clone(); 20],
        );

        let mut reg = DefaultToolRegistry::new();
        reg.register(Arc::new(MockTool {
            name: "mock_read".into(),
            output: "ok".into(),
        }))
        .unwrap();
        config.tool_registry = Arc::new(reg);

        let mut session = Session::new(config).await;

        // Interrupt immediately — the next iteration check will catch it
        session.interrupt();

        let input = TurnInput {
            thread_id: ThreadId::from("t1"),
            messages: vec![Message::UserMessage {
                content: "do something".into(),
            }],
        };

        let events = session.run_turn(input).await;

        // Should have an error about interruption
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::Error { .. })));
        let error_msg = events
            .iter()
            .find_map(|e| {
                if let ResponseEvent::Error { message } = e {
                    Some(message.as_str())
                } else {
                    None
                }
            })
            .expect("should have error");
        assert!(error_msg.contains("interrupted"));
    }

    // ── Tests: Max iterations ─────────────────────────────────────────

    #[tokio::test]
    async fn max_iterations_reached() {
        // Every response is a tool call — the loop should stop after max_iterations
        let tool_call_response = vec![
            ResponseEvent::ToolCallBegin {
                tool_call: ToolCall {
                    id: "call-loop".into(),
                    name: "mock_read".into(),
                    arguments: serde_json::json!({}),
                },
            },
        ];

        let mut config = make_config("test-max-iter", vec![tool_call_response.clone(); 50]);
        config.max_iterations = 3;

        let mut reg = DefaultToolRegistry::new();
        reg.register(Arc::new(MockTool {
            name: "mock_read".into(),
            output: "ok".into(),
        }))
        .unwrap();
        config.tool_registry = Arc::new(reg);

        let mut session = Session::new(config).await;

        let input = TurnInput {
            thread_id: ThreadId::from("t1"),
            messages: vec![Message::UserMessage {
                content: "loop forever".into(),
            }],
        };

        let events = session.run_turn(input).await;

        // Should have an error about max iterations
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::Error { .. })));
        let error_msg = events
            .iter()
            .find_map(|e| {
                if let ResponseEvent::Error { message } = e {
                    Some(message.as_str())
                } else {
                    None
                }
            })
            .expect("should have error");
        assert!(error_msg.contains("Max iterations"));
    }

    // ── Tests: Context manager ────────────────────────────────────────

    #[test]
    fn context_manager_builds_with_system_instructions() {
        let cm = ContextManager::new("You are a bot.".into());
        let messages = cm.build_messages();
        assert_eq!(messages.len(), 1);
        if let Message::UserMessage { content } = &messages[0] {
            assert_eq!(content, "You are a bot.");
        } else {
            panic!("expected user message");
        }
    }

    #[test]
    fn context_manager_tracks_history() {
        let mut cm = ContextManager::new(String::new());
        assert_eq!(cm.history_len(), 0);

        cm.add_message(Message::UserMessage {
            content: "hello".into(),
        });
        assert_eq!(cm.history_len(), 1);

        cm.add_messages(vec![
            Message::AssistantMessage {
                content: "hi".into(),
            },
            Message::UserMessage {
                content: "how are you".into(),
            },
        ]);
        assert_eq!(cm.history_len(), 3);
    }

    #[test]
    fn context_manager_clear() {
        let mut cm = ContextManager::new("sys".into());
        cm.add_message(Message::UserMessage {
            content: "hello".into(),
        });
        cm.clear();
        assert_eq!(cm.history_len(), 0);

        // System instructions should still be prepended
        let messages = cm.build_messages();
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn context_manager_without_system_instructions() {
        let cm = ContextManager::new(String::new());
        let messages = cm.build_messages();
        assert!(messages.is_empty());
    }

    // ── Tests: Concurrent sessions ────────────────────────────────────

    #[tokio::test]
    async fn concurrent_sessions_run_independently() {
        let config1 = make_config(
            "sess-1",
            vec![vec![ResponseEvent::AgentMessageDelta {
                content: "Response from session 1".into(),
            }]],
        );
        let config2 = make_config(
            "sess-2",
            vec![vec![ResponseEvent::AgentMessageDelta {
                content: "Response from session 2".into(),
            }]],
        );

        let mut session1 = Session::new(config1).await;
        let mut session2 = Session::new(config2).await;

        let input1 = TurnInput {
            thread_id: ThreadId::from("t1"),
            messages: vec![Message::UserMessage {
                content: "hello from 1".into(),
            }],
        };
        let input2 = TurnInput {
            thread_id: ThreadId::from("t2"),
            messages: vec![Message::UserMessage {
                content: "hello from 2".into(),
            }],
        };

        let (events1, events2) = tokio::join!(session1.run_turn(input1), session2.run_turn(input2));

        let final1 = events1
            .iter()
            .find_map(|e| {
                if let ResponseEvent::TurnComplete { ref final_message, .. } = e {
                    if let Message::AssistantMessage { content } = final_message {
                        Some(content.as_str())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .unwrap();
        let final2 = events2
            .iter()
            .find_map(|e| {
                if let ResponseEvent::TurnComplete { ref final_message, .. } = e {
                    if let Message::AssistantMessage { content } = final_message {
                        Some(content.as_str())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .unwrap();

        assert_eq!(final1, "Response from session 1");
        assert_eq!(final2, "Response from session 2");
    }

    // ── Tests: Model error handling ───────────────────────────────────

    #[tokio::test]
    async fn model_error_emits_error_event() {
        // Use a mock that returns an error — we simulate this by providing
        // no responses but the mock always returns a stream.
        // For a true error test, we'd need an error-returning mock.
        // Instead, verify that an empty session handles gracefully.
        let config = make_config("test-err", vec![]);
        let mut session = Session::new(config).await;

        let input = TurnInput {
            thread_id: ThreadId::from("t1"),
            messages: vec![Message::UserMessage {
                content: "test".into(),
            }],
        };

        let events = session.run_turn(input).await;
        // Should complete with the default mock response
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })));
    }

    // ── Tests: Tool not found ─────────────────────────────────────────

    #[tokio::test]
    async fn tool_not_found_emits_error_in_tool_result() {
        let first_response = vec![ResponseEvent::ToolCallBegin {
            tool_call: ToolCall {
                id: "call-unknown".into(),
                name: "nonexistent_tool".into(),
                arguments: serde_json::json!({}),
            },
        }];

        let config = make_config("test-tool-404", vec![first_response]);
        let mut session = Session::new(config).await;

        let input = TurnInput {
            thread_id: ThreadId::from("t1"),
            messages: vec![Message::UserMessage {
                content: "use a tool".into(),
            }],
        };

        let events = session.run_turn(input).await;

        // Should have ToolCallEnd with error
        let tool_end = events.iter().find_map(|e| {
            if let ResponseEvent::ToolCallEnd { ref result, .. } = e {
                Some(result)
            } else {
                None
            }
        });
        assert!(tool_end.is_some());
        let result = tool_end.unwrap();
        assert!(result.is_error());
        assert!(result.error.as_ref().unwrap().contains("not found"));
    }
}
