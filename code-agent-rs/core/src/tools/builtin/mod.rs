//! Built-in file and search tools.
//!
//! These tools provide the basic primitives for reading, writing, editing,
//! listing, and searching files on disk. They are registered by default in
//! every agent session.

pub mod edit_file;
pub mod glob;
pub mod grep;
pub mod list_dir;
pub mod read_file;
pub mod write_file;
