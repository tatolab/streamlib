//! Error types for streamlib-python

use pyo3::prelude::*;
use pyo3::exceptions::PyException;
use thiserror::Error;

/// Result type for streamlib-python operations
pub type Result<T> = std::result::Result<T, PyStreamError>;

/// Python-facing error type
#[derive(Error, Debug)]
pub enum PyStreamError {
    /// Error from streamlib-core
    #[error("StreamLib error: {0}")]
    Core(#[from] crate::core::StreamError),

    /// Python runtime error
    #[error("Python error: {0}")]
    Python(String),

    /// Configuration error
    #[error("Configuration error: {0}")]
    Configuration(String),

    /// Processor not found
    #[error("Processor not found: {0}")]
    ProcessorNotFound(String),

    /// Port not found
    #[error("Port not found: {0}")]
    PortNotFound(String),

    /// Connection error
    #[error("Connection error: {0}")]
    Connection(String),
}

impl From<PyStreamError> for PyErr {
    fn from(err: PyStreamError) -> PyErr {
        PyException::new_err(err.to_string())
    }
}

impl From<PyErr> for PyStreamError {
    fn from(err: PyErr) -> PyStreamError {
        PyStreamError::Python(err.to_string())
    }
}
