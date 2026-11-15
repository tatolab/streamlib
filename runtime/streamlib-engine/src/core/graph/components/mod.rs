// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

mod component_map;
mod device_matched_audio_window_contracts_component;
mod function_node_host_component;
mod function_node_promoted_to_its_own_process_component;
mod function_node_ran_compiled_component;
mod iceoryx2_services_held_open_for_link_component;
mod json_component_trait;
mod link_carried_through_the_function_node_pool_component;
mod link_read_through_the_reader_of_its_function_node_island_component;
mod link_state_component;
mod link_type_info_component;
mod out_of_process_link_wire_replies_component;
mod out_of_process_link_wiring_component;
mod pending_deletion_component;
mod processor_instance_component;
mod processor_metrics;
mod processor_pause_gate_component;
mod processor_ready_barrier_component;
mod shader_node_fused_group_component;
mod shutdown_channel_component;
mod source_side_held_open_for_a_reader_outside_its_stream;
mod state_component;
mod step_progress_of_one_node_component;
mod thread_handle_component;

pub use component_map::{
    Component, ComponentMap, ComponentSerializer, StorableComponent, default_component_serializers,
    default_components,
};
pub use device_matched_audio_window_contracts_component::DeviceMatchedAudioWindowContractsComponent;
pub use function_node_host_component::FunctionNodeHostComponent;
pub use function_node_promoted_to_its_own_process_component::{
    FunctionNodePromotedToItsOwnProcessComponent, FunctionNodePromotionOutOfItsReadersIslands,
};
pub use function_node_ran_compiled_component::{
    FunctionNodeRanCompiledComponent, HowItsHelpersRanAFunctionNode,
};
pub use iceoryx2_services_held_open_for_link_component::Iceoryx2ServicesHeldOpenForLinkComponent;
pub use json_component_trait::JsonSerializableComponent;
pub use link_carried_through_the_function_node_pool_component::LinkCarriedThroughTheFunctionNodePoolComponent;
pub use link_read_through_the_reader_of_its_function_node_island_component::{
    LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent,
    ReadOfALinkByTheReaderOfAFunctionNodeIsland,
};
pub use link_state_component::LinkStateComponent;
pub use link_type_info_component::LinkTypeInfoComponent;
pub use out_of_process_link_wire_replies_component::{
    OutOfProcessLinkWireProgress, OutOfProcessLinkWireRepliesComponent,
};
pub use out_of_process_link_wiring_component::OutOfProcessLinkWiringComponent;
pub(crate) use out_of_process_link_wiring_component::ProcessorInstanceWithItsOutOfProcessLinkWiring;
pub use pending_deletion_component::PendingDeletionComponent;
pub use processor_instance_component::ProcessorInstanceComponent;
pub use processor_metrics::{
    LossCountsOfPortsInThisProcess, ProcessorLossCounts, ProcessorMetrics,
};
pub use processor_pause_gate_component::ProcessorPauseGateComponent;
pub use processor_ready_barrier_component::{
    ProcessorReadyBarrierComponent, ProcessorReadyBarrierHandle,
};
pub use shader_node_fused_group_component::{
    ShaderNodeFusedGroupComponent, ShaderNodeFusedGroupOfAnIsland,
};
pub use shutdown_channel_component::ShutdownChannelComponent;
pub(crate) use source_side_held_open_for_a_reader_outside_its_stream::{
    SourceSideHeldOpenForAReaderOutsideItsStream, SourceSideOfALinkWiredOn,
};
pub use state_component::{
    HelperProcessRestartRecord, HelperProcessRestartsComponent, NodeNotReadyBecauseComponent,
    ObservableProcessorState, ProcessRaisingBecauseComponent, ProcessorFailureReasonComponent,
    StateComponent,
};
pub use step_progress_of_one_node_component::{
    StepProgressCallbackInFlight, StepProgressOfOneNode, StepProgressOfOneNodeComponent,
    StepProgressReadingOfOneNode,
};
pub use thread_handle_component::ThreadHandleComponent;
