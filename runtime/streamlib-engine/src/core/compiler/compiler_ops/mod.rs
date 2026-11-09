// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod apply_processor_config_update_op;
pub(crate) mod cross_floor_check_in_the_projects_interpreter;
pub(crate) mod function_node_island;
pub(crate) mod function_node_pool;
pub(crate) mod function_node_promotion;
pub(crate) mod graph_generator_run_in_the_projects_interpreter;
pub(crate) mod input_port_pin_values;
mod open_iceoryx2_service_op;
mod prepare_processor_op;
pub(crate) mod processor_interpreter_describe;
pub(crate) mod processor_interpreter_shutdown_ladder;
pub(crate) mod processor_interpreter_spawn_host;
pub(crate) mod project_node_types_listed_from_source;
pub(crate) mod python_processor_declaration;
mod spawn_processor_op;
pub(crate) mod subprocess_bridge;
mod subprocess_escalate;
mod subprocess_escalate_wire_types;

pub(crate) use apply_processor_config_update_op::{
    ProcessorConfigUpdateOutcome, apply_processor_config_update,
};
pub(crate) use open_iceoryx2_service_op::{
    SourceSideHeldOpenForAReaderOnTheReservedSlot,
    open_the_source_side_for_a_reader_on_the_reserved_slot,
};
pub use open_iceoryx2_service_op::{close_iceoryx2_service, open_iceoryx2_service};
pub(crate) use prepare_processor_op::prepare_processor;
pub(crate) use spawn_processor_op::spawn_processor;
