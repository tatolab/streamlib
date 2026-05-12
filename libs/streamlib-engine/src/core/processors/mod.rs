// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor infrastructure and implementations.

pub mod graph;
pub mod traits;

#[doc(hidden)]
pub mod __generated_private;

mod processor_instance_factory;
mod processor_spec;
// Re-export graph types
pub use graph::{ProcessorState, ProcessorStateComponent};

// Re-export processor traits
pub use traits::{Config, ConfigValidationError};
// Mode-specific processor traits
pub use traits::{ContinuousProcessor, ManualProcessor, ReactiveProcessor};

// Re-export internal traits (doc-hidden but needed by macro and runtime)
#[doc(hidden)]
pub use __generated_private::{DynGeneratedProcessor, GeneratedProcessor};

pub use processor_instance_factory::{
    macro_codegen, DynamicProcessorConstructorFn, ProcessorInstance, ProcessorInstanceFactory,
    RegisterResult, PROCESSOR_REGISTRY,
};
pub use processor_spec::ProcessorSpec;

/// Empty config type for processors that don't need configuration.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EmptyConfig;

// Audio processors (capture, output, mixer, channel converter, resampler,
// buffer rechunker, chord generator) live in `@tatolab/audio` (#672).

// Transformers
pub mod simple_passthrough;

pub use simple_passthrough::SimplePassthroughProcessor;
