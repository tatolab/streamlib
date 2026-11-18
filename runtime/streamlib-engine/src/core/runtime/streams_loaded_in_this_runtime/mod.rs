// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The streams this runtime holds: their loads, status, actions and records, live graph edits,
//! steps and what their nodes write.

pub(super) mod function_node_states_carried_across_loads;
pub(super) mod graph_change_listener;
pub(super) mod live_graph_reconciled_to_a_graph_document;
pub(super) mod loaded_stream;
pub(super) mod local_node_type_registration;
pub(super) mod machine_state_directory;
pub(super) mod node_of_a_stream_not_running;
pub(super) mod operations_on_the_streams_loaded_in_this_runtime;
pub(super) mod output_port_viewer;
pub(super) mod project_stream_action_queue;
pub(super) mod status;
pub(super) mod stream_actions_of_this_runtime;
pub(super) mod stream_environment;
pub(super) mod stream_graph_operation_applied_live;
pub(super) mod stream_graph_operation_under_the_current_port_names;
pub(super) mod stream_input_declared_by_a_graph_input;
pub(super) mod stream_location_and_id;
pub(super) mod stream_step_coordinator;
pub(super) mod view_node_output_written_by_a_panel;

#[cfg(test)]
mod graph_input_tests;
