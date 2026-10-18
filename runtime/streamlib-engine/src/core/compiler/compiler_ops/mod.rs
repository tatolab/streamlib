// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod apply_processor_config_update_op;
mod open_iceoryx2_service_op;
mod prepare_processor_op;
pub(crate) mod processor_interpreter_describe;
pub(crate) mod processor_interpreter_shutdown_ladder;
pub(crate) mod processor_interpreter_spawn_host;
pub(crate) mod python_processor_declaration;
mod reconcile_a_link_reading_another_stream_op;
mod spawn_processor_op;
pub(crate) mod stream_function_compile_in_the_projects_interpreter;
pub(crate) mod subprocess_bridge;
mod subprocess_escalate;
mod subprocess_escalate_wire_types;

pub(crate) use apply_processor_config_update_op::{
    ProcessorConfigUpdateOutcome, apply_processor_config_update,
};
pub(crate) use open_iceoryx2_service_op::{
    SourceSideHeldOpenForTheRuntimesPublisher, WindowedDestinationOutsideTheSourcesGraph,
    channel_service_name, find_the_source_a_caller_named,
    open_the_source_side_for_the_runtimes_publisher, resolve_channel_sizing,
};
pub use open_iceoryx2_service_op::{close_iceoryx2_service, open_iceoryx2_service};
pub(crate) use prepare_processor_op::prepare_processor;
pub(crate) use reconcile_a_link_reading_another_stream_op::{
    HOW_LONG_A_LINK_READING_ANOTHER_STREAM_WAITS_FOR_ITS_SOURCES_GRAPH,
    HowALinkReadingAnotherStreamWasReconciled,
    reconcile_a_link_reading_another_stream_announcing_its_wiring,
    unwire_a_link_reading_another_stream_from_the_wiring_it_holds,
};
pub(crate) use spawn_processor_op::spawn_processor;
