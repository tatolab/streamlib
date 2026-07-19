// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor infrastructure and implementations.

pub mod traits;

#[doc(hidden)]
pub mod __generated_private;

mod processor_instance_factory;
mod processor_spec;
mod processor_type_reference;
// Re-export graph types — `ProcessorState` and `ProcessorStateComponent`
// live in `core::graph` (#786); kept as a re-export here so the public
// `streamlib::sdk::processors::ProcessorState` path stays stable.
pub use crate::core::graph::{ProcessorState, ProcessorStateComponent};

// Re-export processor traits
pub use traits::{Config, ConfigValidationError};
// Mode-specific processor traits
pub use traits::{ContinuousProcessor, ManualProcessor, ReactiveProcessor};

// Re-export internal traits (doc-hidden but needed by macro and runtime)
#[doc(hidden)]
pub use __generated_private::{DynGeneratedProcessor, GeneratedProcessor};

pub use processor_instance_factory::{
    DynamicProcessorConstructorFn, PROCESSOR_REGISTRY, ProcessorInstance, ProcessorInstanceFactory,
};
pub use processor_spec::ProcessorSpec;
pub use processor_type_reference::ProcessorTypeReference;

/// Empty config type for processors that don't need configuration.
///
/// Config-as-bag delivers config as a named map, so `EmptyConfig` must
/// tolerate any wire shape: an empty map `{}`, a legacy `nil`, or a
/// populated map (whose fields it discards). It serializes back as an
/// empty named map so it round-trips as a bag.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EmptyConfig;

// twin-guard(empty-config-serde): BEGIN — wire-load-bearing twin of the SDK's
// EmptyConfig serde in sdk/streamlib-plugin-sdk/src/processors.rs. Config crosses
// the plugin ABI, so both sides must serialize to the same empty named map and
// tolerate any decode shape. twin_drift_guard.rs trip-wires an edit to either.
impl serde::Serialize for EmptyConfig {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serde::ser::SerializeMap::end(serializer.serialize_map(Some(0))?)
    }
}

impl<'de> serde::Deserialize<'de> for EmptyConfig {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_ignored_any(serde::de::IgnoredAny)?;
        Ok(EmptyConfig)
    }
}
// twin-guard(empty-config-serde): END

// Audio processors (capture, output, mixer, channel converter, resampler,
// buffer rechunker, chord generator) live in `@tatolab/audio` (#672).
// SimplePassthrough lives in `@tatolab/debug-utilities` (#783).
