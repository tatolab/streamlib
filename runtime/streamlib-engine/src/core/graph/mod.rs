// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

mod components;
mod data_structure;

mod edges;
mod graph_readiness;
mod nodes;
mod output_port_exposure;
mod processor_state_ecs_component;
mod traits;
mod traversal;
mod validation;

#[cfg(test)]
mod graph_tests;

// top level
pub(crate) use data_structure::node_names_listed_for_a_refusal;
pub use data_structure::{Graph, GraphState};
pub use graph_readiness::ObservableGraphReadiness;
pub use output_port_exposure::{
    OutputPortExposureLevel, OutputPortReaderLocation, output_port_exposure_allows_the_reader,
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
