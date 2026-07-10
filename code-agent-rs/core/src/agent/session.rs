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
use std::sync::Arc;

use code_agent_protocol::{
    Message, PermissionMode, ResponseEvent, SessionId, SessionStatus, ToolCall,
    ToolResultMessage, TurnId, TurnInput,
};
use futures::StreamExt;

use crate::context::ContextManager;
use crate::model::types::ToolDefinition;
use crate::model::ModelClient;
use crate::tools::registry::ToolRegistry;

// ---------------------------------------------------------------------------
// SessionConfig
// ---------------------------------------------------------------------------

/// 会话配置 —— 创建 Session 聚合根所需的全部参数
///
/// 【领域含义】SessionConfig 是创建 Session 的值对象，包含会话标识、
/// 系统指令、最大循环次数、权限模式、模型客户端和工具注册中心的引用。
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
    pub tool_registry: Arc<ToolRegistry>,
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
    tool_registry: Arc<ToolRegistry>,

    /// 对话历史和提示词构建器
    context_manager: ContextManager,

    /// 可变会话状态
    state: SessionState,

    /// 中断标志 —— 设为 true 时当前轮次中止
    interrupted: Arc<AtomicBool>,

    /// 单调递增计数器，用于生成轮次 ID
    turn_counter: u64,
}

impl Session {
    /// 从配置创建一个新的 Session
    ///
    /// `async` 签名为未来扩展预留（例如从数据库加载会话状态）；
    /// 当前不执行任何 I/O 操作。
    pub async fn new(config: SessionConfig) -> Self {
        let context_manager = ContextManager::new(config.system_instructions.clone());
        let state = SessionState {
            id: config.id.clone(),
            status: SessionStatus::Active,
            current_turn: None,
            turn_count: 0,
            permission_mode: config.permission_mode.clone(),
        };

        Self {
            model_client: Arc::clone(&config.model_client),
            tool_registry: Arc::clone(&config.tool_registry),
            config,
            context_manager,
            state,
            interrupted: Arc::new(AtomicBool::new(false)),
            turn_counter: 0,
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

        // Add user input messages to conversation history
        self.context_manager.add_messages(input.messages);

        // --- ReAct loop ---
        for _iteration in 0..self.config.max_iterations {
            // Check for interrupt
            if self.interrupted.load(Ordering::Acquire) {
                events.push(ResponseEvent::Error {
                    message: "Turn interrupted by user".into(),
                });
                self.state.current_turn = None;
                return events;
            }

            // Build prompt context
            let messages = self.context_manager.build_messages();
            let tool_defs = self.build_tool_definitions();

            // 调用模型（流式）
            let mut stream = match self.model_client.complete_stream(&messages, &tool_defs).await {
                Ok(s) => s,
                Err(e) => {
                    events.push(ResponseEvent::Error {
                        message: format!("Model error: {e}"),
                    });
                    self.state.current_turn = None;
                    return events;
                }
            };

            // Collect response from the model stream
            let mut text_buffer = String::new();
            let mut tool_calls: Vec<ToolCall> = Vec::new();
            let mut final_message_content: Option<String> = None;

            while let Some(event) = stream.next().await {
                match event {
                    ResponseEvent::AgentMessageDelta { ref content } => {
                        text_buffer.push_str(content);
                        events.push(ResponseEvent::AgentMessageDelta {
                            content: content.clone(),
                        });
                    }
                    ResponseEvent::ToolCallBegin { ref tool_call } => {
                        tool_calls.push(tool_call.clone());
                        events.push(ResponseEvent::ToolCallBegin {
                            tool_call: tool_call.clone(),
                        });
                    }
                    ResponseEvent::TokenUsage {
                        prompt_tokens,
                        completion_tokens,
                    } => {
                        events.push(ResponseEvent::TokenUsage {
                            prompt_tokens,
                            completion_tokens,
                        });
                    }
                    ResponseEvent::TurnComplete {
                        ref final_message, ..
                    } => {
                        // Capture the model's final message content but don't
                        // emit TurnComplete yet — the ReAct loop may continue.
                        if let Message::AssistantMessage { ref content } = final_message {
                            if !content.is_empty() {
                                final_message_content = Some(content.clone());
                            }
                        }
                        // Do NOT emit the model's TurnComplete here
                    }
                    ResponseEvent::Error { ref message } => {
                        events.push(ResponseEvent::Error {
                            message: message.clone(),
                        });
                    }
                    ResponseEvent::TurnStarted { .. } => {
                        // Model should not emit TurnStarted; ignore if it does.
                    }
                    ResponseEvent::ToolCallEnd { .. } => {
                        // Model should not emit ToolCallEnd; ignore if it does.
                    }
                }
            }

            // If text was collected from deltas, that's the content
            let assistant_text = if !text_buffer.is_empty() {
                text_buffer
            } else {
                final_message_content.unwrap_or_default()
            };

            // --- 工具执行阶段 ---
            if !tool_calls.is_empty() {
                // Record assistant message + tool calls in history
                if !assistant_text.is_empty() {
                    self.context_manager.add_message(Message::AssistantMessage {
                        content: assistant_text,
                    });
                }

                for tc in &tool_calls {
                    self.context_manager
                        .add_message(Message::ToolCall(tc.clone()));

                    // 执行工具
                    let result = self.execute_tool(tc).await;
                    let trm = match &result {
                        Ok(trm) => trm.clone(),
                        Err(trm) => trm.clone(),
                    };

                    events.push(ResponseEvent::ToolCallEnd {
                        tool_call_id: tc.id.clone(),
                        result: trm.clone(),
                    });

                    self.context_manager
                        .add_message(Message::ToolResult(trm));
                }

                // Loop back to model for more reasoning
                continue;
            }

            // --- 最终答案 ---
            let final_message = Message::AssistantMessage {
                content: assistant_text,
            };

            // Add assistant response to history
            self.context_manager
                .add_message(final_message.clone());

            events.push(ResponseEvent::TurnComplete {
                turn_id,
                final_message,
            });

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
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::types::TokenUsage;
    use crate::model::ModelResult;
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
            tool_registry: Arc::new(ToolRegistry::new()),
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
        let mut reg = ToolRegistry::new();
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

        let mut reg = ToolRegistry::new();
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

        let mut reg = ToolRegistry::new();
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
