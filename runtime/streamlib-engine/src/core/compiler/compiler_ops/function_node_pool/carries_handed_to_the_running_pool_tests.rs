// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use parking_lot::RwLock;

use super::carries_handed_to_the_running_pool::hand_the_running_function_node_pool_its_carries;
use super::tests::{
    a_graph_carrying_an_lfo_into_its_reader_through_the_pool, a_reader_in_the_app_process,
};
use super::{
    carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool,
    the_function_node_pool_of,
};
use crate::core::compiler::compiler_ops::function_node_island::tests::link;

#[test]
fn a_running_pool_that_cannot_take_its_carries_is_put_back_as_it_was() {
    let (mut graph, lfo, _, _) = a_graph_carrying_an_lfo_into_its_reader_through_the_pool();
    let pool = the_function_node_pool_of(&graph).expect("the stream has a pool");
    let (ports_before, config_before) = {
        let pool = graph.traversal().v(&pool).first().unwrap();
        (pool.ports.clone(), pool.config.clone())
    };
    let links_before = graph.traversal().e(()).iter().count();
    let late_reader = a_reader_in_the_app_process(&mut graph);
    let late = link(&mut graph, (&lfo, "result"), (&late_reader, "in1"));
    let carries = carry_each_island_its_reader_cannot_evaluate_through_the_function_node_pool(
        &mut graph,
        &mut vec![late_reader],
        &mut vec![late],
    )
    .unwrap()
    .expect("the pool runs, so its carries are handed to its helper");
    let graph = RwLock::new(graph);

    let refusal = hand_the_running_function_node_pool_its_carries(&graph, &carries)
        .expect_err("a pool with no instance cannot take its carries");

    assert!(refusal.to_string().contains("has no instance"), "{refusal}");
    let graph = graph.read();
    let pool_after = graph.traversal().v(&pool).first().unwrap();
    assert_eq!(
        pool_after.ports, ports_before,
        "the pool's ports are put back"
    );
    assert_eq!(pool_after.config, config_before, "and so is its config");
    assert_eq!(
        graph.traversal().e(()).iter().count(),
        links_before,
        "no link through the pool's new ports is left behind"
    );
}
