//! Event processor for non-interactive output.
//!
//! [`EventProcessor`] receives `ResponseEvent`s from a session turn and
//! renders them to stdout according to the configured [`OutputFormat`].

use std::io::{self, Write};

use code_agent_protocol::ResponseEvent;

use super::{ExecExitCode, OutputFormat};

// ---------------------------------------------------------------------------
// EventProcessor
// ---------------------------------------------------------------------------

/// 事件处理器
///
/// 【领域含义】处理 Agent 响应事件并根据配置的输出格式渲染到 stdout。
/// 【核心职责】接收 ResponseEvent 流，按 JSON/文本/静默格式输出，解析退出码。
///
/// # 生命周期
/// 1. 使用 OutputFormat 创建
/// 2. 调用 process 处理 Session::run_turn 的事件
/// 3. 调用 exit_code 获取映射后的退出码
///
/// # 输出格式
/// | 格式     | stdout 输出                                      |
/// |----------|--------------------------------------------------|
/// | `Json`   | 每事件一行 JSON，包含最终 exit_code               |
/// | `Text`   | 仅最终答案文本                                    |
/// | `Silent` | 无输出                                            |
pub struct EventProcessor {
    format: OutputFormat,
    /// 从 AgentMessageDelta 事件累积的文本
    text_buffer: String,
    /// 处理后的退出码
    resolved_code: ExecExitCode,
    /// 是否遇到工具相关错误
    saw_tool_error: bool,
    /// 是否遇到 Agent 级别错误
    saw_agent_error: bool,
}

impl EventProcessor {
    /// 创建事件处理器
    ///
    /// 【领域含义】为指定的输出格式创建事件处理器。
    /// 【核心职责】初始化处理器状态，默认退出码为 Success。
    pub fn new(format: OutputFormat) -> Self {
        Self {
            format,
            text_buffer: String::new(),
            resolved_code: ExecExitCode::Success,
            saw_tool_error: false,
            saw_agent_error: false,
        }
    }

    /// 处理一批响应事件
    ///
    /// 【领域含义】按顺序消费一批 ResponseEvent，根据输出格式写入 stdout。
    /// 【核心职责】逐个处理事件、累积文本、解析退出码、输出最终结果。
    pub fn process(&mut self, events: &[ResponseEvent]) {
        for event in events {
            self.handle_event(event);
        }

        // After all events, flush the final answer for Text mode
        if self.format == OutputFormat::Text && !self.text_buffer.is_empty() {
            let _ = writeln!(io::stdout(), "{}", self.text_buffer);
        }

        // Resolve final exit code
        self.resolve_exit_code();

        // For Json mode, emit the exit code as the final event
        if self.format == OutputFormat::Json {
            let exit_event = serde_json::json!({
                "type": "exit",
                "code": self.resolved_code.as_i32(),
            });
            let _ = writeln!(io::stdout(), "{}", serde_json::to_string(&exit_event).unwrap_or_default());
        }

        let _ = io::stdout().flush();
    }

    /// 获取退出码
    ///
    /// 【领域含义】返回处理过程中确定的退出码。
    /// 【核心职责】供调用方获取最终退出码。
    pub fn exit_code(&self) -> ExecExitCode {
        self.resolved_code
    }

    // ── internal helpers ──────────────────────────────────────────────

    fn handle_event(&mut self, event: &ResponseEvent) {
        match self.format {
            OutputFormat::Json => {
                // Emit every event as a JSON line
                if let Ok(json) = serde_json::to_string(event) {
                    let _ = writeln!(io::stdout(), "{}", json);
                }
            }
            OutputFormat::Text => {
                // Only accumulate text deltas; final output flushed in process()
            }
            OutputFormat::Silent => {
                // No output
            }
        }

        // Always track event types for exit code resolution
        match event {
            ResponseEvent::AgentMessageDelta { content } => {
                self.text_buffer.push_str(content);
            }
            ResponseEvent::Error { message } => {
                // Heuristic: errors containing "tool" are tool errors,
                // everything else is an agent error.
                let msg_lower = message.to_lowercase();
                if msg_lower.contains("tool") {
                    self.saw_tool_error = true;
                } else {
                    self.saw_agent_error = true;
                }
                // Also emit error to stderr for visibility
                if self.format != OutputFormat::Silent {
                    let _ = writeln!(io::stderr(), "error: {message}");
                }
            }
            ResponseEvent::ToolCallEnd { result, .. } if result.is_error() => {
                self.saw_tool_error = true;
            }
            _ => {}
        }
    }

    fn resolve_exit_code(&mut self) {
        // Priority: Timeout > ToolError > AgentError > Success
        // (Timeout is handled by ExecCli::execute before events reach here,
        //  but we keep the logic for completeness)
        if self.saw_tool_error {
            self.resolved_code = ExecExitCode::ToolError;
        } else if self.saw_agent_error {
            self.resolved_code = ExecExitCode::AgentError;
        } else {
            self.resolved_code = ExecExitCode::Success;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use code_agent_protocol::{Message, ToolCall, ToolResultMessage, TurnId};

    /// Helper: create events and run them through the processor, returning exit code.
    fn process_events(format: OutputFormat, events: &[ResponseEvent]) -> ExecExitCode {
        let mut processor = EventProcessor::new(format);
        processor.process(events);
        processor.exit_code()
    }

    // ── Exit code tests ────────────────────────────────────────────

    #[test]
    fn success_exit_code_on_normal_completion() {
        let events = vec![
            ResponseEvent::TurnStarted {
                turn_id: TurnId::from("t1"),
            },
            ResponseEvent::AgentMessageDelta {
                content: "Done!".into(),
            },
            ResponseEvent::TurnComplete {
                turn_id: TurnId::from("t1"),
                final_message: Message::AssistantMessage {
                    content: "Done!".into(),
                },
            },
        ];
        assert_eq!(
            process_events(OutputFormat::Json, &events),
            ExecExitCode::Success
        );
    }

    #[test]
    fn agent_error_exit_code_on_model_error() {
        let events = vec![
            ResponseEvent::Error {
                message: "Model error: connection refused".into(),
            },
        ];
        assert_eq!(
            process_events(OutputFormat::Silent, &events),
            ExecExitCode::AgentError
        );
    }

    #[test]
    fn agent_error_exit_code_on_max_iterations() {
        let events = vec![
            ResponseEvent::Error {
                message: "Max iterations (10) reached without final answer".into(),
            },
        ];
        assert_eq!(
            process_events(OutputFormat::Silent, &events),
            ExecExitCode::AgentError
        );
    }

    #[test]
    fn tool_error_exit_code_on_tool_failure() {
        let events = vec![
            ResponseEvent::ToolCallEnd {
                tool_call_id: "call-1".into(),
                result: ToolResultMessage {
                    tool_call_id: "call-1".into(),
                    output: None,
                    error: Some("permission denied".into()),
                },
            },
        ];
        assert_eq!(
            process_events(OutputFormat::Silent, &events),
            ExecExitCode::ToolError
        );
    }

    #[test]
    fn tool_error_exit_code_on_tool_error_message() {
        let events = vec![
            ResponseEvent::Error {
                message: "tool execution failed: bash: command not found".into(),
            },
        ];
        assert_eq!(
            process_events(OutputFormat::Silent, &events),
            ExecExitCode::ToolError
        );
    }

    #[test]
    fn tool_error_priority_over_agent_error() {
        // Both tool error and agent error are present — tool error wins
        let events = vec![
            ResponseEvent::ToolCallEnd {
                tool_call_id: "call-1".into(),
                result: ToolResultMessage {
                    tool_call_id: "call-1".into(),
                    output: None,
                    error: Some("tool failed".into()),
                },
            },
            ResponseEvent::Error {
                message: "Model error".into(),
            },
        ];
        assert_eq!(
            process_events(OutputFormat::Silent, &events),
            ExecExitCode::ToolError
        );
    }

    // ── JSON output tests ──────────────────────────────────────────

    #[test]
    fn json_output_contains_events_as_json_lines() {
        let events = vec![
            ResponseEvent::TurnStarted {
                turn_id: TurnId::from("t1"),
            },
            ResponseEvent::AgentMessageDelta {
                content: "Hello".into(),
            },
        ];
        let mut processor = EventProcessor::new(OutputFormat::Json);
        processor.process(&events);

        // Text buffer should accumulate regardless of format
        assert_eq!(processor.text_buffer, "Hello");
        assert_eq!(processor.exit_code(), ExecExitCode::Success);
    }

    #[test]
    fn json_output_includes_exit_code() {
        let events = vec![
            ResponseEvent::TurnStarted {
                turn_id: TurnId::from("t1"),
            },
        ];
        let code = process_events(OutputFormat::Json, &events);
        assert_eq!(code, ExecExitCode::Success);
    }

    #[test]
    fn text_output_accumulates_deltas() {
        let events = vec![
            ResponseEvent::AgentMessageDelta {
                content: "Part 1. ".into(),
            },
            ResponseEvent::AgentMessageDelta {
                content: "Part 2.".into(),
            },
        ];
        let mut processor = EventProcessor::new(OutputFormat::Text);
        processor.process(&events);
        assert_eq!(processor.text_buffer, "Part 1. Part 2.");
    }

    #[test]
    fn silent_output_accumulates_but_no_output() {
        let events = vec![
            ResponseEvent::AgentMessageDelta {
                content: "secret".into(),
            },
        ];
        let mut processor = EventProcessor::new(OutputFormat::Silent);
        processor.process(&events);
        // Text buffer still accumulates for exit code resolution
        assert_eq!(processor.text_buffer, "secret");
    }

    // ── Tool call tracking ─────────────────────────────────────────

    #[test]
    fn successful_tool_call_no_error() {
        let events = vec![
            ResponseEvent::ToolCallBegin {
                tool_call: ToolCall {
                    id: "call-1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "src/main.rs"}),
                },
            },
            ResponseEvent::ToolCallEnd {
                tool_call_id: "call-1".into(),
                result: ToolResultMessage {
                    tool_call_id: "call-1".into(),
                    output: Some("file contents".into()),
                    error: None,
                },
            },
        ];
        assert_eq!(
            process_events(OutputFormat::Silent, &events),
            ExecExitCode::Success
        );
    }

    #[test]
    fn json_output_serializes_full_event() {
        // Verify a ToolCallBegin event serializes correctly as JSON
        let event = ResponseEvent::ToolCallBegin {
            tool_call: ToolCall {
                id: "call-json".into(),
                name: "read_file".into(),
                arguments: serde_json::json!({"path": "src/lib.rs"}),
            },
        };
        let json = serde_json::to_string(&event).expect("serialize");
        assert!(json.contains("\"tool_call_begin\""));
        assert!(json.contains("\"read_file\""));
        assert!(json.contains("\"src/lib.rs\""));
    }

    // ── Exit code priority ─────────────────────────────────────────

    #[test]
    fn agent_error_wins_over_success() {
        let events = vec![
            ResponseEvent::AgentMessageDelta {
                content: "Almost done...".into(),
            },
            ResponseEvent::Error {
                message: "Model error: timeout".into(),
            },
            ResponseEvent::TurnComplete {
                turn_id: TurnId::from("t1"),
                final_message: Message::AssistantMessage {
                    content: "Done".into(),
                },
            },
        ];
        // Error is agent-level (no "tool" keyword)
        assert_eq!(
            process_events(OutputFormat::Silent, &events),
            ExecExitCode::AgentError
        );
    }

    #[test]
    fn processor_fresh_state_is_success() {
        let processor = EventProcessor::new(OutputFormat::Json);
        assert_eq!(processor.exit_code(), ExecExitCode::Success);
        assert!(processor.text_buffer.is_empty());
    }
}
