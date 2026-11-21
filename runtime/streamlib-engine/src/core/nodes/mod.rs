// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

pub mod traits;

#[doc(hidden)]
pub mod __generated_private;

mod empty_config;
mod graph_interface_node_types;
#[cfg(test)]
mod graph_interface_node_types_tests;
mod graph_variable_node_types;
#[cfg(test)]
mod graph_variable_node_types_tests;
mod node_instance_factory;
mod node_spec;
mod node_type_ports_described_on_the_wire;
#[cfg(test)]
mod node_type_ports_described_on_the_wire_tests;
mod node_types_of_the_graph_itself;
#[cfg(test)]
mod node_types_of_the_graph_itself_tests;
mod node_types_one_stream_resolves;
mod out_of_process_link_wire_reply;
mod placed_stream_node_type;
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
    FiresNotHandedToTheFarSide, FunctionNodeCallUnderWayInTheFarSide,
    FunctionNodePromotionAwaitedBeforeTheReaderRestarts,
    FunctionNodeThatOverranItsFarSidesFrameInterval,
};
pub use empty_config::EmptyConfig;
pub(crate) use graph_interface_node_types::{
    the_family_a_graph_interface_value_port_declares, the_node_type_of_a_graph_input,
    the_settings_of_a_graph_input,
};
pub(crate) use graph_variable_node_types::{
    the_settings_of_a_get_variable_offering, the_value_port_of_a_graph_variable_node,
};
pub use node_instance_factory::{
    DynamicNodeConstructorFn, NODE_REGISTRY, NodeInstance, NodeInstanceFactory,
};
pub use node_spec::NodeSpec;
pub use node_type_ports_described_on_the_wire::NodeTypePortsDescribedOnTheWire;
pub use node_types_of_the_graph_itself::{
    GRAPH_NODE_TYPE_CATEGORY, the_node_types_of_the_graph_itself,
};
pub(crate) use node_types_of_the_graph_itself::{
    refuse_a_link_into_a_setting_of_a_node_of_the_graph_itself,
    the_settings_of_a_node_type_of_the_graph_itself,
};
pub(crate) use node_types_one_stream_resolves::{
    HowANodeOfADescribedTypeRuns, is_a_natively_compiled_node_type,
};
pub use node_types_one_stream_resolves::{InstalledModuleOfANodeType, NodeTypesOneStreamResolves};
pub(crate) use out_of_process_link_wire_reply::LinksAwaitingTheirOutOfProcessWireReply;
pub use out_of_process_link_wire_reply::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};
pub(crate) use placed_stream_node_type::{
    the_setting_of_a_placed_stream_named, the_settings_of_a_placed_stream,
};
pub use project_view_node::VIEW_NODE_ARRAY_OUTPUT_ATTRIBUTE_NAME;
pub(crate) use project_view_node::{
    is_the_input_of_an_output_a_panel_writes,
    refuse_a_saved_value_on_the_input_of_an_output_a_panel_writes, the_names_listed,
    the_refusal_of_a_value_an_output_does_not_carry,
    the_saved_values_without_any_on_an_output_a_panel_writes,
};
