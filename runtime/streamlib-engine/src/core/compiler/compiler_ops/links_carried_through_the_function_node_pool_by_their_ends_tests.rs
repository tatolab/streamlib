// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::function_node_island::tests::link;
use super::function_node_pool::tests::a_graph_carrying_an_lfo_into_its_reader_through_the_pool;
use super::function_node_pool::{the_function_node_pool_of, the_links_a_link_is_wired_as};
use super::links_carried_through_the_function_node_pool_by_their_ends::{
    the_link_carried_through_the_function_node_pool_between, the_links_a_disconnect_takes_out,
};
use crate::core::graph::{GraphEdgeWithComponents, PendingDeletionComponent};

#[test]
fn a_carried_link_is_found_by_the_ends_it_was_given_with() {
    let (graph, lfo, reader, lfo_into_the_reader) =
        a_graph_carrying_an_lfo_into_its_reader_through_the_pool();

    assert_eq!(
        the_link_carried_through_the_function_node_pool_between(
            &graph, &lfo, "result", &reader, "in1"
        ),
        Some(lfo_into_the_reader)
    );
    assert_eq!(
        the_link_carried_through_the_function_node_pool_between(
            &graph, &lfo, "result", &reader, "in2"
        ),
        None
    );
    assert_eq!(
        the_link_carried_through_the_function_node_pool_between(
            &graph, &reader, "result", &lfo, "in1"
        ),
        None
    );
}

#[test]
fn a_carried_link_a_disconnect_has_marked_is_no_longer_found() {
    let (mut graph, lfo, reader, lfo_into_the_reader) =
        a_graph_carrying_an_lfo_into_its_reader_through_the_pool();
    let into_the_pool = the_links_a_link_is_wired_as(&graph, &lfo_into_the_reader)[0].clone();
    graph
        .traversal_mut()
        .e(&into_the_pool)
        .first_mut()
        .unwrap()
        .insert(PendingDeletionComponent);

    assert_eq!(
        the_link_carried_through_the_function_node_pool_between(
            &graph, &lfo, "result", &reader, "in1"
        ),
        None
    );
}

#[test]
fn a_disconnect_of_a_carried_link_takes_out_both_halves_and_answers_the_ends_it_was_given() {
    let (graph, lfo, reader, lfo_into_the_reader) =
        a_graph_carrying_an_lfo_into_its_reader_through_the_pool();
    let pool = the_function_node_pool_of(&graph).expect("the stream has a pool");

    let (source, target, taken_out) =
        the_links_a_disconnect_takes_out(&graph, &lfo_into_the_reader).unwrap();

    assert_eq!(source.node_run_id(), &lfo);
    assert_eq!(source.port_name(), "result");
    assert_eq!(target.node_run_id(), &reader);
    assert_eq!(target.port_name(), "in1");
    let mut wired_as = the_links_a_link_is_wired_as(&graph, &lfo_into_the_reader);
    let mut taken_out_sorted = taken_out.clone();
    wired_as.sort();
    taken_out_sorted.sort();
    assert_eq!(taken_out_sorted, wired_as);
    assert!(taken_out.iter().all(|half| {
        graph.traversal().e(half).first().is_some_and(|link| {
            link.source().node_run_id() == &pool || link.to_port().node_run_id() == &pool
        })
    }));
}

#[test]
fn a_disconnect_of_a_link_the_graph_holds_takes_out_that_link_alone() {
    let (mut graph, lfo, reader, _) = a_graph_carrying_an_lfo_into_its_reader_through_the_pool();
    let direct = link(&mut graph, (&lfo, "result"), (&reader, "in2"));

    let (source, target, taken_out) = the_links_a_disconnect_takes_out(&graph, &direct).unwrap();

    assert_eq!(source.node_run_id(), &lfo);
    assert_eq!(target.port_name(), "in2");
    assert_eq!(taken_out, vec![direct]);
}

#[test]
fn a_disconnect_of_a_link_the_graph_does_not_hold_is_refused() {
    let (graph, _, _, _) = a_graph_carrying_an_lfo_into_its_reader_through_the_pool();
    let pool = the_function_node_pool_of(&graph).unwrap();
    let unheld = crate::core::graph::LinkUniqueId::from(format!("{pool}-no-such-link"));

    assert!(the_links_a_disconnect_takes_out(&graph, &unheld).is_err());
}
