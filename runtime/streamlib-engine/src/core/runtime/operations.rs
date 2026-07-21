// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::error::Result;
use crate::core::graph::{LinkUniqueId, ProcessorUniqueId};
use crate::core::processors::ProcessorSpec;
use crate::core::runtime::TapSubscription;
use crate::core::{InputLinkPortRef, OutputLinkPortRef};
use std::future::Future;
use std::pin::Pin;
use streamlib_idents::ModuleIdent;
use streamlib_processor_schema::PortSchemaSpec;
pub use streamlib_processor_schema::ProcessorLanguage;

use crate::core::descriptors::{PortDescriptor, ProcessorDescriptor};

/// Boxed future type for async trait methods (required for dyn compatibility).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub use crate::core::schema_agreement::SchemaValidationPosture;

/// Per-wiring-site options for a connect call.
///
/// The default is the engine-wide loose-but-observed schema validation
/// ([`SchemaValidationPosture::Loose`]): a concrete producer/consumer schema
/// mismatch warns but the link is still wired. A safety-critical channel selects
/// [`ConnectOptions::strict`] so the same mismatch instead hard-fails at the
/// wiring site with [`Error::SchemaIdentMismatch`].
///
/// [`Error::SchemaIdentMismatch`]: crate::core::error::Error::SchemaIdentMismatch
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ConnectOptions {
    /// Schema-agreement posture applied when the link is wired.
    pub validation: SchemaValidationPosture,
}

impl ConnectOptions {
    /// Loose-but-observed validation — the engine-wide default; a concrete
    /// schema mismatch warns, then wires the link anyway.
    pub fn loose() -> Self {
        Self {
            validation: SchemaValidationPosture::Loose,
        }
    }

    /// Strict validation — a concrete producer/consumer schema mismatch is
    /// rejected at the wiring site with [`Error::SchemaIdentMismatch`].
    ///
    /// [`Error::SchemaIdentMismatch`]: crate::core::error::Error::SchemaIdentMismatch
    pub fn strict() -> Self {
        Self {
            validation: SchemaValidationPosture::Strict,
        }
    }

    /// Set the schema-validation posture explicitly.
    #[must_use]
    pub fn with_validation(mut self, validation: SchemaValidationPosture) -> Self {
        self.validation = validation;
        self
    }
}

/// A processor definition submitted as source text for live registration
/// into a running runtime (`register_processor_source` / `replace_processor`).
///
/// The host stages the source through the module_loader's transactional
/// session-source seam and mints a `@session/<name>@0.0.N` identity. This is
/// the msgpack wire payload the `RuntimeOpsVTable` v3 slots carry, so it is
/// serde-stable across the plugin ABI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SubmittedProcessorSource {
    /// The processor source text (a Python module / a TypeScript module).
    pub source_text: String,
    /// The runtime language the source is authored in.
    pub language: ProcessorLanguage,
    /// The `@session/<name>` package-name segment to mint the registration
    /// under. `None` derives a default segment from `processor_type_name`.
    pub requested_name: Option<String>,
    /// The PascalCase processor type name the source defines — the subprocess
    /// entrypoint class symbol and the registered short type name. `None`
    /// derives it from `requested_name`.
    pub processor_type_name: Option<String>,
}

/// A `replace_processor` request: remove a prior `@session/<name>`
/// registration, then re-register `replacement` at a monotonically-bumped
/// `0.0.N`. The msgpack wire payload the `RuntimeOpsVTable::replace_processor`
/// slot carries.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ReplaceProcessorFromSource {
    /// The `@session/<name>` module whose prior registration is removed
    /// before the replacement registers.
    pub target_session_module: ModuleIdent,
    /// The replacement definition, registered at a fresh `0.0.N`.
    pub replacement: SubmittedProcessorSource,
}

/// The success payload of `register_processor_source` / `replace_processor`:
/// the minted registration [`ModuleIdent`] plus each installed processor's
/// committed port surface.
///
/// The msgpack wire payload the register/replace slots return; serde-stable
/// across the plugin ABI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RegisterProcessorReceipt {
    /// The minted `@session/<name>@0.0.N` registration ident (NOT an
    /// `add_processor` instance id).
    pub module: ModuleIdent,
    /// The processors the registration installed, with their committed ports.
    pub processors: Vec<RegisteredProcessorReceipt>,
}

impl RegisterProcessorReceipt {
    /// A receipt of the minted registration ident plus each installed
    /// processor's committed ports.
    pub fn new(module: ModuleIdent, processors: Vec<RegisteredProcessorReceipt>) -> Self {
        Self { module, processors }
    }
}

/// One processor's committed port surface within a [`RegisterProcessorReceipt`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RegisteredProcessorReceipt {
    /// The processor's PascalCase short `Type` name.
    pub name: String,
    /// Input ports, in declaration order.
    pub inputs: Vec<RegisteredPortReceipt>,
    /// Output ports, in declaration order.
    pub outputs: Vec<RegisteredPortReceipt>,
}

impl RegisteredProcessorReceipt {
    /// Project a committed [`ProcessorDescriptor`] onto its receipt surface.
    pub fn from_descriptor(descriptor: &ProcessorDescriptor) -> Self {
        Self {
            name: descriptor.name.r#type.as_str().to_string(),
            inputs: descriptor
                .inputs
                .iter()
                .map(RegisteredPortReceipt::from_port)
                .collect(),
            outputs: descriptor
                .outputs
                .iter()
                .map(RegisteredPortReceipt::from_port)
                .collect(),
        }
    }
}

/// One committed port within a [`RegisteredProcessorReceipt`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RegisteredPortReceipt {
    /// The port name.
    pub name: String,
    /// The port's schema id — `Any` or a fully-qualified `SchemaIdent`.
    /// Serialized through the plugin-ABI-safe wire codec so `Specific`
    /// round-trips across the msgpack boundary (the default `PortSchemaSpec`
    /// serde is YAML-shaped and lossy on `Specific`).
    #[serde(with = "crate::core::descriptors::port_schema_spec_wire")]
    pub schema: PortSchemaSpec,
    /// Input-port delivery-profile override (`"latest"` / `"every_sample"` /
    /// `"lossless"`); always `None` on output ports.
    #[serde(default)]
    pub delivery_profile: Option<String>,
}

impl RegisteredPortReceipt {
    fn from_port(port: &PortDescriptor) -> Self {
        Self {
            name: port.name.clone(),
            schema: port.schema.clone(),
            delivery_profile: port.delivery_profile.clone(),
        }
    }
}

/// Unified interface for runtime graph operations.
///
/// Implemented by `Runner` (direct) and `RuntimeProxy` (channel-based).
/// Callers use this trait and don't need to know the underlying implementation.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to allow sharing across threads.
/// Graph operations should return quickly - compilation happens asynchronously.
///
/// # Sync vs Async Methods
///
/// Both sync and async variants are provided:
/// - **Async methods** (`*_async`): Safe to call from any context including tokio tasks.
///   Use these from async code: `ctx.runtime().add_processor_async(spec).await`
/// - **Sync methods**: Convenience wrappers that block on the async variants.
///   Use these from sync code: `runtime.add_processor(spec)`
///
/// The sync methods internally use `block_on`, so they must NOT be called from
/// within a tokio task (will panic). Use the async variants in async contexts.
pub trait RuntimeOperations: Send + Sync {
    // =========================================================================
    // Async Methods (primary implementation - safe from any context)
    // =========================================================================

    /// Add a processor to the graph asynchronously. Returns the processor ID.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the ID in fire-and-forget scenarios.
    fn add_processor_async(&self, spec: ProcessorSpec) -> BoxFuture<'_, Result<ProcessorUniqueId>>;

    /// Remove a processor from the graph asynchronously.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the result in fire-and-forget scenarios.
    fn remove_processor_async(&self, processor_id: ProcessorUniqueId) -> BoxFuture<'_, Result<()>>;

    /// Connect two ports asynchronously. Returns the link ID.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the ID in fire-and-forget scenarios.
    fn connect_async(
        &self,
        from: OutputLinkPortRef,
        to: InputLinkPortRef,
    ) -> BoxFuture<'_, Result<LinkUniqueId>>;

    /// Disconnect a link asynchronously.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the result in fire-and-forget scenarios.
    fn disconnect_async(&self, link_id: LinkUniqueId) -> BoxFuture<'_, Result<()>>;

    /// Export graph state as JSON asynchronously.
    fn to_json_async(&self) -> BoxFuture<'_, Result<serde_json::Value>>;

    /// Register a processor definition from source text into the live
    /// runtime, minting it a `@session/<name>@0.0.N` identity through the
    /// module_loader's transactional session-source seam. Returns a
    /// [`RegisterProcessorReceipt`] — the minted registration ident plus the
    /// committed port surface of each installed processor.
    fn register_processor_source_async(
        &self,
        request: SubmittedProcessorSource,
    ) -> BoxFuture<'_, Result<RegisterProcessorReceipt>>;

    /// Remove a prior `@session/<name>` registration, then re-register the
    /// replacement source at a monotonically-bumped `0.0.N`. Returns a
    /// [`RegisterProcessorReceipt`] for the new definition.
    fn replace_processor_async(
        &self,
        request: ReplaceProcessorFromSource,
    ) -> BoxFuture<'_, Result<RegisterProcessorReceipt>>;

    /// Attach a read-only tap to a named channel, streaming its raw bags.
    ///
    /// `channel` is a channel data-service name
    /// (`{source_processor}/{source_output_port}`,
    /// `streamlib_idents::source_channel_name`); `count` bounds the tap to that
    /// many bags then ends, `None` streams live until the returned
    /// [`TapSubscription`] is dropped. The tap consumes the channel's single
    /// reserved subscriber slot with no publisher re-open, so exactly one
    /// concurrent tap per channel is allowed — a second attach fails with
    /// [`Error::TapSlotOccupied`] until the first detaches (drops). An unwired /
    /// unknown channel fails with [`Error::TapChannelNotFound`].
    ///
    /// There is no sync variant: a tap yields a live streaming handle, not a
    /// one-shot result, so blocking on it is never the intent. Host-side only —
    /// a plugin cdylib cannot own the host's `!Send` subscriber, so
    /// implementations reachable only across the plugin ABI reject this with
    /// [`Error::NotSupported`].
    ///
    /// [`Error::TapSlotOccupied`]: crate::core::error::Error::TapSlotOccupied
    /// [`Error::TapChannelNotFound`]: crate::core::error::Error::TapChannelNotFound
    /// [`Error::NotSupported`]: crate::core::error::Error::NotSupported
    fn tap_async(
        &self,
        channel: String,
        count: Option<usize>,
    ) -> BoxFuture<'_, Result<TapSubscription>>;

    // =========================================================================
    // Sync Methods (convenience wrappers - NOT safe from tokio tasks)
    // =========================================================================

    /// Add a processor to the graph. Returns the processor ID.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the ID in fire-and-forget scenarios.
    ///
    /// This is a blocking wrapper around [`add_processor_async`]. Do not call
    /// from within a tokio task - use the async variant instead.
    fn add_processor(&self, spec: ProcessorSpec) -> Result<ProcessorUniqueId>;

    /// Remove a processor from the graph.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the result in fire-and-forget scenarios.
    ///
    /// This is a blocking wrapper around [`remove_processor_async`]. Do not call
    /// from within a tokio task - use the async variant instead.
    fn remove_processor(&self, processor_id: &ProcessorUniqueId) -> Result<()>;

    /// Connect two ports. Returns the link ID.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the ID in fire-and-forget scenarios.
    ///
    /// This is a blocking wrapper around [`connect_async`]. Do not call
    /// from within a tokio task - use the async variant instead.
    fn connect(&self, from: OutputLinkPortRef, to: InputLinkPortRef) -> Result<LinkUniqueId>;

    /// Disconnect a link.
    ///
    /// Note: No `#[must_use]` - callers may intentionally ignore the result in fire-and-forget scenarios.
    ///
    /// This is a blocking wrapper around [`disconnect_async`]. Do not call
    /// from within a tokio task - use the async variant instead.
    fn disconnect(&self, link_id: &LinkUniqueId) -> Result<()>;

    // =========================================================================
    // Introspection
    // =========================================================================

    /// Export graph state as JSON including topology, processor states, metrics, and buffer levels.
    fn to_json(&self) -> Result<serde_json::Value>;
}

#[cfg(test)]
mod source_submit_wire_tests {
    //! Tier-1 wire-format locks for the `RuntimeOpsVTable` v3 register-
    //! from-source payloads. These msgpack shapes cross the plugin ABI
    //! (`register_processor_source` / `replace_processor` slots), so a
    //! field rename / reorder must fail here, not silently at a plugin.

    use super::*;

    #[test]
    fn submitted_source_round_trips_through_msgpack() {
        let request = SubmittedProcessorSource {
            source_text: "class Widget:\n    pass\n".to_string(),
            language: ProcessorLanguage::Python,
            requested_name: Some("widget".to_string()),
            processor_type_name: Some("Widget".to_string()),
        };
        let bytes = rmp_serde::to_vec_named(&request).expect("encode");
        let decoded: SubmittedProcessorSource = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(decoded.source_text, request.source_text);
        assert_eq!(decoded.language, request.language);
        assert_eq!(decoded.requested_name, request.requested_name);
        assert_eq!(decoded.processor_type_name, request.processor_type_name);
    }

    #[test]
    fn replace_request_round_trips_through_msgpack() {
        let target = streamlib_idents::mint_session_module_ident("widget")
            .expect("valid name mints")
            .module;
        let request = ReplaceProcessorFromSource {
            target_session_module: target.clone(),
            replacement: SubmittedProcessorSource {
                source_text: "class Widget:\n    pass\n".to_string(),
                language: ProcessorLanguage::TypeScript,
                requested_name: None,
                processor_type_name: Some("Widget".to_string()),
            },
        };
        let bytes = rmp_serde::to_vec_named(&request).expect("encode");
        let decoded: ReplaceProcessorFromSource = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(
            decoded.target_session_module.to_string(),
            target.to_string()
        );
        assert_eq!(decoded.replacement.language, ProcessorLanguage::TypeScript);
    }

    #[test]
    fn register_receipt_round_trips_through_msgpack() {
        // The v3 SUCCESS payload: minted ident + per-processor committed ports.
        // Crosses the plugin ABI, so a field rename / reorder or a schema-spec
        // wire-codec regression must fail here. Covers both an `Any` port and a
        // `Specific(SchemaIdent)` port (the case the default YAML-shaped
        // PortSchemaSpec serde would lose) plus an input delivery profile.
        use streamlib_idents::{Org, Package, SchemaIdent, SemVer, TypeName};

        let module = streamlib_idents::mint_session_module_ident("widget")
            .expect("valid name mints")
            .module;
        let specific = PortSchemaSpec::Specific(SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("core").unwrap(),
            TypeName::new("VideoFrame").unwrap(),
            SemVer::new(1, 2, 3),
        ));
        let receipt = RegisterProcessorReceipt::new(
            module.clone(),
            vec![RegisteredProcessorReceipt {
                name: "Widget".to_string(),
                inputs: vec![RegisteredPortReceipt {
                    name: "in0".to_string(),
                    schema: PortSchemaSpec::Any,
                    delivery_profile: Some("latest".to_string()),
                }],
                outputs: vec![RegisteredPortReceipt {
                    name: "out0".to_string(),
                    schema: specific.clone(),
                    delivery_profile: None,
                }],
            }],
        );

        let bytes = rmp_serde::to_vec_named(&receipt).expect("encode");
        let decoded: RegisterProcessorReceipt = rmp_serde::from_slice(&bytes).expect("decode");
        assert_eq!(decoded.module.to_string(), module.to_string());
        assert_eq!(decoded.processors.len(), 1);
        assert_eq!(decoded.processors[0].name, "Widget");
        assert_eq!(decoded.processors[0].inputs[0].name, "in0");
        assert_eq!(
            decoded.processors[0].inputs[0].delivery_profile.as_deref(),
            Some("latest")
        );
        assert_eq!(decoded.processors[0].outputs[0].name, "out0");
        assert_eq!(
            decoded.processors[0].outputs[0].schema, specific,
            "the Specific schema id must round-trip through the wire codec"
        );
    }

    #[test]
    fn malformed_request_bytes_fail_to_decode() {
        // Invalid-args wire lock: a truncated / non-conforming buffer must
        // surface a decode error (the host callback converts this to an error
        // completion), never a partially-populated request.
        let garbage = [0xffu8, 0x00, 0x13, 0x37];
        let decoded = rmp_serde::from_slice::<SubmittedProcessorSource>(&garbage);
        assert!(decoded.is_err(), "malformed bytes must not decode");
    }
}
