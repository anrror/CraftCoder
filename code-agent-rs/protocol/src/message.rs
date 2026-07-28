//! 消息层 (Message Layer) — 对话领域事件
//!
//! 本模块定义了 Agent 对话系统中消息的完整类型体系：
//! [`Message`] 枚举作为顶层消息抽象，[`ToolCall`] 和 [`ToolResultMessage`]
//! 分别描述工具调用的请求与响应。
//!
//! 【DDD 分层】消息层属于领域事件层。Message 是对话的基本原子单元，
//! 在 Agent 循环中产生，经 MCP/SSE 事件流传输，最终持久化于 SessionStore。
//!
//! 【事件流关系】
//! ```text
//! UserMessage → AssistantMessage → ToolCall → ToolResultMessage → AssistantMessage → ...
//! ```
//! 消息在 Turn 内按时间序排列，构成完整的 Agent 推理链。

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Message — 消息枚举
// ---------------------------------------------------------------------------

/// 对话消息 (Conversation Message Domain Event)
///
/// 【领域含义】Agent 对话系统中的核心事件载体。
/// Message 覆盖了用户、AI、工具三者之间的所有交互形式，
/// 是 Agent 循环中传递和处理的最小信息单元。
///
/// 【使用场景】
/// - 在 Agent 主循环中作为输入/输出数据流
/// - 在持久化层按时间序存储对话历史
/// - 在上下文窗口中组装 LLM API 请求
/// - 在 UI 层渲染不同角色的消息气泡
///
/// 【变体说明】
/// - `UserMessage`: 用户在 TUI/Web UI 中输入的自然语言指令
/// - `AssistantMessage`: Agent 的文本响应（推理结果、解释说明）
/// - `ToolCall`: Agent 请求执行工具（如读文件、搜索代码）
/// - `ToolResult`: 工具执行完成后返回给 Agent 的结果
///
/// 【序列化格式】`#[serde(rename_all = "snake_case")]`
/// 确保 JSON 输出为 `{"user_message": {"content": "..."}}` 格式。
///
/// 【与其他类型的关系】
/// - Message 的 `ToolCall` 变体包装了 [`ToolCall`] 结构体
/// - Message 的 `ToolResult` 变体包装了 [`ToolResultMessage`] 结构体
/// - 被 [`crate::execution::TurnInput`] 的 `messages` 字段收集
/// - 被 [`crate::execution::ResponseEvent::TurnComplete`] 的 `final_message` 字段引用
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Message {
    /// 用户消息 — 来自人类用户的自然语言输入
    ///
    /// 【领域含义】这是 Agent 对话的起点，承载用户意图。
    /// 内容可以是问题、指令、代码片段或任意自然语言。
    UserMessage {
        /// 用户消息的文本内容
        content: String,
    },
    /// 助手消息 — AI Agent 的文本响应
    ///
    /// 【领域含义】Agent 的推理结果、解释说明或问题回答。
    /// 在流式输出场景中，多条 AgentMessageDelta 事件聚合为一条 AssistantMessage。
    AssistantMessage {
        /// 助手响应的文本内容（完整文本，非流式增量）
        content: String,
    },
    /// 工具调用 — Agent 请求执行外部工具
    ///
    /// 【领域含义】当 Agent 判断需要访问文件系统、执行命令或搜索代码时，
    /// 发出 ToolCall 消息。ToolCall 会被 Permission System 检查权限后执行。
    ToolCall(ToolCall),
    /// 工具执行结果 — 工具执行完成后返回给 Agent
    ///
    /// 【领域含义】将工具执行的输出或错误信息反馈给 Agent，
    /// Agent 据此决定下一步行动（继续调用工具或给出最终回答）。
    ToolResult(ToolResultMessage),
}

// ---------------------------------------------------------------------------
// ToolCall — 工具调用请求
// ---------------------------------------------------------------------------

/// 工具调用请求 (Tool Invocation Request)
///
/// 【领域含义】Agent 请求执行某个具名工具的完整信息包。
/// 包含工具的标识（id）、名称（name）和参数（arguments）。
///
/// 【使用场景】
/// - Agent 生成工具调用后，通过此结构体传递给 ToolRouter
/// - ToolRouter 根据 name 字段路由到对应的工具处理函数
/// - Permission System 根据 name + arguments 决定是否允许执行
/// - 执行结果通过 [`ToolResultMessage`] 返回，用 id 关联
///
/// 【字段说明】
/// - `id`: 此次工具调用的唯一标识，与 [`ToolResultMessage::tool_call_id`] 对应
/// - `name`: 工具名称，如 `"read_file"`、`"bash"`、`"grep"`
/// - `arguments`: 工具参数，以任意 JSON 对象表示（不同工具有不同 schema）
///
/// 【约束】
/// - `id` 在单个 Turn 内必须唯一
/// - `arguments` 必须是合法的 JSON 对象（不能是数组或原始值）
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    /// 此次工具调用的唯一标识符
    pub id: String,
    /// 被调用的工具名称（如 "read_file", "bash", "grep"）
    pub name: String,
    /// 传递给工具的 JSON 参数（具体 schema 由工具定义）
    pub arguments: serde_json::Value,
}

// ---------------------------------------------------------------------------
// ToolResultMessage — 工具执行结果
// ---------------------------------------------------------------------------

/// 工具执行结果 (Tool Execution Result Domain Event)
///
/// 【领域含义】工具执行完成后的结果载体。
/// 成功执行时 `output` 有值，失败时 `error` 有值。
/// 二者互斥：有且仅有一个字段为 `Some`（允许均为 None 表示空输出无错误）。
///
/// 【使用场景】
/// - 工具执行完成后，将结果封装为 ToolResultMessage 返回给 Agent
/// - Agent 根据 `is_success()` / `is_error()` 判断工具执行状态
/// - 在 UI 层展示工具执行结果给用户
///
/// 【序列化策略】
/// - `#[serde(skip_serializing_if = "Option::is_none")]`：
///   当 output/error 为 None 时，JSON 输出中不包含该字段（非 null）
/// - 反序列化时 `null` 等价于 `None`
///
/// 【约束】
/// - `tool_call_id` 必须对应一个已发出的 [`ToolCall::id`]
/// - 正常情况下 `output` 和 `error` 不同时为 `Some`
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResultMessage {
    /// 对应的 ToolCall ID，用于关联请求与响应
    pub tool_call_id: String,
    /// 工具执行成功时的输出内容（stdout / 返回值等）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// 工具执行失败时的错误信息
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ToolResultMessage {
    /// 判断工具是否执行成功（即没有错误信息）
    ///
    /// 【使用场景】在 Agent 循环中，根据返回值决定下一步：
    /// - `true` → 继续处理输出，可能进行下一轮推理
    /// - `false` → 处理错误，可能重试或报告给用户
    ///
    /// ```rust
    /// use code_agent_protocol::ToolResultMessage;
    /// let result = ToolResultMessage {
    ///     tool_call_id: "call-1".into(),
    ///     output: Some("done".into()),
    ///     error: None,
    /// };
    /// assert!(result.is_success());
    /// ```
    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }

    /// 判断工具是否执行失败（即存在错误信息）
    ///
    /// 【使用场景】用于错误处理分支：
    /// - `true` → 进入错误恢复流程（重试 / 降级 / 提示用户）
    /// - `false` → 正常处理流程
    ///
    /// ```rust
    /// use code_agent_protocol::ToolResultMessage;
    /// let result = ToolResultMessage {
    ///     tool_call_id: "call-1".into(),
    ///     output: None,
    ///     error: Some("permission denied".into()),
    /// };
    /// assert!(result.is_error());
    /// ```
    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_result_is_success_with_output() {
        let tr = ToolResultMessage {
            tool_call_id: "call-1".into(),
            output: Some("done".into()),
            error: None,
        };
        assert!(tr.is_success());
        assert!(!tr.is_error());
    }

    #[test]
    fn tool_result_is_error_with_error() {
        let tr = ToolResultMessage {
            tool_call_id: "call-1".into(),
            output: None,
            error: Some("fail".into()),
        };
        assert!(tr.is_error());
        assert!(!tr.is_success());
    }

    #[test]
    fn tool_result_both_none_is_success() {
        let tr = ToolResultMessage {
            tool_call_id: "call-1".into(),
            output: None,
            error: None,
        };
        // is_success returns true when error is None (even if output is also None)
        assert!(tr.is_success());
        assert!(!tr.is_error());
    }
}
