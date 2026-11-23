// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link from a function node into a setting read at setup is carried through the stream's
//! function node pool, read by the engine out of the pool, and its reader set up after the pool.

use std::sync::Arc;

use parking_lot::RwLock;
use serde_json::Value as JsonValue;

use super::nodes_set_up_after_the_sources_of_their_wired_settings::the_next_wave;
use crate::core::compiler::compiler_ops::function_node_island::tests::{
    a_helper_reader_before_its_setup, add_a_node, link,
};
use crate::core::compiler::compiler_ops::function_node_pool::tests::a_graph_resolving_the_pool_and_three_function_node_types;
use crate::core::compiler::compiler_ops::function_node_pool::{
    carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool,
    the_function_node_pool_of, the_links_carried_through_the_function_node_pool_of,
};
use crate::core::compiler::compiler_ops::{
    ReaderOfALinkIntoASettingComponent, open_iceoryx2_service,
};
use crate::core::execution::ProcessExecution;
use crate::core::graph::{
    GraphEdgeWithComponents, GraphNodeWithComponents, NodeReadyBarrierComponent,
    NodeReadyBarrierHandle, NodeRunId, OutOfProcessLinkWiringComponent,
};
use crate::core::nodes::OutOfProcessLinkWiringEnvelope;
use crate::core::test_support::make_in1_a_setting_read_at_setup;
use crate::iceoryx2::Iceoryx2Node;

/// The compile of a link from a function node into a setting read at setup carries it through
/// the pool, the engine reads the half out of the pool into the setting, and the pool sets up in
/// a wave before the setting's node, which is built with what the pool wrote as it set up.
///
/// Mental-revert: leaving the link to its reader carries nothing through the pool, and the
/// reader sets up in the first wave, beside nothing it waits on.
#[test]
fn a_function_node_into_a_setting_is_read_out_of_the_pool_and_its_reader_waits_on_the_pool() {
    let mut graph = a_graph_resolving_the_pool_and_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup(&mut graph);
    make_in1_a_setting_read_at_setup(&mut graph, reader.as_str());
    let into_the_setting = link(&mut graph, (&lfo, "result"), (&reader, "in1"));
    let mut nodes_to_add = vec![lfo, reader.clone()];
    let mut links_to_add = vec![into_the_setting];
    carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
        &mut graph,
        &mut nodes_to_add,
        &mut links_to_add,
    )
    .unwrap();
    let pool = the_function_node_pool_of(&graph).expect("the compile adds the pool");
    graph
        .traversal_mut()
        .v(&pool)
        .first_mut()
        .unwrap()
        .insert_component_without_rendering_it(OutOfProcessLinkWiringComponent(Arc::new(
            OutOfProcessLinkWiringEnvelope::for_a_far_side_driven_in(
                ProcessExecution::Continuous { interval_ms: 10 },
            ),
        )));
    let out_of_the_pool = the_links_carried_through_the_function_node_pool_of(&graph)
        .first()
        .expect("the link is carried through the pool")
        .out_of_the_function_node_pool
        .id
        .clone();

    open_iceoryx2_service(
        &mut graph,
        &out_of_the_pool,
        &Iceoryx2Node::for_this_test_process(),
    )
    .expect("the half out of the pool wires");

    assert!(
        graph
            .traversal()
            .e(&out_of_the_pool)
            .first()
            .unwrap()
            .has::<ReaderOfALinkIntoASettingComponent>(),
        "the engine reads the half out of the pool into the setting"
    );
    let barrier_handle = || NodeReadyBarrierComponent::new().1;
    let (first_wave, after_it) = the_next_wave(
        &RwLock::new(graph),
        vec![
            (reader.clone(), barrier_handle()),
            (pool.clone(), barrier_handle()),
        ],
    );
    let nodes_of = |wave: &[(NodeRunId, NodeReadyBarrierHandle)]| -> Vec<NodeRunId> {
        wave.iter().map(|(node, _)| node.clone()).collect()
    };
    assert_eq!(nodes_of(&first_wave), vec![pool]);
    assert_eq!(nodes_of(&after_it), vec![reader]);
}
