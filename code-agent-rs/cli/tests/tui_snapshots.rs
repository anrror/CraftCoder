//! Insta snapshot tests for TUI components.
//!
//! These tests verify that UI components render correctly by capturing their
//! output state (text, colors, layout) as insta snapshots.

use code_agent_cli::tui::chat::{ChatMessage, ChatPane, ChatRole};
use code_agent_cli::tui::input::{InputAction, InputBar};
use code_agent_cli::tui::status::{ConnectionStatus, InputMode, StatusBar};

// ---------------------------------------------------------------------------
// ChatPane snapshot tests
// ---------------------------------------------------------------------------

#[test]
fn chat_pane_empty_renders_correctly() {
    let pane = ChatPane::new();
    assert_eq!(pane.all_text(), "");
}

#[test]
fn chat_pane_with_user_message() {
    let mut pane = ChatPane::new();
    pane.push_message(ChatMessage {
        role: ChatRole::User,
        content: "Hello, can you help me fix a bug?".into(),
        is_streaming: false,
    });
    insta::assert_snapshot!(pane.all_text(), @"[You] Hello, can you help me fix a bug?");
}

#[test]
fn chat_pane_with_agent_response() {
    let mut pane = ChatPane::new();
    pane.push_message(ChatMessage {
        role: ChatRole::Agent,
        content: "I found the issue. The null check was missing on line 42.".into(),
        is_streaming: false,
    });
    insta::assert_snapshot!(
        pane.all_text(),
        @"[Agent] I found the issue. The null check was missing on line 42."
    );
}

#[test]
fn chat_pane_mixed_conversation() {
    let mut pane = ChatPane::new();
    pane.push_message(ChatMessage {
        role: ChatRole::User,
        content: "What is 2+2?".into(),
        is_streaming: false,
    });
    pane.push_message(ChatMessage {
        role: ChatRole::Agent,
        content: "2+2 = 4".into(),
        is_streaming: false,
    });
    pane.push_message(ChatMessage {
        role: ChatRole::User,
        content: "Thanks!".into(),
        is_streaming: false,
    });
    insta::assert_snapshot!(
        pane.all_text(),
        @"[You] What is 2+2?\n[Agent] 2+2 = 4\n[You] Thanks!"
    );
}

#[test]
fn chat_pane_with_tool_call() {
    let mut pane = ChatPane::new();
    pane.push_tool_call("read_file", r#"{"path":"src/main.rs"}"#);
    insta::assert_snapshot!(
        pane.all_text(),
        @"[Tool →] read_file {\"path\":\"src/main.rs\"}"
    );
}

#[test]
fn chat_pane_with_error() {
    let mut pane = ChatPane::new();
    pane.push_tool_result("permission denied: cannot write to /etc/hosts", true);
    insta::assert_snapshot!(
        pane.all_text(),
        @"[ERROR] permission denied: cannot write to /etc/hosts"
    );
}

#[test]
fn chat_pane_streaming_accumulates() {
    let mut pane = ChatPane::new();
    pane.push_streaming("I ");
    pane.push_streaming("think ");
    pane.push_streaming("therefore ");
    pane.push_streaming("I am.");
    insta::assert_snapshot!(
        pane.all_text(),
        @"[Agent] I think therefore I am."
    );
}

#[test]
fn chat_pane_scroll_behavior() {
    let mut pane = ChatPane::new();
    // Auto-scroll on by default
    assert!(pane.is_auto_scrolling());

    // Add messages and scroll
    for i in 0..10 {
        pane.push_message(ChatMessage {
            role: ChatRole::User,
            content: format!("message {}", i),
            is_streaming: false,
        });
    }
    assert_eq!(pane.message_count(), 10);

    // Scroll up should disable auto-scroll
    pane.scroll_up(5);
    assert!(!pane.is_auto_scrolling());

    // Scroll to bottom re-enables
    pane.scroll_to_bottom();
    assert!(pane.is_auto_scrolling());
}

#[test]
fn chat_pane_search_finds_text() {
    let mut pane = ChatPane::new();
    pane.push_message(ChatMessage {
        role: ChatRole::User,
        content: "Find the null pointer exception".into(),
        is_streaming: false,
    });
    pane.push_message(ChatMessage {
        role: ChatRole::Agent,
        content: "The null check is at line 42.".into(),
        is_streaming: false,
    });

    let matches = pane.search("null".into());
    assert_eq!(matches, 2);
}

// ---------------------------------------------------------------------------
// InputBar snapshot tests
// ---------------------------------------------------------------------------

#[test]
fn input_bar_starts_empty() {
    let bar = InputBar::new();
    assert!(bar.is_empty());
    assert_eq!(bar.text(), "");
    assert_eq!(bar.history_len(), 0);
}

#[test]
fn input_bar_typing_text() {
    let mut bar = InputBar::new();
    bar.insert_char('h');
    bar.insert_char('e');
    bar.insert_char('l');
    bar.insert_char('l');
    bar.insert_char('o');
    insta::assert_snapshot!(bar.text(), @"hello");
}

#[test]
fn input_bar_command_detection() {
    let mut bar = InputBar::new();
    bar.insert_char('/');
    bar.insert_char('e');
    bar.insert_char('d');
    bar.insert_char('i');
    bar.insert_char('t');
    assert!(bar.is_command());

    let mut bar2 = InputBar::new();
    bar2.insert_char('/');
    bar2.insert_char('s');
    bar2.insert_char('e');
    bar2.insert_char('a');
    bar2.insert_char('r');
    bar2.insert_char('c');
    bar2.insert_char('h');
    assert!(bar2.is_command());
}

#[test]
fn input_bar_not_command_without_slash() {
    let mut bar = InputBar::new();
    bar.insert_char('h');
    bar.insert_char('e');
    bar.insert_char('l');
    bar.insert_char('l');
    bar.insert_char('o');
    assert!(!bar.is_command());
}

#[test]
fn input_bar_submit_saves_to_history() {
    let mut bar = InputBar::new();
    bar.insert_char('t');
    bar.insert_char('e');
    bar.insert_char('s');
    bar.insert_char('t');

    let action = bar.submit();
    assert_eq!(action, InputAction::Submit("test".into()));
    assert_eq!(bar.history_len(), 1);
    assert!(bar.is_empty());
}

#[test]
fn input_bar_history_navigation() {
    let mut bar = InputBar::new();

    // Submit two messages
    bar.insert_char('f');
    bar.insert_char('i');
    bar.insert_char('r');
    bar.insert_char('s');
    bar.insert_char('t');
    bar.submit();

    bar.insert_char('s');
    bar.insert_char('e');
    bar.insert_char('c');
    bar.insert_char('o');
    bar.insert_char('n');
    bar.insert_char('d');
    bar.submit();

    // Navigate history
    bar.history_prev();
    assert_eq!(bar.text(), "second");

    bar.history_prev();
    assert_eq!(bar.text(), "first");

    bar.history_next();
    assert_eq!(bar.text(), "second");
}

#[test]
fn input_bar_interrupt_empty() {
    let mut bar = InputBar::new();
    let action = bar.handle_interrupt();
    assert_eq!(action, InputAction::Interrupt);
}

#[test]
fn input_bar_interrupt_with_text_clears() {
    let mut bar = InputBar::new();
    bar.insert_char('x');
    let action = bar.handle_interrupt();
    assert_eq!(action, InputAction::Clear);
    assert!(bar.is_empty());
}

#[test]
fn input_bar_cursor_movement() {
    let mut bar = InputBar::new();
    bar.insert_char('a');
    bar.insert_char('b');
    bar.insert_char('c');

    assert_eq!(bar.text(), "abc");
    bar.cursor_left();
    bar.cursor_left();
    bar.insert_char('X');
    // cursor was at position 1 (after 'a'), so X is inserted between a and b
    insta::assert_snapshot!(bar.text(), @"aXbc");
}

// ---------------------------------------------------------------------------
// StatusBar snapshot tests
// ---------------------------------------------------------------------------

#[test]
fn status_bar_defaults() {
    let bar = StatusBar::new();
    assert_eq!(bar.connection, ConnectionStatus::Disconnected);
    assert_eq!(bar.mode, InputMode::Insert);
}

#[test]
fn status_bar_active_session() {
    let mut bar = StatusBar::new();
    bar.connection = ConnectionStatus::Connected;
    bar.session_id = "sess-abc-123".into();
    bar.model_name = "test-model".into();
    bar.prompt_tokens = 1500;
    bar.completion_tokens = 300;
    bar.turn_count = 5;
    bar.mode = InputMode::Insert;

    assert_eq!(bar.connection, ConnectionStatus::Connected);
    assert_eq!(bar.session_id, "sess-abc-123");
    assert_eq!(bar.model_name, "test-model");
    assert_eq!(bar.prompt_tokens, 1500);
    assert_eq!(bar.completion_tokens, 300);
    assert_eq!(bar.turn_count, 5);
    assert_eq!(bar.mode, InputMode::Insert);
}

#[test]
fn status_bar_token_accumulation() {
    let mut bar = StatusBar::new();
    bar.update_tokens(200, 100);
    bar.update_tokens(300, 150);
    assert_eq!(bar.prompt_tokens, 500);
    assert_eq!(bar.completion_tokens, 250);
}

#[test]
fn status_bar_turn_tracking() {
    let mut bar = StatusBar::new();
    for _ in 0..10 {
        bar.increment_turn();
    }
    assert_eq!(bar.turn_count, 10);
}

// ---------------------------------------------------------------------------
// Color specification verification
// ---------------------------------------------------------------------------

#[test]
fn color_codes_match_spec() {
    // User = Cyan, Agent = Green, ToolCall = Yellow, Error = Red
    use ratatui::style::Color;

    assert_eq!(ChatRole::User.color(), Color::Cyan);
    assert_eq!(ChatRole::Agent.color(), Color::Green);
    assert_eq!(ChatRole::ToolCall.color(), Color::Yellow);
    assert_eq!(ChatRole::Error.color(), Color::Red);
    assert_eq!(ChatRole::ToolResult.color(), Color::Gray);
    assert_eq!(ChatRole::System.color(), Color::Magenta);
}

#[test]
fn chat_pane_color_label_mapping() {
    assert_eq!(ChatRole::User.label(), "You");
    assert_eq!(ChatRole::Agent.label(), "Agent");
    assert_eq!(ChatRole::ToolCall.label(), "Tool →");
    assert_eq!(ChatRole::ToolResult.label(), "Tool ←");
    assert_eq!(ChatRole::Error.label(), "ERROR");
    assert_eq!(ChatRole::System.label(), "System");
}
