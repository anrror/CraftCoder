//! Central tool registry.
//!
//! `ToolRegistry` trait is the abstraction for tool registration, lookup,
//! and listing. `DefaultToolRegistry` is the standard HashMap-backed
//! implementation.

use std::collections::HashMap;
use std::sync::Arc;

use super::{Tool, ToolDefinition, ToolError};

/// Abstract interface for a tool registry.
///
/// Implementations store tools keyed by name and provide lookup,
/// listing, and removal. This trait is object-safe so it can be
/// used behind `Arc<dyn ToolRegistry>` for dependency injection.
#[allow(clippy::module_name_repetitions)]
pub trait ToolRegistry: Send + Sync {
    /// Register a tool. Returns an error if a tool with the same name is
    /// already registered.
    fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), ToolError>;

    /// Look up a tool by name. Returns `None` if not found.
    fn get(&self, name: &str) -> Option<Arc<dyn Tool>>;

    /// Return `ToolDefinition` metadata for every registered tool.
    ///
    /// This is the list sent to the model context so it knows what tools are
    /// available and how to call them.
    fn list(&self) -> Vec<ToolDefinition>;

    /// Remove a tool from the registry by name. No-op if not found.
    fn unregister(&mut self, name: &str);

    /// Returns the number of registered tools.
    fn len(&self) -> usize;

    /// Returns `true` if no tools are registered.
    fn is_empty(&self) -> bool;
}

// ---------------------------------------------------------------------------
// DefaultToolRegistry — standard HashMap-backed implementation
// ---------------------------------------------------------------------------

/// A thread-safe registry of available tools backed by a `HashMap`.
///
/// Tools are stored keyed by name. Registration fails if a tool with the same
/// name already exists (use `unregister` first to replace).
///
/// # Example
///
/// ```rust
/// use code_agent_core::tools::registry::DefaultToolRegistry;
/// use code_agent_core::tools::registry::ToolRegistry;
/// use code_agent_core::tools::builtin::read_file::ReadFileTool;
/// use std::sync::Arc;
///
/// let mut registry = DefaultToolRegistry::new();
/// let tool = Arc::new(ReadFileTool::default());
/// registry.register(tool).unwrap();
/// assert!(registry.get("read_file").is_some());
/// ```
#[allow(clippy::module_name_repetitions)]
pub struct DefaultToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl Default for DefaultToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultToolRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }
}

impl ToolRegistry for DefaultToolRegistry {
    fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), ToolError> {
        let name = tool.name().to_owned();
        if self.tools.contains_key(&name) {
            return Err(ToolError::already_registered(&name));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    fn list(&self) -> Vec<ToolDefinition> {
        self.tools
            .values()
            .map(|tool| ToolDefinition {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                input_schema: tool.input_schema(),
            })
            .collect()
    }

    fn unregister(&mut self, name: &str) {
        self.tools.remove(name);
    }

    fn len(&self) -> usize {
        self.tools.len()
    }

    fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::builtin::{
        edit_file::EditFileTool, glob::GlobTool, grep::GrepTool, list_dir::ListDirTool,
        read_file::ReadFileTool, write_file::WriteFileTool,
    };

    fn all_builtins() -> Vec<Arc<dyn Tool>> {
        vec![
            Arc::new(ReadFileTool::default()),
            Arc::new(WriteFileTool::default()),
            Arc::new(EditFileTool::default()),
            Arc::new(ListDirTool::default()),
            Arc::new(GrepTool::default()),
            Arc::new(GlobTool::default()),
        ]
    }

    #[test]
    fn register_and_get() {
        let mut reg = DefaultToolRegistry::new();
        let tool = Arc::new(ReadFileTool::default());
        reg.register(tool).unwrap();
        assert!(reg.get("read_file").is_some());
        assert!(reg.get("nonexistent").is_none());
    }

    #[test]
    fn register_duplicate_fails() {
        let mut reg = DefaultToolRegistry::new();
        let t1 = Arc::new(ReadFileTool::default());
        let t2 = Arc::new(ReadFileTool::default());
        reg.register(t1).unwrap();
        let err = reg.register(t2).unwrap_err();
        assert!(matches!(err, ToolError::AlreadyRegistered(_)));
    }

    #[test]
    fn register_six_builtins_list_returns_six() {
        let mut reg = DefaultToolRegistry::new();
        for tool in all_builtins() {
            reg.register(tool).unwrap();
        }
        let defs = reg.list();
        assert_eq!(defs.len(), 6);
        assert_eq!(reg.len(), 6);
        assert!(!reg.is_empty());
    }

    #[test]
    fn list_definitions_have_correct_shape() {
        let mut reg = DefaultToolRegistry::new();
        reg.register(Arc::new(ReadFileTool::default())).unwrap();
        let defs = reg.list();
        assert_eq!(defs.len(), 1);
        let def = &defs[0];
        assert_eq!(def.name, "read_file");
        assert!(!def.description.is_empty());
        assert!(def.input_schema.is_object());
    }

    #[test]
    fn unregister_removes_tool() {
        let mut reg = DefaultToolRegistry::new();
        reg.register(Arc::new(ReadFileTool::default())).unwrap();
        assert_eq!(reg.len(), 1);
        reg.unregister("read_file");
        assert_eq!(reg.len(), 0);
        assert!(reg.get("read_file").is_none());
    }

    #[test]
    fn unregister_nonexistent_is_noop() {
        let mut reg = DefaultToolRegistry::new();
        reg.register(Arc::new(ReadFileTool::default())).unwrap();
        reg.unregister("nonexistent");
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn empty_registry() {
        let reg = DefaultToolRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.list().is_empty());
    }
}
