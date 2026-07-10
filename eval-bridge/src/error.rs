use std::fmt;

/// Errors that can occur when calling into the Python eval module.
#[derive(Debug)]
pub enum BridgeError {
    /// Failed to import the `code_agent_eval` Python package.
    Import(String),
    /// Python raised an exception during execution.
    Python(String),
    /// The returned JSON could not be parsed / extracted.
    Serialization(String),
}

impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Import(msg) => write!(f, "import error: {msg}"),
            Self::Python(msg) => write!(f, "python error: {msg}"),
            Self::Serialization(msg) => write!(f, "serialization error: {msg}"),
        }
    }
}

impl std::error::Error for BridgeError {}

// Map pyo3 errors into our domain type.
impl From<pyo3::PyErr> for BridgeError {
    fn from(e: pyo3::PyErr) -> Self {
        Self::Python(e.to_string())
    }
}
