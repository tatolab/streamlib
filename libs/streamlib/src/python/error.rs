
use pyo3::prelude::*;
use pyo3::exceptions::PyException;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, PyStreamError>;

#[derive(Error, Debug)]
pub enum PyStreamError {
    #[error("StreamLib error: {0}")]
    Core(#[from] crate::core::StreamError),

    #[error("Python error: {0}")]
    Python(String),

    #[error("Configuration error: {0}")]
    Configuration(String),

    #[error("Processor not found: {0}")]
    ProcessorNotFound(String),

    #[error("Port not found: {0}")]
    PortNotFound(String),

    #[error("Connection error: {0}")]
    Connection(String),

    #[error("Runtime error: {0}")]
    Runtime(String),
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
