// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Canonical StreamLib [`Error`] + [`Result`].
//!
//! Shared by `streamlib-engine` (which re-exports it at
//! `core::error`) and the plugin-SDK, so a plugin cdylib can author
//! against `Result` without linking the engine. Every variant is
//! String / std / anyhow based plus the engine-free `SchemaIdent`
//! (from `streamlib-processor-schema`) and, on Linux, the engine-free
//! `ConsumerRhiError` conversion.

use streamlib_processor_schema::{PackageRef, SchemaIdent};

/// The StreamLib error type.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("GPU operation failed: {0}")]
    GpuError(String),

    #[error("No display surface available: {0}")]
    DisplaySurfaceUnavailable(String),

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
    UnknownProcessorType { ident: SchemaIdent },

    #[error(
        "Processor type {processor_type} is provided by more than one package in \
         streamlib_modules/: {packages:?} — lazy discovery cannot pick one; remove \
         the duplicate package folder"
    )]
    AmbiguousProcessorTypeProviders {
        processor_type: SchemaIdent,
        packages: Vec<PackageRef>,
    },

    #[error(
        "Lazy load of package {package} providing processor type {processor_type} \
         failed: {detail}"
    )]
    LazyModuleLoadFailed {
        processor_type: SchemaIdent,
        package: PackageRef,
        detail: String,
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

    #[error("Invalid escalate scope: {0}")]
    InvalidEscalateScope(String),

    #[error("Escalate begin rejected: {0}")]
    EscalateBeginRejected(String),

    #[error(
        "Plugin ABI version mismatch loading '{plugin_path}': plugin was built \
         against plugin-ABI v{plugin_abi_version}, but this host speaks \
         v{host_abi_version}. Rebuild the plugin against the host's \
         streamlib-plugin-abi version — publish a matching engine `-dev` \
         version and bump the plugin's pin, or use `streamlib link`."
    )]
    PluginAbiVersionMismatch {
        plugin_path: String,
        plugin_abi_version: u32,
        host_abi_version: u32,
    },

    #[error(
        "Plugin build mismatch loading '{plugin_path}': the plugin's build \
         fingerprint does not match this host's. Plugin build: [{plugin_identity}] \
         (abi_layout={plugin_abi_fingerprint:#018x}, \
         engine_transit={plugin_transit_fingerprint:#018x}); host build: \
         [{host_identity}] (abi_layout={host_abi_fingerprint:#018x}, \
         engine_transit={host_transit_fingerprint:#018x}). Rebuild the plugin \
         against the host's engine build — publish a matching engine `-dev` \
         version and bump the plugin's pin, or use `streamlib link`."
    )]
    PluginBuildMismatch {
        plugin_path: String,
        plugin_identity: String,
        host_identity: String,
        plugin_abi_fingerprint: u64,
        host_abi_fingerprint: u64,
        plugin_transit_fingerprint: u64,
        host_transit_fingerprint: u64,
    },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// StreamLib result alias.
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
            streamlib_consumer_rhi::ConsumerRhiError::Configuration(s) => Error::Configuration(s),
        }
    }
}
