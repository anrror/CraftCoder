//! End-to-end integration test suite for the AI Coding Agent.
//!
//! Tests the full agent pipeline using mock model clients and mock tools.
//! No external dependencies are needed.
//!
//! # Test Categories
//!
//! 1. **Complete coding task**: Write fn → test → fix → commit workflow
//! 2. **Debug workflow**: Find bug → fix → verify
//! 3. **Multi-file refactor**: Rename across files → verify
//! 4. **Interruption and resume**: Ctrl+C → resume
//! 5. **Performance benchmarks**: Startup, turn latency, compaction, search
//! 6. **Stress**: 50-turn session with large files
//! 7. **Security**: Sandbox, permissions, no leakage

// ===========================================================================
// Shared mock implementations
// ===========================================================================

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use code_agent_core::agent::{Session, SessionConfig};
use code_agent_core::context::ContextManager;
use code_agent_core::model::types::{TokenUsage, ToolDefinition};
use code_agent_core::model::{ModelClient, ModelResult};
use code_agent_core::tools::permission::PermissionEnforcer;
use code_agent_core::tools::registry::{DefaultToolRegistry, ToolRegistry};
use code_agent_core::tools::router::ToolRouter;
use code_agent_core::tools::Tool;
use code_agent_protocol::{
    CapabilityLevel, Message, PermissionMode, ResponseEvent, SessionId, ThreadId, ToolCall,
    ToolResultMessage, TurnInput,
};
use futures::stream;

// ---------------------------------------------------------------------------
// MockModelClient
// ---------------------------------------------------------------------------

/// A mock model client that returns predefined response event sequences.
/// Each call to `complete_stream` consumes the next sequence from the queue.
pub struct MockModelClient {
    responses: Mutex<Vec<Vec<ResponseEvent>>>,
    usage: Mutex<Option<TokenUsage>>,
}

impl MockModelClient {
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

// ---------------------------------------------------------------------------
// MockTool
// ---------------------------------------------------------------------------

/// A mock tool that returns predefined outputs in sequence.
struct MockTool {
    name: String,
    outputs: Vec<String>,
    call_count: AtomicUsize,
    capability: CapabilityLevel,
}

impl MockTool {
    fn new(name: &str, outputs: Vec<&str>) -> Self {
        Self {
            name: name.to_string(),
            outputs: outputs.into_iter().map(|s| s.to_string()).collect(),
            call_count: AtomicUsize::new(0),
            capability: CapabilityLevel::Read,
        }
    }

    fn with_capability(mut self, cap: CapabilityLevel) -> Self {
        self.capability = cap;
        self
    }
}

#[async_trait]
impl Tool for MockTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "Mock tool for e2e testing"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn capability(&self) -> CapabilityLevel {
        self.capability.clone()
    }
    async fn execute(
        &self,
        _params: serde_json::Value,
    ) -> Result<ToolResultMessage, code_agent_core::tools::ToolError> {
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        let output = self
            .outputs
            .get(idx)
            .cloned()
            .unwrap_or_else(|| format!("mock output {}", idx));
        Ok(ToolResultMessage {
            tool_call_id: String::new(),
            output: Some(output),
            error: None,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_config(
    id: &str,
    responses: Vec<Vec<ResponseEvent>>,
    registry: DefaultToolRegistry,
) -> SessionConfig {
    SessionConfig {
        id: SessionId::from(id),
        system_instructions: "You are a coding assistant.".into(),
        max_iterations: 20,
        permission_mode: PermissionMode::Auto,
        model_client: Arc::new(MockModelClient::new(responses)),
        tool_registry: Arc::new(registry),
        tool_router: None,
        hook_registry: None,
        external_cancel: None,
        max_context_tokens: None,
        temperature: None,
        knowledge: None,
        quality_gate: None,
    }
}

fn make_user_msg(content: &str) -> Message {
    Message::UserMessage {
        content: content.to_string(),
    }
}

fn make_assistant_msg(content: &str) -> Message {
    Message::AssistantMessage {
        content: content.to_string(),
    }
}

fn large_text(tokens: usize) -> String {
    let chars_needed = tokens * 4;
    let word = "test_data_";
    let repeats = chars_needed / word.len() + 1;
    word.repeat(repeats)
}

// ===========================================================================
// 1. Complete Coding Task Workflow
// ===========================================================================

#[tokio::test]
async fn e2e_complete_coding_task_workflow() {
    // Agent: write_fn → write_test → run_test(fail) → fix_fn → run_test(pass) → commit
    let responses = vec![
        vec![
            ResponseEvent::AgentMessageDelta { content: "Writing function...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c1".into(), name: "write_fn".into(), arguments: serde_json::json!({"name":"fib"}) } },
        ],
        vec![
            ResponseEvent::AgentMessageDelta { content: "Writing test...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c2".into(), name: "write_test".into(), arguments: serde_json::json!({"test":"test_fib"}) } },
        ],
        vec![
            ResponseEvent::AgentMessageDelta { content: "Running test...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c3".into(), name: "run_test".into(), arguments: serde_json::json!({"test":"test_fib"}) } },
        ],
        vec![
            ResponseEvent::AgentMessageDelta { content: "Test failed, fixing...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c4".into(), name: "fix_fn".into(), arguments: serde_json::json!({"name":"fib"}) } },
        ],
        vec![
            ResponseEvent::AgentMessageDelta { content: "Re-running test...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c5".into(), name: "run_test".into(), arguments: serde_json::json!({"test":"test_fib"}) } },
        ],
        vec![
            ResponseEvent::AgentMessageDelta { content: "Test passes, committing...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c6".into(), name: "commit".into(), arguments: serde_json::json!({"msg":"add fib"}) } },
        ],
        vec![ResponseEvent::AgentMessageDelta {
            content: "Task complete: fibonacci function written, tested, and committed.".into() }],
    ];

    let mut reg = DefaultToolRegistry::new();
    reg.register(Arc::new(MockTool::new("write_fn", vec!["fn fib() {}"]))).unwrap();
    reg.register(Arc::new(MockTool::new("write_test", vec!["test written"]))).unwrap();
    reg.register(Arc::new(MockTool::new("run_test", vec![
        "FAILED: test_fib - timeout",
        "PASSED: test_fib (0.002s)",
    ]))).unwrap();
    reg.register(Arc::new(MockTool::new("fix_fn", vec!["fn fib() { 0 }"]))).unwrap();
    reg.register(Arc::new(MockTool::new("commit", vec!["commit abc123"]))).unwrap();

    let mut session = Session::new(make_config("e2e-coding-task", responses, reg)).await;
    let events = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Write fibonacci, test, fix, commit.")],
    }).await;

    assert!(events.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })));

    let tool_calls: Vec<&str> = events.iter().filter_map(|e| {
        if let ResponseEvent::ToolCallBegin { tool_call } = e { Some(tool_call.name.as_str()) } else { None }
    }).collect();
    assert_eq!(tool_calls, vec!["write_fn", "write_test", "run_test", "fix_fn", "run_test", "commit"],
        "Tool call order should match coding workflow");

    let tool_results: Vec<&str> = events.iter().filter_map(|e| {
        if let ResponseEvent::ToolCallEnd { ref result, .. } = e { result.output.as_deref() } else { None }
    }).collect();
    assert!(tool_results.iter().any(|r| r.contains("FAILED")));
    assert!(tool_results.iter().any(|r| r.contains("PASSED")));

    let final_msg = events.iter().find_map(|e| {
        if let ResponseEvent::TurnComplete { ref final_message, .. } = e {
            if let Message::AssistantMessage { content } = final_message { Some(content.as_str()) } else { None }
        } else { None }
    });
    assert_eq!(final_msg, Some("Task complete: fibonacci function written, tested, and committed."));
}

#[tokio::test]
async fn e2e_coding_task_text_only() {
    let responses = vec![vec![ResponseEvent::AgentMessageDelta {
        content: "Here's the function: fn fib(n: u64) -> u64 { n }".into(),
    }]];
    let mut session = Session::new(make_config("e2e-coding-text", responses, DefaultToolRegistry::new())).await;
    let events = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Write a fib function.")],
    }).await;
    assert!(events.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })));
    assert!(!events.iter().any(|e| matches!(e, ResponseEvent::ToolCallBegin { .. })));
}

// ===========================================================================
// 2. Debug Workflow
// ===========================================================================

#[tokio::test]
async fn e2e_debug_workflow() {
    // Agent: read_file → analyze → edit_file → verify
    let responses = vec![
        vec![
            ResponseEvent::AgentMessageDelta { content: "Reading buggy file...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c1".into(), name: "read_file".into(), arguments: serde_json::json!({"path":"src/lib.rs"}) } },
        ],
        vec![
            ResponseEvent::AgentMessageDelta { content: "Analyzing bug...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c2".into(), name: "analyze".into(), arguments: serde_json::json!({"issue":"div_zero"}) } },
        ],
        vec![
            ResponseEvent::AgentMessageDelta { content: "Fixing bug...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c3".into(), name: "edit_file".into(), arguments: serde_json::json!({"path":"src/lib.rs"}) } },
        ],
        vec![
            ResponseEvent::AgentMessageDelta { content: "Verifying fix...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c4".into(), name: "verify".into(), arguments: serde_json::json!({"test":"test_div"}) } },
        ],
        vec![ResponseEvent::AgentMessageDelta {
            content: "Bug fixed: divide() now returns Err for zero divisor. Verification passed.".into() }],
    ];

    let mut reg = DefaultToolRegistry::new();
    reg.register(Arc::new(MockTool::new("read_file", vec!["fn divide(a: i32, b: i32) -> Result<i32, String> { Ok(a / b) }"]))).unwrap();
    reg.register(Arc::new(MockTool::new("analyze", vec!["Bug: division by zero at line 5"]))).unwrap();
    reg.register(Arc::new(MockTool::new("edit_file", vec!["src/lib.rs patched"]))).unwrap();
    reg.register(Arc::new(MockTool::new("verify", vec!["PASSED: test_div (0.001s)"]))).unwrap();

    let mut session = Session::new(make_config("e2e-debug", responses, reg)).await;
    let events = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Fix the divide-by-zero bug in calculator.")],
    }).await;

    assert!(events.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })));

    let tool_calls: Vec<&str> = events.iter().filter_map(|e| {
        if let ResponseEvent::ToolCallBegin { tool_call } = e { Some(tool_call.name.as_str()) } else { None }
    }).collect();
    assert_eq!(tool_calls, vec!["read_file", "analyze", "edit_file", "verify"]);

    let final_msg = events.iter().find_map(|e| {
        if let ResponseEvent::TurnComplete { ref final_message, .. } = e {
            if let Message::AssistantMessage { content } = final_message { Some(content.as_str()) } else { None }
        } else { None }
    });
    assert!(final_msg.unwrap_or("").contains("Bug fixed"));
    assert!(final_msg.unwrap_or("").contains("zero divisor"));
}

#[tokio::test]
async fn e2e_debug_unknown_tool() {
    let responses = vec![vec![
        ResponseEvent::AgentMessageDelta { content: "Using special debugger...".into() },
        ResponseEvent::ToolCallBegin { tool_call: ToolCall {
            id: "c1".into(), name: "nonexistent_tool".into(), arguments: serde_json::json!({}) } },
    ]];
    let mut session = Session::new(make_config("e2e-debug-unknown", responses, DefaultToolRegistry::new())).await;
    let events = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Debug this.")],
    }).await;
    let tool_errors: Vec<&str> = events.iter().filter_map(|e| {
        if let ResponseEvent::ToolCallEnd { ref result, .. } = e { result.error.as_deref() } else { None }
    }).collect();
    assert!(!tool_errors.is_empty());
    assert!(tool_errors.iter().any(|e| e.contains("not found")));
}

// ===========================================================================
// 3. Multi-file Refactor
// ===========================================================================

#[tokio::test]
async fn e2e_multi_file_refactor() {
    // Agent: grep → read×3 → edit×3 → verify
    let responses = vec![
        vec![
            ResponseEvent::AgentMessageDelta { content: "Searching for usages...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c1".into(), name: "grep".into(), arguments: serde_json::json!({"pattern":"old_fn"}) } },
        ],
        vec![
            ResponseEvent::AgentMessageDelta { content: "Reading file 1...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c2".into(), name: "read_file".into(), arguments: serde_json::json!({"path":"src/a.rs"}) } },
        ],
        vec![
            ResponseEvent::AgentMessageDelta { content: "Reading file 2...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c3".into(), name: "read_file".into(), arguments: serde_json::json!({"path":"src/b.rs"}) } },
        ],
        vec![
            ResponseEvent::AgentMessageDelta { content: "Reading file 3...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c4".into(), name: "read_file".into(), arguments: serde_json::json!({"path":"src/c.rs"}) } },
        ],
        vec![
            ResponseEvent::AgentMessageDelta { content: "Editing file 1...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c5".into(), name: "edit_file".into(), arguments: serde_json::json!({"path":"src/a.rs"}) } },
        ],
        vec![
            ResponseEvent::AgentMessageDelta { content: "Editing file 2...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c6".into(), name: "edit_file".into(), arguments: serde_json::json!({"path":"src/b.rs"}) } },
        ],
        vec![
            ResponseEvent::AgentMessageDelta { content: "Editing file 3...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c7".into(), name: "edit_file".into(), arguments: serde_json::json!({"path":"src/c.rs"}) } },
        ],
        vec![
            ResponseEvent::AgentMessageDelta { content: "Verifying build...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c8".into(), name: "verify_build".into(), arguments: serde_json::json!({"cmd":"cargo check"}) } },
        ],
        vec![ResponseEvent::AgentMessageDelta {
            content: "Refactor complete: renamed `old_fn` to `new_fn` across 3 files. Build passes.".into() }],
    ];

    let mut reg = DefaultToolRegistry::new();
    reg.register(Arc::new(MockTool::new("grep", vec!["src/a.rs:10: old_fn\nsrc/b.rs:25: old_fn\nsrc/c.rs:42: old_fn"]))).unwrap();
    reg.register(Arc::new(MockTool::new("read_file", vec![
        "pub fn old_fn() {}", "fn caller() { old_fn(); }", "let x = old_fn();",
    ]))).unwrap();
    reg.register(Arc::new(MockTool::new("edit_file", vec![
        "src/a.rs: 1 occurrence replaced",
        "src/b.rs: 1 occurrence replaced",
        "src/c.rs: 1 occurrence replaced",
    ]))).unwrap();
    reg.register(Arc::new(MockTool::new("verify_build", vec!["cargo check succeeded (0 errors)"]))).unwrap();

    let mut session = Session::new(make_config("e2e-refactor", responses, reg)).await;
    let events = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Rename `old_fn` to `new_fn` across the codebase.")],
    }).await;

    assert!(events.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })));

    let tool_calls: Vec<&str> = events.iter().filter_map(|e| {
        if let ResponseEvent::ToolCallBegin { tool_call } = e { Some(tool_call.name.as_str()) } else { None }
    }).collect();
    assert_eq!(tool_calls, vec!["grep", "read_file", "read_file", "read_file", "edit_file", "edit_file", "edit_file", "verify_build"]);

    let edit_count = tool_calls.iter().filter(|&&n| n == "edit_file").count();
    assert_eq!(edit_count, 3);

    let final_msg = events.iter().find_map(|e| {
        if let ResponseEvent::TurnComplete { ref final_message, .. } = e {
            if let Message::AssistantMessage { content } = final_message { Some(content.as_str()) } else { None }
        } else { None }
    });
    let msg = final_msg.unwrap_or("");
    assert!(msg.contains("Refactor complete"));
    assert!(msg.contains("3 files"));
}

#[tokio::test]
async fn e2e_refactor_no_usages() {
    let responses = vec![
        vec![
            ResponseEvent::AgentMessageDelta { content: "Searching...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c1".into(), name: "grep".into(), arguments: serde_json::json!({"pattern":"obsolete"}) } },
        ],
        vec![ResponseEvent::AgentMessageDelta { content: "No usages found. Nothing to refactor.".into() }],
    ];
    let mut reg = DefaultToolRegistry::new();
    reg.register(Arc::new(MockTool::new("grep", vec!["No matches found"]))).unwrap();
    let mut session = Session::new(make_config("e2e-refactor-none", responses, reg)).await;
    let events = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Rename `obsolete` to `new`.")],
    }).await;
    assert!(events.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })));
    let edit_calls: Vec<&str> = events.iter().filter_map(|e| {
        if let ResponseEvent::ToolCallBegin { tool_call } = e { if tool_call.name == "edit_file" { Some("") } else { None } } else { None }
    }).collect();
    assert!(edit_calls.is_empty());
}

// ===========================================================================
// 4. Interruption and Resume
// ===========================================================================

#[tokio::test]
async fn e2e_interrupt_before_turn() {
    let mut session = Session::new(make_config("e2e-int-before", vec![
        vec![ResponseEvent::AgentMessageDelta { content: "Response.".into() }],
    ], DefaultToolRegistry::new())).await;

    session.interrupt();
    let events = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Do something.")],
    }).await;
    assert!(events.iter().any(|e| matches!(e, ResponseEvent::Error { .. })));

    // Resume should work
    let events2 = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Now do this.")],
    }).await;
    assert!(events2.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })));
}

#[tokio::test]
async fn e2e_interrupt_and_resume() {
    let tool_call = vec![
        ResponseEvent::AgentMessageDelta { content: "Starting...".into() },
        ResponseEvent::ToolCallBegin { tool_call: ToolCall {
            id: "c1".into(), name: "long_op".into(), arguments: serde_json::json!({"ms":5000}) } },
    ];
    let mut responses: Vec<Vec<ResponseEvent>> = (0..10).map(|_| tool_call.clone()).collect();
    responses.push(vec![ResponseEvent::AgentMessageDelta { content: "Done.".into() }]);

    let mut reg = DefaultToolRegistry::new();
    reg.register(Arc::new(MockTool::new("long_op", vec!["completed"]))).unwrap();

    let mut session = Session::new(make_config("e2e-int-resume", responses, reg)).await;

    // Interrupt
    session.interrupt();
    let events1 = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Run long op.")],
    }).await;
    assert!(events1.iter().any(|e| matches!(e, ResponseEvent::Error { .. })));

    // Resume
    let events2 = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Continue.")],
    }).await;
    assert!(events2.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })));

    let state = session.status();
    assert_eq!(state.turn_count, 1);
    assert!(state.current_turn.is_none());
}

#[tokio::test]
async fn e2e_multiple_interrupts() {
    let mut session = Session::new(make_config("e2e-multi-int", vec![
        vec![ResponseEvent::AgentMessageDelta { content: "R1".into() }],
        vec![ResponseEvent::AgentMessageDelta { content: "R2".into() }],
        vec![ResponseEvent::AgentMessageDelta { content: "R3".into() }],
    ], DefaultToolRegistry::new())).await;

    session.interrupt();
    session.interrupt(); // second is no-op

    let events = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Try 1.")],
    }).await;
    assert!(events.iter().any(|e| matches!(e, ResponseEvent::Error { .. })));

    let events2 = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Try 2.")],
    }).await;
    assert!(events2.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })));

    let events3 = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Try 3.")],
    }).await;
    assert!(events3.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })));

    assert_eq!(session.status().turn_count, 2);
}

// ===========================================================================
// 5. Performance Benchmarks
// ===========================================================================

#[tokio::test]
async fn e2e_benchmark_startup() {
    let start = std::time::Instant::now();
    let session = Session::new(make_config("bench-startup", vec![], DefaultToolRegistry::new())).await;
    let elapsed = start.elapsed();
    assert_eq!(session.status().id.0, "bench-startup");
    assert!(elapsed.as_millis() < 1000, "Startup took {}ms", elapsed.as_millis());
}

#[tokio::test]
async fn e2e_benchmark_turn_latency() {
    let mut session = Session::new(make_config("bench-turn", vec![
        vec![ResponseEvent::AgentMessageDelta { content: "Quick response.".into() }],
    ], DefaultToolRegistry::new())).await;
    let start = std::time::Instant::now();
    let events = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Quick question.")],
    }).await;
    let elapsed = start.elapsed();
    assert!(events.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })));
    assert!(elapsed.as_millis() < 500, "Turn latency {}ms", elapsed.as_millis());
}

#[tokio::test]
async fn e2e_benchmark_compaction() {
    let mut cm = ContextManager::new("You are a coding assistant.".into());
    for i in 0..100 {
        cm.add_message(make_user_msg(&format!("Turn {}: {}", i + 1, large_text(500))));
        cm.add_message(make_assistant_msg(&format!("Response {}: {}", i + 1, large_text(400))));
    }
    let start = std::time::Instant::now();
    let token_count = cm.token_count();
    let compaction_count = cm.compaction_count();
    let elapsed = start.elapsed();

    assert!(elapsed.as_millis() < 1000, "Compaction took {}ms", elapsed.as_millis());
    assert!(token_count <= 64_000, "After compaction: {} tokens > 64K", token_count);
    assert!(compaction_count > 0, "Compaction should have triggered");
    assert!(!cm.compaction_history().is_empty());
}

#[tokio::test]
async fn e2e_benchmark_history_search() {
    let mut cm = ContextManager::new("sys".into());
    for i in 0..50 {
        cm.add_message(make_user_msg(&format!("Request {}", i + 1)));
        cm.add_message(make_assistant_msg(&format!("Response {}", i + 1)));
    }
    let start = std::time::Instant::now();
    let messages = cm.build_messages();
    let found: Vec<&Message> = messages.iter().filter(|m| {
        if let Message::AssistantMessage { content } = m { content.contains("Response 42") } else { false }
    }).collect();
    let elapsed = start.elapsed();
    assert!(!found.is_empty());
    assert!(elapsed.as_millis() < 100, "Search took {}ms", elapsed.as_millis());
}

#[tokio::test]
async fn e2e_benchmark_multi_turn_throughput() {
    let mut all_responses = Vec::new();
    for i in 0..10 {
        all_responses.push(vec![ResponseEvent::AgentMessageDelta {
            content: format!("Response {}.", i + 1),
        }]);
    }
    let mut session = Session::new(make_config("bench-throughput", all_responses, DefaultToolRegistry::new())).await;
    let start = std::time::Instant::now();
    for i in 0..10 {
        let events = session.run_turn(TurnInput {
            thread_id: ThreadId::from("t1"),
            messages: vec![make_user_msg(&format!("Input {}.", i + 1))],
        }).await;
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })));
    }
    let elapsed = start.elapsed();
    let avg = elapsed.as_millis() as f64 / 10.0;
    assert!(avg < 200.0, "Avg turn latency {:.1}ms", avg);
    assert_eq!(session.status().turn_count, 10);
}

// ===========================================================================
// 6. Stress Tests
// ===========================================================================

#[tokio::test]
async fn e2e_stress_50_turns_large_files() {
    let mut all_responses = Vec::new();
    for i in 0..50 {
        // Each turn needs 2 model calls: one with tool call, one with final answer
        all_responses.push(vec![
            ResponseEvent::AgentMessageDelta { content: format!("Turn {}...", i + 1) },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: format!("c{}", i), name: "read_large".into(), arguments: serde_json::json!({"path":format!("f{}.rs",i)}) } },
        ]);
        all_responses.push(vec![ResponseEvent::AgentMessageDelta {
            content: format!("Turn {} done.", i + 1),
        }]);
    }

    let mut reg = DefaultToolRegistry::new();
    reg.register(Arc::new(MockTool::new("read_large", vec![&large_text(2000); 50]))).unwrap();

    let mut session = Session::new(make_config("stress-50", all_responses, reg)).await;
    for i in 0..50 {
        let events = session.run_turn(TurnInput {
            thread_id: ThreadId::from("t1"),
            messages: vec![make_user_msg(&format!("Read file {}.", i))],
        }).await;
        assert!(
            events.iter().any(|e| matches!(e, ResponseEvent::ToolCallBegin { .. })),
            "Turn {} should have ToolCallBegin. Events: {:?}",
            i,
            events.iter().map(|e| match e {
                ResponseEvent::TurnStarted { .. } => "TurnStarted",
                ResponseEvent::AgentMessageDelta { .. } => "Delta",
                ResponseEvent::ToolCallBegin { .. } => "ToolCallBegin",
                ResponseEvent::ToolCallEnd { .. } => "ToolCallEnd",
                ResponseEvent::TurnComplete { .. } => "TurnComplete",
                ResponseEvent::Error { .. } => "Error",
                ResponseEvent::TokenUsage { .. } => "TokenUsage",
            }).collect::<Vec<_>>()
        );
    }
    assert_eq!(session.status().turn_count, 50);
}

#[tokio::test]
async fn e2e_stress_context_compaction() {
    let mut cm = ContextManager::new("You are a coding assistant.".into());
    for i in 0..50 {
        cm.add_message(make_user_msg(&format!("Req {}: {}", i + 1, large_text(1000))));
        cm.add_message(make_assistant_msg(&format!("Resp {}: {}", i + 1, large_text(800))));
    }
    assert!(cm.token_count() <= 64_000, "Tokens {} > 64K", cm.token_count());
    assert!(!cm.compaction_history().is_empty());
    let messages = cm.build_messages();
    assert!(messages.iter().any(|m| {
        if let Message::AssistantMessage { content } = m { content.contains("Resp 50") } else { false }
    }));
}

#[tokio::test]
async fn e2e_stress_concurrent_sessions() {
    let mut sessions = Vec::new();
    for i in 0..5 {
        sessions.push(Session::new(make_config(&format!("stress-con-{}", i+1), vec![
            vec![ResponseEvent::AgentMessageDelta { content: format!("Session {} response.", i+1) }],
        ], DefaultToolRegistry::new())).await);
    }
    let mut handles = Vec::new();
    for (i, mut s) in sessions.into_iter().enumerate() {
        handles.push(tokio::spawn(async move {
            s.run_turn(TurnInput {
                thread_id: ThreadId::from(format!("t{}", i+1)),
                messages: vec![make_user_msg(&format!("Hello from {}", i+1))],
            }).await
        }));
    }
    for (i, h) in handles.into_iter().enumerate() {
        let events = h.await.expect("concurrent session");
        assert!(events.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })),
            "Session {} should complete", i + 1);
    }
}

#[tokio::test]
async fn e2e_stress_large_tool_output() {
    let responses = vec![
        vec![
            ResponseEvent::AgentMessageDelta { content: "Reading huge file...".into() },
            ResponseEvent::ToolCallBegin { tool_call: ToolCall {
                id: "c1".into(), name: "read_huge".into(), arguments: serde_json::json!({"path":"huge.rs"}) } },
        ],
        vec![ResponseEvent::AgentMessageDelta { content: "File read complete.".into() }],
    ];
    let mut reg = DefaultToolRegistry::new();
    reg.register(Arc::new(MockTool::new("read_huge", vec![&large_text(10_000)]))).unwrap();
    let mut session = Session::new(make_config("stress-large-output", responses, reg)).await;
    let events = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Read huge file.")],
    }).await;
    assert!(events.iter().any(|e| matches!(e, ResponseEvent::TurnComplete { .. })));
    let tool_results: Vec<&ToolResultMessage> = events.iter().filter_map(|e| {
        if let ResponseEvent::ToolCallEnd { ref result, .. } = e { Some(result) } else { None }
    }).collect();
    assert!(!tool_results.is_empty());
    assert!(tool_results.iter().all(|r| r.is_success()));
}

// ===========================================================================
// 7. Security Tests
// ===========================================================================

#[tokio::test]
async fn e2e_security_block_mode_prevents_edit() {
    // Note: Session.execute_tool() directly calls tool.execute() without
    // permission checking. Permission enforcement is done via ToolRouter.
    // This test verifies that the ToolRouter correctly denies Edit tools
    // in Block mode.
    let mut reg = DefaultToolRegistry::new();
    reg.register(Arc::new(MockTool::new("write_file", vec!["written"]).with_capability(CapabilityLevel::Edit))).unwrap();
    let router = ToolRouter::new(Arc::new(reg), Arc::new(PermissionEnforcer::new(PermissionMode::Block)));
    let result = router.route(&ToolCall {
        id: "c1".into(), name: "write_file".into(), arguments: serde_json::json!({"path":"/etc/passwd"}),
    }).await;
    assert!(result.is_error());
    assert!(result.error.as_deref().unwrap_or("").contains("permission denied"));
}

#[tokio::test]
async fn e2e_security_auto_mode_allows_all() {
    let responses = vec![vec![
        ResponseEvent::AgentMessageDelta { content: "Running command...".into() },
        ResponseEvent::ToolCallBegin { tool_call: ToolCall {
            id: "c1".into(), name: "bash".into(), arguments: serde_json::json!({"cmd":"echo hi"}) } },
    ]];
    let mut reg = DefaultToolRegistry::new();
    reg.register(Arc::new(MockTool::new("bash", vec!["hi"]).with_capability(CapabilityLevel::Exec))).unwrap();
    let mut session = Session::new(make_config("sec-auto", responses, reg)).await;
    let events = session.run_turn(TurnInput {
        thread_id: ThreadId::from("t1"),
        messages: vec![make_user_msg("Run command.")],
    }).await;
    let tool_results: Vec<&ToolResultMessage> = events.iter().filter_map(|e| {
        if let ResponseEvent::ToolCallEnd { ref result, .. } = e { Some(result) } else { None }
    }).collect();
    assert!(!tool_results.is_empty());
    assert!(tool_results.iter().all(|r| r.is_success()));
}

#[tokio::test]
async fn e2e_security_no_data_leakage() {
    let mut session_a = Session::new(make_config("sec-sess-a", vec![
        vec![ResponseEvent::AgentMessageDelta { content: "Secret: my-api-key-12345".into() }],
    ], DefaultToolRegistry::new())).await;
    let mut session_b = Session::new(make_config("sec-sess-b", vec![
        vec![ResponseEvent::AgentMessageDelta { content: "Session B response.".into() }],
    ], DefaultToolRegistry::new())).await;

    let events_a = session_a.run_turn(TurnInput {
        thread_id: ThreadId::from("ta"),
        messages: vec![make_user_msg("Tell me a secret.")],
    }).await;
    let events_b = session_b.run_turn(TurnInput {
        thread_id: ThreadId::from("tb"),
        messages: vec![make_user_msg("Your response?")],
    }).await;

    let msg_a = events_a.iter().find_map(|e| {
        if let ResponseEvent::TurnComplete { ref final_message, .. } = e {
            if let Message::AssistantMessage { content } = final_message { Some(content.as_str()) } else { None }
        } else { None }
    });
    assert_eq!(msg_a, Some("Secret: my-api-key-12345"));

    let msg_b = events_b.iter().find_map(|e| {
        if let ResponseEvent::TurnComplete { ref final_message, .. } = e {
            if let Message::AssistantMessage { content } = final_message { Some(content.as_str()) } else { None }
        } else { None }
    });
    assert_eq!(msg_b, Some("Session B response."));
    assert!(!msg_b.unwrap_or("").contains("my-api-key"));
}

#[tokio::test]
async fn e2e_security_capability_enforcement() {
    let auto = PermissionEnforcer::new(PermissionMode::Auto);
    let block = PermissionEnforcer::new(PermissionMode::Block);
    let permit = PermissionEnforcer::new(PermissionMode::Permit);

    assert!(auto.check("r", CapabilityLevel::Read).is_ok());
    assert!(auto.check("e", CapabilityLevel::Edit).is_ok());
    assert!(auto.check("x", CapabilityLevel::Exec).is_ok());

    assert!(block.check("r", CapabilityLevel::Read).is_ok());
    assert!(block.check("e", CapabilityLevel::Edit).is_err());
    assert!(block.check("x", CapabilityLevel::Exec).is_err());

    assert!(permit.check("r", CapabilityLevel::Read).is_ok());
    assert!(permit.check("e", CapabilityLevel::Edit).is_err());
    assert!(permit.check("x", CapabilityLevel::Exec).is_err());
}

#[tokio::test]
async fn e2e_security_router_enforces_permissions() {
    let mut reg = DefaultToolRegistry::new();
    reg.register(Arc::new(MockTool::new("danger", vec!["ok"]).with_capability(CapabilityLevel::Exec))).unwrap();
    let router = ToolRouter::new(Arc::new(reg), Arc::new(PermissionEnforcer::new(PermissionMode::Block)));
    let result = router.route(&ToolCall {
        id: "c1".into(), name: "danger".into(), arguments: serde_json::json!({}),
    }).await;
    assert!(result.is_error());
    assert!(result.error.as_deref().unwrap_or("").contains("permission denied"));
}

#[tokio::test]
async fn e2e_security_permission_mode_switching() {
    let mut e = PermissionEnforcer::new(PermissionMode::Auto);
    assert!(e.check("x", CapabilityLevel::Exec).is_ok());
    e.set_mode(PermissionMode::Block);
    assert!(e.check("x", CapabilityLevel::Exec).is_err());
    e.set_mode(PermissionMode::Auto);
    assert!(e.check("x", CapabilityLevel::Exec).is_ok());
    e.set_mode(PermissionMode::Permit);
    assert!(e.check("r", CapabilityLevel::Read).is_ok());
    assert!(e.check("x", CapabilityLevel::Exec).is_err());
}
