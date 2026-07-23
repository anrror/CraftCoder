//! Code Agent CLI — Interactive Terminal UI (TUI) + headless exec mode.
//!
//! Provides a full-featured terminal interface for the AI Coding Agent
//! using ratatui and crossterm, and a CI-friendly headless exec mode
//! for automation pipelines.

pub mod config;
pub mod diff;
pub mod exec;
pub mod tools;
pub mod tui;

pub use tui::App;
