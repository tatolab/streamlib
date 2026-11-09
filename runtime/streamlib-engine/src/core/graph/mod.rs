// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod components;
mod config_node_reference_edges;
mod data_structure;

mod edges;
mod exposed_config_field_input_ports;
mod graph_readiness;
mod nodes;
mod port_family_link_checks;
mod processor_state_ecs_component;
mod traits;
mod traversal;
mod validation;

#[cfg(test)]
mod graph_tests;

// top level
pub(crate) use config_node_reference_edges::{
    config_keys_of_a_node_type_holding_a_node_reference, config_node_reference_edges_of_one_node,
    refuse_a_live_config_naming_a_node_the_graph_does_not_hold,
    refuse_a_reference_to_a_node_of_the_wrong_type,
    refuse_a_second_referrer_of_a_node_that_takes_one,
};
pub use data_structure::{Graph, GraphState};
pub(crate) use data_structure::{
    names_listed_for_a_refusal, node_named_or_refused, node_names_listed_for_a_refusal,
    port_names_listed_for_a_refusal,
};
pub(crate) use exposed_config_field_input_ports::the_input_ports_of_the_exposed_config_fields;
pub use graph_readiness::ObservableGraphReadiness;
pub(crate) use port_family_link_checks::{
    refuse_a_config_update_that_a_wired_link_out_of_the_node_would_refuse,
    refuse_a_link_between_ports_whose_families_do_not_match,
    the_family_a_link_out_of_an_output_carries, the_outputs_a_link_carries_on,
};
pub use processor_state_ecs_component::{ProcessorState, ProcessorStateComponent};
pub(crate) use streamlib_processor_schema::is_in_exposed_name_cast_form;
pub use streamlib_processor_schema::{
    EXPOSED_NAME_MAXIMUM_LENGTH, ExposedNameCastsToNothingError, cast_exposed_name_to_url_safe,
};
pub use traits::{GraphEdgeWithComponents, GraphNodeWithComponents, GraphWeight};
pub use validation::validate_graph;

pub use components::*;
pub use edges::*;
pub use nodes::*;
pub use traversal::*;
