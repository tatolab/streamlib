// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor and port descriptor types for introspection.
//!
//! These live in the engine-free `streamlib-processor-schema` crate so the
//! engine and the `#[processor]` macro share one definition. The engine
//! re-exports them here so every `crate::core::descriptors::*` path resolves
//! unchanged.

pub use streamlib_processor_schema::descriptors::{
    CodeExamples, ConfigDescriptor, ConfigField, PortDescriptor, ProcessorDescriptor,
    ProcessorRuntime,
};
pub use streamlib_processor_schema::{
    AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE, AUDIO_WINDOW_DTYPE_DECLARATION_VALUES,
    AudioWindowContract, AudioWindowContractDeclaredValues,
    ProcessorClassImportPath, ProcessorClassShortName, ProcessorScheduling,
    refuse_audio_window_beside_a_skipping_delivery_profile,
};
