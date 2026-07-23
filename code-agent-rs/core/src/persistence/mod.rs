//! Session persistence and resume capability.
//!
//! This module provides SQLite-backed storage for agent sessions, enabling
//! crash recovery, session resume, and long-term archival.
//!
//! # Architecture
//!
//! ```text
//! Session     SessionSnapshot (serializable)     SessionStore (SQLite)
//!                                                       
//!       run_turn()     TurnSnapshot                        save_turn()
//!                                                       
//!       new(config)     SessionSnapshot                                            resume()
//! ```
//!
//! # Tables
//!
//! | Table       | Purpose                                    |
//! |-------------|--------------------------------------------|
//! | `sessions`  | One row per session (metadata)             |
//! | `turns`     | One row per turn within a session          |
//! | `tool_calls`| One row per tool invocation within a turn  |
//! | `events`    | All response events emitted during a turn  |
//!
//! # Usage
//!
//! ```rust,ignore
//! use code_agent_core::persistence::{SessionStore, SessionSnapshot};
//!
//! // Open/create the database
//! let store = SessionStore::open("sessions.db")?;
//!
//! // Save a session
//! let snap = SessionSnapshot::new(id, "You are a coder.".into(), 20, mode);
//! store.save_session(&snap)?;
//!
//! // After each turn
//! store.save_turn(&session_id, &turn_snapshot, true)?;
//!
//! // Resume later
//! if let Some(snap) = store.resume(&session_id)? {
//!     let session = Session::from_snapshot(snap, model_client, tool_registry).await;
//! }
//!
//! // Auto-cleanup old sessions
//! store.archive_old_sessions(30)?;
//! ```


//!                                                                     
//! 
//!                                         SQLite                            
//!                                                           sessions   turns   tool_calls   events             
pub mod schema;
pub mod session_store;
pub mod snapshot;

pub use session_store::SessionStore;
pub use snapshot::{SessionSnapshot, ToolCallSnapshot, TurnSnapshot};
