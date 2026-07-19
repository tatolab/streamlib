// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor-authoring traits and support types (cdylib arm).
//!
//! These are the engine-free copies of the engine's processor authoring
//! surface: the mode traits (`ReactiveProcessor` / `ContinuousProcessor`
//! / `ManualProcessor`), the `Config` bound, `EmptyConfig`, the internal
//! [`GeneratedProcessor`] trait the `#[processor]` macro implements, the
//! object-safe [`DynGeneratedProcessor`] companion, [`ProcessorSpec`],
//! and the port-marker traits. The lifecycle methods take the SDK's
//! `#[repr(C)]` [`RuntimeContext`](crate::context) views and return
//! [`streamlib_error::Result`].

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value as JsonValue;

use streamlib_error::{Error, Result};
use streamlib_processor_schema::descriptors::ProcessorDescriptor;
use streamlib_processor_schema::{ExecutionConfig, SchemaIdent};

use crate::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use crate::iceoryx2::{InputMailboxes, InputMailboxesInner, OutputWriter, OutputWriterInner};

// =============================================================================
// Config
// =============================================================================

/// Trait for processor configuration types.
///
/// All processor configs must be pure data that can round-trip through JSON.
pub trait Config:
    Send + Sync + 'static + Default + Serialize + DeserializeOwned + PartialEq
{
    /// Validate that config can round-trip through JSON without data loss.
    fn validate_round_trip(&self) -> std::result::Result<(), ConfigValidationError> {
        let json = serde_json::to_value(self)
            .map_err(|e| ConfigValidationError::SerializationFailed(e.to_string()))?;
        let round_tripped: Self = serde_json::from_value(json)
            .map_err(|e| ConfigValidationError::DeserializationFailed(e.to_string()))?;
        if self != &round_tripped {
            return Err(ConfigValidationError::RoundTripMismatch);
        }
        Ok(())
    }
}

/// Failure modes for [`Config::validate_round_trip`].
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigValidationError {
    /// JSON serialization of the config failed.
    SerializationFailed(String),
    /// JSON deserialization back into the config type failed.
    DeserializationFailed(String),
    /// The config did not survive a JSON round-trip unchanged.
    RoundTripMismatch,
}

impl std::fmt::Display for ConfigValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SerializationFailed(e) => write!(f, "Config serialization failed: {}", e),
            Self::DeserializationFailed(e) => write!(f, "Config deserialization failed: {}", e),
            Self::RoundTripMismatch => write!(
                f,
                "Config round-trip mismatch: some fields may be skipped during serialization"
            ),
        }
    }
}

impl std::error::Error for ConfigValidationError {}

/// Blanket implementation for all types meeting the requirements.
impl<T> Config for T where
    T: Send + Sync + 'static + Default + Serialize + DeserializeOwned + PartialEq
{
}

/// Empty config type for processors that don't need configuration.
///
/// Config-as-bag delivers config as a named map, so `EmptyConfig` must
/// tolerate any wire shape: an empty map `{}`, a legacy `nil`, or a
/// populated map (whose fields it discards). It serializes back as an
/// empty named map so it round-trips as a bag.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EmptyConfig;

// twin-guard(empty-config-serde): BEGIN — wire-load-bearing twin of the engine's
// EmptyConfig serde in runtime/streamlib-engine/src/core/processors/mod.rs. Config
// crosses the plugin ABI, so both sides must serialize to the same empty named map
// and tolerate any decode shape. twin_drift_guard.rs trip-wires an edit to either.
impl Serialize for EmptyConfig {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        serializer.serialize_map(Some(0))?.end()
    }
}

impl<'de> Deserialize<'de> for EmptyConfig {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_ignored_any(serde::de::IgnoredAny)?;
        Ok(EmptyConfig)
    }
}
// twin-guard(empty-config-serde): END

// =============================================================================
// Mode traits
// =============================================================================

/// Processor that reacts to input data.
///
/// Runtime calls `process()` when upstream writes to any input port.
pub trait ReactiveProcessor {
    /// Called once when the processor starts. Privileged ctx.
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called once when the processor stops. Privileged ctx.
    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called when the processor is paused. Restricted ctx.
    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called when the processor is resumed after being paused. Restricted ctx.
    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called when input data arrives. Restricted ctx.
    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;
}

/// Processor that runs continuously in a loop.
pub trait ContinuousProcessor {
    /// Called once when the processor starts. Privileged ctx.
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called once when the processor stops. Privileged ctx.
    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called when the processor is paused. Restricted ctx.
    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called when the processor is resumed after being paused. Restricted ctx.
    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called repeatedly by the runtime in a loop. Restricted ctx.
    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;
}

/// Processor with manual timing control.
pub trait ManualProcessor {
    /// Called once when the processor starts. Privileged ctx.
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called once when the processor stops. Privileged ctx.
    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called when the processor is paused. Restricted ctx.
    fn on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called when the processor is resumed after being paused. Restricted ctx.
    fn on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called once to start the processor. Privileged ctx.
    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    /// Called when the processor should stop. Privileged ctx.
    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

// =============================================================================
// GeneratedProcessor (macro-implemented) + DynGeneratedProcessor
// =============================================================================

/// Internal trait implemented by the processor macro.
///
/// **DO NOT IMPLEMENT DIRECTLY** — use the `#[processor]` macro instead.
pub trait GeneratedProcessor: Send + 'static {
    /// Processor config type.
    type Config: Config;

    /// Returns the processor name.
    fn name(&self) -> &str;

    /// Construct an instance from its config.
    fn from_config(config: Self::Config) -> Result<Self>
    where
        Self: Sized;

    /// Hot-path entry point. Restricted ctx.
    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;

    /// Update configuration at runtime (hot-reload).
    fn update_config(&mut self, _config: Self::Config) -> Result<()> {
        Ok(())
    }

    /// Apply a JSON config update at runtime.
    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> Result<()>
    where
        Self: Sized,
    {
        let config: Self::Config = serde_json::from_value(config_json.clone())
            .map_err(|e| Error::Config(e.to_string()))?;
        self.update_config(config)
    }

    /// Returns the execution configuration for this processor.
    fn execution_config(&self) -> ExecutionConfig {
        ExecutionConfig::default()
    }

    /// Returns the processor descriptor.
    fn descriptor() -> Option<ProcessorDescriptor>
    where
        Self: Sized;

    /// Check if this processor has iceoryx2-based output ports.
    fn has_iceoryx2_outputs(&self) -> bool {
        false
    }

    /// Check if this processor has iceoryx2-based input ports.
    fn has_iceoryx2_inputs(&self) -> bool {
        false
    }

    /// Install host-allocated iceoryx2 resources on this processor.
    fn set_iceoryx2_resources(
        &mut self,
        _output_writer: Option<OutputWriter>,
        _input_mailboxes: Option<InputMailboxes>,
    ) -> Result<()> {
        Ok(())
    }

    /// Borrow the host-side `OutputWriterInner` Arc the processor's
    /// PluginAbiObject is wired to, if any. Default `None`; the macro emits an
    /// override for processors with outputs (which returns `None` in
    /// cdylib mode — the inner reach is host-only).
    fn iceoryx2_output_writer_inner(&self) -> Option<std::sync::Arc<OutputWriterInner>> {
        None
    }

    /// Borrow the host-side `InputMailboxesInner` Arc the processor's
    /// PluginAbiObject is wired to, if any. Default `None`; the macro emits an
    /// override for processors with inputs.
    fn iceoryx2_input_mailboxes_inner(&self) -> Option<std::sync::Arc<InputMailboxesInner>> {
        None
    }

    /// Serialize processor-specific runtime state to JSON.
    fn to_runtime_json(&self) -> JsonValue {
        JsonValue::Null
    }

    /// Get the current config as JSON.
    fn config_json(&self) -> JsonValue
    where
        Self: Sized,
    {
        JsonValue::Null
    }

    /// Generated setup hook called by runtime with privileged ctx.
    fn __generated_setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Generated teardown hook called by runtime with privileged ctx.
    fn __generated_teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Generated on_pause hook — restricted ctx.
    fn __generated_on_pause(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Generated on_resume hook — restricted ctx.
    fn __generated_on_resume(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }

    /// Called once to start a Manual mode processor. Privileged ctx.
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(Error::Runtime(
            "start() is only valid for Manual execution mode".into(),
        ))
    }

    /// Called to stop a Manual mode processor. Privileged ctx.
    fn stop(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Err(Error::Runtime(
            "stop() is only valid for Manual execution mode".into(),
        ))
    }
}

/// Object-safe version of [`GeneratedProcessor`] for dynamic dispatch.
///
/// **DO NOT USE DIRECTLY** — internal implementation detail. A blanket
/// impl covers every [`GeneratedProcessor`].
pub trait DynGeneratedProcessor: Send + 'static {
    /// Generated setup hook called by runtime with privileged ctx.
    fn __generated_setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    /// Generated teardown hook called by runtime with privileged ctx.
    fn __generated_teardown(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    /// Generated on_pause hook — restricted ctx.
    fn __generated_on_pause(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;

    /// Generated on_resume hook — restricted ctx.
    fn __generated_on_resume(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;

    /// Hot-path entry point. Restricted ctx.
    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()>;

    /// Called once to start a Manual mode processor. Privileged ctx.
    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    /// Called to stop a Manual mode processor. Privileged ctx.
    fn stop(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()>;

    /// Returns the processor name.
    fn name(&self) -> &str;

    /// Returns the processor descriptor.
    fn descriptor(&self) -> Option<ProcessorDescriptor>;

    /// Returns the execution configuration for this processor.
    fn execution_config(&self) -> ExecutionConfig;

    /// Check if this processor has iceoryx2-based output ports.
    fn has_iceoryx2_outputs(&self) -> bool;

    /// Check if this processor has iceoryx2-based input ports.
    fn has_iceoryx2_inputs(&self) -> bool;

    /// Install host-allocated iceoryx2 resources.
    fn set_iceoryx2_resources(
        &mut self,
        output_writer: Option<OutputWriter>,
        input_mailboxes: Option<InputMailboxes>,
    ) -> Result<()>;

    /// Borrow the host-side `OutputWriterInner` Arc.
    fn iceoryx2_output_writer_inner(&self) -> Option<std::sync::Arc<OutputWriterInner>>;

    /// Borrow the host-side `InputMailboxesInner` Arc.
    fn iceoryx2_input_mailboxes_inner(&self) -> Option<std::sync::Arc<InputMailboxesInner>>;

    /// Apply a JSON config update at runtime.
    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> Result<()>;

    /// Serialize processor-specific runtime state to JSON.
    fn to_runtime_json(&self) -> serde_json::Value;

    /// Get the current config as JSON.
    fn config_json(&self) -> serde_json::Value;

    /// Erase to `&mut dyn Any`.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// Blanket implementation of [`DynGeneratedProcessor`] for all
/// [`GeneratedProcessor`] types.
impl<T> DynGeneratedProcessor for T
where
    T: GeneratedProcessor,
{
    fn __generated_setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::__generated_setup(self, ctx)
    }

    fn __generated_teardown(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::__generated_teardown(self, ctx)
    }

    fn __generated_on_pause(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::__generated_on_pause(self, ctx)
    }

    fn __generated_on_resume(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::__generated_on_resume(self, ctx)
    }

    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::process(self, ctx)
    }

    fn start(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::start(self, ctx)
    }

    fn stop(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        <Self as GeneratedProcessor>::stop(self, ctx)
    }

    fn name(&self) -> &str {
        <Self as GeneratedProcessor>::name(self)
    }

    fn descriptor(&self) -> Option<ProcessorDescriptor> {
        <T as GeneratedProcessor>::descriptor()
    }

    fn execution_config(&self) -> ExecutionConfig {
        <Self as GeneratedProcessor>::execution_config(self)
    }

    fn has_iceoryx2_outputs(&self) -> bool {
        <Self as GeneratedProcessor>::has_iceoryx2_outputs(self)
    }

    fn has_iceoryx2_inputs(&self) -> bool {
        <Self as GeneratedProcessor>::has_iceoryx2_inputs(self)
    }

    fn set_iceoryx2_resources(
        &mut self,
        output_writer: Option<OutputWriter>,
        input_mailboxes: Option<InputMailboxes>,
    ) -> Result<()> {
        <Self as GeneratedProcessor>::set_iceoryx2_resources(self, output_writer, input_mailboxes)
    }

    fn iceoryx2_output_writer_inner(&self) -> Option<std::sync::Arc<OutputWriterInner>> {
        <Self as GeneratedProcessor>::iceoryx2_output_writer_inner(self)
    }

    fn iceoryx2_input_mailboxes_inner(&self) -> Option<std::sync::Arc<InputMailboxesInner>> {
        <Self as GeneratedProcessor>::iceoryx2_input_mailboxes_inner(self)
    }

    fn apply_config_json(&mut self, config_json: &serde_json::Value) -> Result<()> {
        <Self as GeneratedProcessor>::apply_config_json(self, config_json)
    }

    fn to_runtime_json(&self) -> serde_json::Value {
        <Self as GeneratedProcessor>::to_runtime_json(self)
    }

    fn config_json(&self) -> serde_json::Value {
        <Self as GeneratedProcessor>::config_json(self)
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// Doc-hidden re-export module the `#[processor]` macro targets when
/// emitting `impl ...::__generated_private::GeneratedProcessor for ...`.
#[doc(hidden)]
pub mod __generated_private {
    pub use super::{DynGeneratedProcessor, GeneratedProcessor};
}

// =============================================================================
// ProcessorSpec
// =============================================================================

/// Specification for creating a processor.
///
/// Contains only what the user provides: processor identity and
/// configuration. Internal details (id, ports) are resolved by the
/// runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessorSpec {
    /// Structured processor identity (matches the registered key).
    pub name: SchemaIdent,
    /// Configuration as JSON value.
    pub config: serde_json::Value,
    /// Display name override. `None` defaults to the processor's
    /// PascalCase short name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

impl ProcessorSpec {
    /// Build a spec from a structured identity and a JSON config.
    pub fn new(name: SchemaIdent, config: serde_json::Value) -> Self {
        Self {
            name,
            config,
            display_name: None,
        }
    }

    /// Set a custom display name for this processor.
    pub fn with_display_name(mut self, display_name: impl Into<String>) -> Self {
        self.display_name = Some(display_name.into());
        self
    }
}

// =============================================================================
// Port markers
// =============================================================================

/// Marker trait for output ports.
pub trait OutputPortMarker {
    /// The declared port name.
    const PORT_NAME: &'static str;
    /// The owning processor type.
    type Processor;
}

/// Marker trait for input ports.
pub trait InputPortMarker {
    /// The declared port name.
    const PORT_NAME: &'static str;
    /// The owning processor type.
    type Processor;
}

/// Wrapper trait for port markers.
pub trait PortMarker {
    /// The declared port name.
    const PORT_NAME: &'static str;
}

impl<M: OutputPortMarker> PortMarker for M {
    const PORT_NAME: &'static str = M::PORT_NAME;
}
