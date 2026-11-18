// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

pub mod traits;

#[doc(hidden)]
pub mod __generated_private;

mod empty_config;
mod node_instance_factory;
mod node_spec;
mod node_types_one_stream_resolves;
mod out_of_process_link_wire_reply;
mod project_view_node;
pub use crate::core::graph::{NodeState, NodeStateComponent};

pub use traits::{Config, ConfigValidationError};
pub use traits::{ContinuousNode, ManualNode, ReactiveNode};

// Re-export internal traits (doc-hidden but needed by macro and runtime)
#[doc(hidden)]
pub use __generated_private::{
    DynGeneratedNode, GeneratedNode, OutOfProcessFarSideLinkDelivery,
    OutOfProcessLinkWiringEnvelope,
};

pub(crate) use __generated_private::{
    FunctionNodeCallUnderWayInTheFarSide, FunctionNodePromotionAwaitedBeforeTheReaderRestarts,
    FunctionNodeThatOverranItsFarSidesFrameInterval,
};
pub use empty_config::EmptyConfig;
pub use node_instance_factory::{
    DynamicNodeConstructorFn, NODE_REGISTRY, NodeInstance, NodeInstanceFactory,
};
pub use node_spec::NodeSpec;
pub(crate) use node_types_one_stream_resolves::{
    HowANodeOfADescribedTypeRuns, is_a_natively_compiled_node_type,
};
pub use node_types_one_stream_resolves::{InstalledModuleOfANodeType, NodeTypesOneStreamResolves};
pub(crate) use out_of_process_link_wire_reply::LinksAwaitingTheirOutOfProcessWireReply;
pub use out_of_process_link_wire_reply::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};
pub use project_view_node::VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME;
pub(crate) use project_view_node::{
    THE_FAMILY_A_GRAPH_INPUT_DECLARES, the_message_a_value_written_onto_an_output_is,
    the_node_type_of_a_top_level_graph_input,
};
