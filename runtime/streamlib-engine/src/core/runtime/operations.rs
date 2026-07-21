// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::error::Result;
use crate::core::graph::{LinkUniqueId, ProcessorUniqueId};
use crate::core::processors::ProcessorSpec;
use crate::core::{InputLinkPortRef, OutputLinkPortRef};
use std::future::Future;
use std::pin::Pin;
use streamlib_idents::ModuleIdent;
use streamlib_processor_schema::ProcessorLanguage;

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
    /// module_loader's transactional session-source seam. Returns the minted
    /// registration [`ModuleIdent`] (NOT an `add_processor` instance id).
    fn register_processor_source_async(
        &self,
        request: SubmittedProcessorSource,
    ) -> BoxFuture<'_, Result<ModuleIdent>>;

    /// Remove a prior `@session/<name>` registration, then re-register the
    /// replacement source at a monotonically-bumped `0.0.N`. Returns the
    /// minted registration [`ModuleIdent`] for the new definition.
    fn replace_processor_async(
        &self,
        request: ReplaceProcessorFromSource,
    ) -> BoxFuture<'_, Result<ModuleIdent>>;

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
    fn malformed_request_bytes_fail_to_decode() {
        // Invalid-args wire lock: a truncated / non-conforming buffer must
        // surface a decode error (the host callback converts this to an error
        // completion), never a partially-populated request.
        let garbage = [0xffu8, 0x00, 0x13, 0x37];
        let decoded = rmp_serde::from_slice::<SubmittedProcessorSource>(&garbage);
        assert!(decoded.is_err(), "malformed bytes must not decode");
    }
}
