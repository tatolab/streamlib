// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor infrastructure and implementations.

pub mod traits;

#[doc(hidden)]
pub mod __generated_private;

mod empty_config;
mod out_of_process_link_wire_reply;
mod processor_instance_factory;
mod processor_spec;
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
pub use __generated_private::{
    DynGeneratedProcessor, GeneratedProcessor, OutOfProcessLinkWiringEnvelope,
};

pub use empty_config::EmptyConfig;
pub use out_of_process_link_wire_reply::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};
// Bridge-internal: the board one helper's own bridge keeps. Deliberately not
// re-exported — `sdk::processors` is a blanket glob, and nothing outside this
// crate has any business holding another helper's board.
pub(crate) use out_of_process_link_wire_reply::LinksAwaitingTheirOutOfProcessWireReply;
pub use processor_instance_factory::{
    DynamicProcessorConstructorFn, PROCESSOR_REGISTRY, ProcessorInstance, ProcessorInstanceFactory,
};
pub use processor_spec::ProcessorSpec;

// Audio processors are not here: capture and playback ship as engine
// built-ins in `streamlib-media-builtins` (`microphone_source.rs`,
// `speaker_sink.rs`).
