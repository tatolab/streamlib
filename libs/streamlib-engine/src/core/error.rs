// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("GPU operation failed: {0}")]
    GpuError(String),

    #[error("Shader compilation failed: {0}")]
    ShaderCompilation(String),

    #[error("Texture operation failed: {0}")]
    TextureError(String),

    #[error("Stream graph error: {0}")]
    GraphError(String),

    #[error("Port error: {0}")]
    PortError(String),

    #[error("Link error: {0}")]
    Link(String),

    #[error("Link already exists: {0}")]
    LinkAlreadyExists(String),

    #[error("Link not found: {0}")]
    LinkNotFound(String),

    #[error("Link not wired: {0}")]
    LinkNotWired(String),

    #[error("Link already disconnected: {0}")]
    LinkAlreadyDisconnected(String),

    #[error("Invalid link: {0}")]
    InvalidLink(String),

    #[error("Invalid port address: {0}")]
    InvalidPortAddress(String),

    #[error("Invalid graph: {0}")]
    InvalidGraph(String),

    #[error("Processor not found: {0}")]
    ProcessorNotFound(String),

    #[error("Unknown processor type: {ident} (not registered)")]
    UnknownProcessorType {
        ident: crate::core::descriptors::SchemaIdent,
    },

    #[error("Processor '{processor_id}' has no {direction} port named '{port_name}'")]
    ProcessorPortNotFound {
        processor_id: String,
        port_name: String,
        direction: PortDirection,
    },

    #[error("Buffer operation failed: {0}")]
    BufferError(String),

    #[error("Clock synchronization error: {0}")]
    ClockError(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Invalid configuration: {0}")]
    Configuration(String),

    #[error("Config update failed: {0}")]
    Config(String),

    #[error("Operation not supported: {0}")]
    NotSupported(String),

    #[error("Resource not found: {0}")]
    NotFound(String),

    #[error("Runtime error: {0}")]
    Runtime(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Direction of a port relative to its processor — `Output` for source-side,
/// `Input` for destination-side. Used by [`Error::ProcessorPortNotFound`] to
/// distinguish "the source processor has no output port named X" from "the
/// target processor has no input port named X."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDirection {
    Input,
    Output,
}

impl std::fmt::Display for PortDirection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Input => f.write_str("input"),
            Self::Output => f.write_str("output"),
        }
    }
}

#[cfg(target_os = "linux")]
impl From<streamlib_consumer_rhi::ConsumerRhiError> for Error {
    fn from(e: streamlib_consumer_rhi::ConsumerRhiError) -> Self {
        match e {
            streamlib_consumer_rhi::ConsumerRhiError::Gpu(s) => Error::GpuError(s),
            streamlib_consumer_rhi::ConsumerRhiError::Configuration(s) => {
                Error::Configuration(s)
            }
        }
    }
}
