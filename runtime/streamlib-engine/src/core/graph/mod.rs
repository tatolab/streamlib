// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

mod components;
mod config_node_reference_edges;
mod config_schema_value_checks;
mod data_structure;

mod edges;
mod exposed_config_field_input_ports;
pub mod graph_fragment_flattening;
pub mod graph_interface_resolution;
mod graph_readiness;
pub mod graph_snapshot;
pub mod graph_variable_resolution;
pub mod input_values;
pub(crate) mod legacy_config_of_a_node;
mod node_state_ecs_component;
mod nodes;
mod pin_type_link_checks;
mod port_family_link_checks;
mod traits;
mod traversal;
mod validation;

#[cfg(test)]
mod graph_tests;
#[cfg(test)]
mod renamed_port_tests;

// top level
pub(crate) use config_node_reference_edges::{
    config_keys_of_a_node_type_holding_a_node_reference, config_node_reference_edges_of_one_node,
    refuse_a_live_config_naming_a_node_the_graph_does_not_hold,
    refuse_a_reference_to_a_node_of_the_wrong_type,
    refuse_a_second_referrer_of_a_node_that_takes_one,
};
pub(crate) use config_schema_value_checks::refuse_a_config_its_node_types_config_schema_refuses;
pub use data_structure::{Graph, GraphState, WhyTheNodeLeaves};
pub(crate) use data_structure::{
    names_listed_for_a_refusal, node_named_or_refused, node_names_listed_for_a_refusal,
    port_names_listed_for_a_refusal,
};
pub(crate) use exposed_config_field_input_ports::the_input_ports_of_the_exposed_config_fields;
pub use graph_readiness::ObservableGraphReadiness;
pub use node_state_ecs_component::{NodeState, NodeStateComponent};
pub(crate) use pin_type_link_checks::{
    ALinkConnected, TheEndsALinkIsJudgedBy, a_link_made_now_would_be_refused,
    refuse_a_link_its_pin_types_or_families_refuse, the_ends_a_link_is_judged_by,
};
pub(crate) use port_family_link_checks::{
    refuse_a_config_update_that_a_wired_link_out_of_the_node_would_refuse,
    the_family_a_link_out_of_an_output_carries, the_outputs_a_link_carries_on,
};
pub(crate) use streamlib_node_schema::is_in_exposed_name_cast_form;
pub use streamlib_node_schema::{
    EXPOSED_NAME_MAXIMUM_LENGTH, ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe,
};
pub use traits::{GraphEdgeWithComponents, GraphNodeWithComponents, GraphWeight};
pub use validation::validate_graph;

pub(crate) use components::HowOneReadersHelperRanEachFunctionNode;
pub use components::{
    ComponentMap, ComponentRenderer, DeviceMatchedAudioWindowContractsComponent,
    FunctionNodeHostComponent, FunctionNodePromotedToItsOwnProcessComponent,
    FunctionNodePromotionOutOfItsReadersIslands, FunctionNodeRanCompiledComponent, GraphRenders,
    HelperProcessRestartRecord, HelperProcessRestartsComponent, HowItsHelpersRanAFunctionNode,
    Iceoryx2ServicesHeldOpenForLinkComponent, LinkCarriedThroughTheFunctionNodePoolComponent,
    LinkComponent, LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent, LinkStateComponent,
    LossCountsOfPortsInThisProcess, NodeComponent, NodeFailureReasonComponent,
    NodeInstanceComponent, NodeLossCounts, NodeMetrics, NodeNotReadyBecauseComponent,
    NodePauseGateComponent, NodeReadyBarrierComponent, NodeReadyBarrierHandle, ObservableNodeState,
    OutOfProcessLinkWireProgress, OutOfProcessLinkWireRepliesComponent,
    OutOfProcessLinkWiringComponent, PendingDeletionComponent, ProcessRaisingBecauseComponent,
    ReadOfALinkByTheReaderOfAFunctionNodeIsland, ShaderNodeFusedGroupComponent,
    ShaderNodeFusedGroupOfAnIsland, ShutdownChannelComponent, StateComponent,
    StepProgressCallbackInFlight, StepProgressOfOneNode, StepProgressOfOneNodeComponent,
    StepProgressReadingOfOneNode, StorableComponent, ThreadHandleComponent,
    default_component_renderers, default_components,
};
pub(crate) use components::{
    NodeInstanceWithItsOutOfProcessLinkWiring, SourceSideHeldOpenForAReaderOutsideItsStream,
    SourceSideOfALinkWiredOn,
};
pub use edges::{
    InputLinkPortRef, InputPortMarker, Link, LinkCapacity, LinkDirection, LinkState, LinkUniqueId,
    OutputLinkPortRef, OutputPortMarker, PortMarker, input, output,
};
pub(crate) use nodes::the_refusal_of_a_link_into_a_setting;
pub use nodes::{
    GraphNode, GraphNodePorts, InputHoldingAValueUnderANameItHad, InputNumberRange, NodeRunId,
    PortInfo,
};
pub(crate) use traversal::the_default_name_a_new_node_takes;
pub use traversal::{
    LinkTraversal, LinkTraversalMut, NodeTraversal, NodeTraversalMut, TraversalSource,
    TraversalSourceMut,
};
