// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

mod bundle_member_link_ends;
mod components;
mod config_node_reference_edges;
pub mod connection_wire_lowering;
mod data_structure;

mod edges;
pub mod graph_fragment_flattening;
pub mod graph_interface_resolution;
mod graph_readiness;
pub mod graph_snapshot;
pub mod graph_variable_resolution;
pub mod input_values;
mod node_health_with_its_links;
mod node_state_ecs_component;
mod nodes;
mod pin_type_link_checks;
mod pin_type_resolution;
mod traits;
mod traversal;
mod validation;
mod values_port_facts_read;

#[cfg(test)]
mod graph_tests;
#[cfg(test)]
mod renamed_port_tests;

// top level
pub(crate) use bundle_member_link_ends::{
    WhatTheLinksIntoAnInputFeed, the_bundle_member_a_link_ends_at,
    the_bundle_member_a_link_starts_at,
};
pub(crate) use config_node_reference_edges::{
    config_keys_of_a_node_type_holding_a_node_reference, config_node_reference_edges_of_one_node,
    refuse_a_reference_to_a_node_of_the_wrong_type,
    refuse_a_second_referrer_of_a_node_that_takes_one,
    refuse_input_values_naming_a_node_the_graph_does_not_hold,
};
pub use data_structure::{Graph, GraphState, WhyTheNodeLeaves};
pub(crate) use data_structure::{
    names_listed_for_a_refusal, node_named_or_refused, node_names_listed_for_a_refusal,
    port_names_listed_for_a_refusal,
};
pub use graph_readiness::ObservableGraphReadiness;
pub use node_state_ecs_component::{NodeState, NodeStateComponent};
pub(crate) use pin_type_link_checks::{
    ALinkConnected, TheEndsALinkIsJudgedBy, a_link_made_now_would_be_refused,
    a_type_choice_of_the_node_types_a_port, refuse_a_link_its_pin_types_refuse,
    the_bundle_frame_type_an_output_may_carry, the_ends_a_link_is_judged_by,
    the_verdict_of_a_link_between_declared_ports, the_verdict_on_a_link_between_typed_ends,
};
pub(crate) use pin_type_resolution::{
    the_input_an_output_takes_the_type_of, the_outputs_a_link_carries_on,
    the_pin_a_link_out_of_an_output_carries, the_pin_a_port_resolves_to,
};
pub(crate) use streamlib_node_schema::is_in_exposed_name_cast_form;
pub use streamlib_node_schema::{
    EXPOSED_NAME_MAXIMUM_LENGTH, ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe,
};
pub use traits::{GraphEdgeWithComponents, GraphNodeWithComponents, GraphWeight};
pub use validation::validate_graph;
pub(crate) use values_port_facts_read::the_values_port_facts_read;

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
pub(crate) use components::{HowOneReadersHelperRanEachFunctionNode, LinkPinTypesDifferComponent};
pub(crate) use components::{
    NodeInstanceWithItsOutOfProcessLinkWiring, SourceSideHeldOpenForAReaderOutsideItsStream,
    SourceSideOfALinkWiredOn,
};
pub use edges::{
    InputLinkPortRef, InputPortMarker, Link, LinkCapacity, LinkDirection, LinkState, LinkUniqueId,
    OutputLinkPortRef, OutputPortMarker, PortMarker, input, output,
};
pub use nodes::{
    GraphNode, GraphNodePorts, InputHoldingAValueUnderANameItHad, NodeRunId, PinTypeAsDeclared,
    PortInfo,
};
pub(crate) use nodes::{
    PortAnnotations, grow_a_pin_group_to_hold_the_port, refuse_a_link_into_a_port_taking_no_wire,
    reshape_the_pin_group_slots_of, the_refusal_of_a_link_into_a_setting,
};
pub(crate) use traversal::the_default_name_a_new_node_takes;
pub use traversal::{
    LinkTraversal, LinkTraversalMut, NodeTraversal, NodeTraversalMut, TraversalSource,
    TraversalSourceMut,
};
