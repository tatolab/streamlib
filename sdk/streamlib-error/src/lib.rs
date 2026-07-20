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

    #[error("{}", unknown_processor_type_message(.ident))]
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

    #[error("acquire-on-reference for package {package} failed: {detail}")]
    AcquireOnReferenceFailed { package: PackageRef, detail: String },

    #[error(
        "This app's streamlib.yaml at {manifest_path} declares `dependencies:` \
         ({declared_count} package(s)), but an app is code, not a manifest — it \
         resolves processor refs against its installed set (streamlib_modules/ + \
         streamlib.lock), never a manifest dependency list. Remove the \
         `dependencies:` block; install each package with `streamlib add <source>` \
         (a folder, a .slpkg archive, or a URL)."
    )]
    AppManifestDeclaresDependencies {
        manifest_path: String,
        declared_count: usize,
    },

    #[error("Processor '{processor_id}' has no {direction} port named '{port_name}'")]
    ProcessorPortNotFound {
        processor_id: String,
        port_name: String,
        direction: PortDirection,
    },

    #[error(
        "Schema-ident mismatch on link {from_processor}:{from_port} -> \
         {to_processor}:{to_port}: producer emits `{producer_schema}` but consumer \
         port `{to_port}` expects `{consumer_schema}`. Strict schema validation was \
         requested for this wiring site — align the two schemas, or relax a port to \
         `any` to accept the mismatch."
    )]
    SchemaIdentMismatch {
        from_processor: String,
        from_port: String,
        to_processor: String,
        to_port: String,
        producer_schema: String,
        consumer_schema: String,
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

    #[error("Plugin host services unavailable: {0}")]
    PluginHostUnavailable(String),

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
         (abi_layout={plugin_abi_fingerprint:#018x}); host build: \
         [{host_identity}] (abi_layout={host_abi_fingerprint:#018x}). Rebuild the \
         plugin against the host's engine build — publish a matching engine `-dev` \
         version and bump the plugin's pin, or use `streamlib link`."
    )]
    PluginBuildMismatch {
        plugin_path: String,
        plugin_identity: String,
        host_identity: String,
        plugin_abi_fingerprint: u64,
        host_abi_fingerprint: u64,
    },

    #[error("Bag key '{key}' is not present")]
    BagKeyMissing { key: String },

    #[error("Bag key '{key}' could not be read as `{expected_type}`: {detail}")]
    BagTypeMismatch {
        key: String,
        expected_type: String,
        detail: String,
    },

    #[error("Bag msgpack decode failed: {0}")]
    BagDecodeFailed(String),

    #[error("Bag msgpack encode failed: {0}")]
    BagEncodeFailed(String),

    #[error(
        "payload of {payload_bytes} bytes on channel '{channel}' exceeds the \
         per-channel ceiling of {ceiling_bytes} bytes ({tier} tier) — the sample \
         was refused and counted, the stream continues; raise the node's \
         max_payload_bytes_per_channel for this tier or split the payload"
    )]
    PayloadExceedsChannelCeiling {
        channel: String,
        payload_bytes: usize,
        ceiling_bytes: usize,
        tier: String,
    },

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Render the [`Error::UnknownProcessorType`] message. A genuinely-absent
/// package-owned type carries a fix-it naming `streamlib add @org/name` — the
/// installed-set-only load gate resolves refs against `streamlib_modules/` +
/// `streamlib.lock`, so the recovery is to install the providing package. A
/// `@session/` type is exempt: session processors register live via
/// `Runner::add_local` and are never installable, so no `streamlib add` fix-it
/// is offered for them.
fn unknown_processor_type_message(ident: &SchemaIdent) -> String {
    if ident.org.is_reserved_for_session() {
        format!("Unknown processor type: {ident} (not registered)")
    } else {
        format!(
            "Unknown processor type: {ident} (not registered). No installed package \
             provides it — run `streamlib add @{}/{}` to install the providing package \
             into this app's streamlib_modules/, then re-run.",
            ident.org, ident.package
        )
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_processor_schema::{Org, Package, SchemaIdent, SemVer, TypeName};

    fn ident(org: &str, package: &str, ty: &str) -> SchemaIdent {
        SchemaIdent::new(
            Org::new(org).unwrap(),
            Package::new(package).unwrap(),
            TypeName::new(ty).unwrap(),
            SemVer::new(1, 0, 0),
        )
    }

    #[test]
    fn unknown_processor_type_names_streamlib_add_fix_it() {
        // A genuinely-absent package-owned type must name the exact
        // `streamlib add @org/name` recovery. Mentally revert the fix-it branch
        // and the message drops the actionable command.
        let msg = Error::UnknownProcessorType {
            ident: ident("tatolab", "camera", "Camera"),
        }
        .to_string();
        assert!(
            msg.contains("streamlib add @tatolab/camera"),
            "fix-it missing: {msg}"
        );
    }

    #[test]
    fn unknown_processor_type_exempts_session_types() {
        // A `@session/` type registers live via `add_local` and is never
        // installable, so it must NOT be told to `streamlib add`.
        let msg = Error::UnknownProcessorType {
            ident: ident("session", "test-mock", "TestMock"),
        }
        .to_string();
        assert!(
            !msg.contains("streamlib add"),
            "session type must not carry an install fix-it: {msg}"
        );
        assert!(msg.contains("not registered"), "message: {msg}");
    }

    #[test]
    fn app_manifest_declares_dependencies_names_the_installed_set() {
        let msg = Error::AppManifestDeclaresDependencies {
            manifest_path: "/app/streamlib.yaml".to_string(),
            declared_count: 2,
        }
        .to_string();
        assert!(msg.contains("streamlib_modules/"), "message: {msg}");
        assert!(msg.contains("streamlib add"), "message: {msg}");
    }
}
