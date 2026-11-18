// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

mod apply_node_config_update_op;
pub(crate) mod cross_floor_check_in_the_projects_interpreter;
pub(crate) mod function_node_island;
pub(crate) mod function_node_pool;
pub(crate) mod function_node_promotion;
pub(crate) mod graph_generator_run_in_the_projects_interpreter;
pub(crate) mod input_port_pin_values;
pub(crate) mod node_interpreter_describe;
pub(crate) mod node_interpreter_shutdown_ladder;
pub(crate) mod node_interpreter_spawn_host;
mod open_iceoryx2_service_op;
mod prepare_node_op;
pub(crate) mod project_node_types_listed_from_source;
pub(crate) mod python_node_declaration;
pub(crate) mod shader_node_island;
pub(crate) mod shader_node_island_program;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod shader_node_native_evaluator;
pub(crate) mod shader_node_program;
mod spawn_node_op;
pub(crate) mod subprocess_bridge;
mod subprocess_escalate;
mod subprocess_escalate_wire_types;
pub(crate) mod unlinked_input_values_delivery;

pub(crate) use apply_node_config_update_op::{NodeConfigUpdateOutcome, apply_node_config_update};
pub(crate) use open_iceoryx2_service_op::{
    SourceSideHeldOpenForAReaderOnTheReservedSlot,
    close_iceoryx2_service_into_the_host_of_a_shader_node_island,
    open_iceoryx2_service_into_the_host_of_a_shader_node_island,
    open_the_source_side_for_a_reader_on_the_reserved_slot, refuse_a_link_a_channel_cannot_carry,
};
pub(crate) use open_iceoryx2_service_op::{close_iceoryx2_service, open_iceoryx2_service};
pub(crate) use prepare_node_op::prepare_node;
pub(crate) use spawn_node_op::spawn_node;
