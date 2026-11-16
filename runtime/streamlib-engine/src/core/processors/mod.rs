// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


pub mod traits;

#[doc(hidden)]
pub mod __generated_private;

mod empty_config;
mod node_types_one_stream_resolves;
mod out_of_process_link_wire_reply;
mod processor_instance_factory;
mod processor_spec;
mod project_view_node;
pub use crate::core::graph::{ProcessorState, ProcessorStateComponent};

pub use traits::{Config, ConfigValidationError};
pub use traits::{ContinuousProcessor, ManualProcessor, ReactiveProcessor};

// Re-export internal traits (doc-hidden but needed by macro and runtime)
#[doc(hidden)]
pub use __generated_private::{
    DynGeneratedProcessor, GeneratedProcessor, OutOfProcessFarSideLinkDelivery,
    OutOfProcessLinkWiringEnvelope,
};

pub(crate) use __generated_private::{
    FunctionNodeCallUnderWayInTheFarSide, FunctionNodePromotionAwaitedBeforeTheReaderRestarts,
    FunctionNodeThatOverranItsFarSidesFrameInterval,
};
pub use empty_config::EmptyConfig;
pub(crate) use node_types_one_stream_resolves::HowANodeOfADescribedTypeRuns;
pub use node_types_one_stream_resolves::{InstalledModuleOfANodeType, NodeTypesOneStreamResolves};
pub use out_of_process_link_wire_reply::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};
pub(crate) use out_of_process_link_wire_reply::LinksAwaitingTheirOutOfProcessWireReply;
pub use processor_instance_factory::{
    DynamicProcessorConstructorFn, PROCESSOR_REGISTRY, ProcessorInstance, ProcessorInstanceFactory,
};
pub use processor_spec::ProcessorSpec;
pub use project_view_node::VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME;
pub(crate) use project_view_node::{
    NodeWhoseOutputsAClientWritesProcessor, the_message_a_value_written_onto_an_output_is,
};
