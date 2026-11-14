// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde_json::Value as JsonValue;

use super::*;
use crate::core::compiler::compiler_ops::function_node_island::tests::{
    a_class_node_feeding_remap_into_a_helper_reader, a_function_node_type,
    a_graph_resolving_these_function_node_types, a_graph_resolving_three_function_node_types,
    a_helper_reader_before_its_setup, a_helper_reader_before_its_setup_driven_in, add_a_node, link,
    reader_setup_command_ports,
};
use crate::core::compiler::compiler_ops::function_node_island::{
    HowALinkIsWired, how_a_link_is_wired, wire_a_link_from_a_function_node_island_into_its_reader,
};
use crate::core::graph::{
    FunctionNodeRanCompiledComponent, HowItsHelpersRanAFunctionNode, JsonSerializableComponent,
};

const NOTHING_REMOVED: WhatTheCompileCarryingAPromotionRemoves<'static> =
    WhatTheCompileCarryingAPromotionRemoves {
        processors: &[],
        links: &[],
    };

fn the_link_state_of(graph: &Graph, link_id: &LinkUniqueId) -> Option<LinkState> {
    graph
        .traversal()
        .e(link_id)
        .first()
        .and_then(|link| link.get::<LinkStateComponent>())
        .map(|state| state.0)
}

fn the_process_execution_promoted_to(
    graph: &Graph,
    node_id: &ProcessorUniqueId,
) -> Option<ProcessExecution> {
    graph
        .traversal()
        .v(node_id)
        .first()
        .and_then(|node| node.get::<FunctionNodePromotedToItsOwnProcessComponent>())
        .map(|promotion| promotion.process_execution)
}

/// `lfo` feeds `remap`, which feeds a reader's helper as one island.
struct OneIslandIntoOneReader {
    graph: Graph,
    lfo: ProcessorUniqueId,
    remap: ProcessorUniqueId,
    reader: ProcessorUniqueId,
    lfo_into_remap: LinkUniqueId,
    remap_into_the_reader: LinkUniqueId,
}

fn one_island_into_one_reader_driven_in(
    reader_process_execution: ProcessExecution,
) -> OneIslandIntoOneReader {
    let mut graph = a_graph_resolving_three_function_node_types();
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let reader = a_helper_reader_before_its_setup_driven_in(&mut graph, reader_process_execution);
    let lfo_into_remap = link(&mut graph, (&lfo, "result"), (&remap, "x"));
    let remap_into_the_reader = link(&mut graph, (&remap, "result"), (&reader, "in1"));
    if reader_process_execution == ProcessExecution::Reactive {
        let source_that_wakes_the_reader = add_a_node(
            &mut graph,
            crate::core::test_support::MockOutputOnlyProcessor::processor_class_import_path()
                .as_str(),
            JsonValue::Null,
        );
        link(
            &mut graph,
            (&source_that_wakes_the_reader, "out1"),
            (&reader, "in1"),
        );
    }
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &remap_into_the_reader,
        &crate::iceoryx2::Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();
    OneIslandIntoOneReader {
        graph,
        lfo,
        remap,
        reader,
        lfo_into_remap,
        remap_into_the_reader,
    }
}

#[test]
fn a_crashed_function_node_leaves_its_readers_island_and_what_it_reads_becomes_its_own_island() {
    let OneIslandIntoOneReader {
        mut graph,
        lfo,
        remap,
        reader,
        lfo_into_remap,
        remap_into_the_reader,
    } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 25 });
    assert_eq!(
        reader_setup_command_ports(&graph, &reader)["islands"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    let planned = plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
        &mut graph,
        &remap,
        &reader,
        "function node `remap` crashed its helper process during its call.",
        &NOTHING_REMOVED,
    )
    .unwrap();

    assert_eq!(planned.processors_to_add, vec![remap.clone()]);
    assert_eq!(
        planned.links_to_add.iter().collect::<HashSet<_>>(),
        HashSet::from([&lfo_into_remap, &remap_into_the_reader])
    );
    assert_eq!(
        reader_setup_command_ports(&graph, &reader)["islands"],
        serde_json::json!([]),
        "the reader's next helper evaluates no island"
    );
    assert_eq!(
        the_process_execution_promoted_to(&graph, &remap),
        Some(ProcessExecution::Continuous { interval_ms: 25 }),
        "the promoted node runs at its reader's frame interval"
    );
    assert!(!runs_inside_the_read_of_each_node_it_feeds(&graph, &remap));
    assert!(runs_inside_the_read_of_each_node_it_feeds(&graph, &lfo));
    assert_eq!(
        how_a_link_is_wired(&graph, &lfo_into_remap).unwrap(),
        HowALinkIsWired::AsTheIslandItsReaderEvaluatesAtEachRead
    );
    assert_eq!(
        how_a_link_is_wired(&graph, &remap_into_the_reader).unwrap(),
        HowALinkIsWired::ThroughAChannel
    );
    for link_id in [&lfo_into_remap, &remap_into_the_reader] {
        assert_eq!(the_link_state_of(&graph, link_id), Some(LinkState::Pending));
    }
}

#[test]
fn a_promoted_function_node_reading_a_class_node_reads_its_channel_in_its_own_helper() {
    let mut graph = a_graph_resolving_three_function_node_types();
    let (remap, reader, into_the_island, into_the_reader) =
        a_class_node_feeding_remap_into_a_helper_reader(&mut graph);
    wire_a_link_from_a_function_node_island_into_its_reader(
        &mut graph,
        &into_the_reader,
        &crate::iceoryx2::Iceoryx2Node::for_this_test_process(),
    )
    .unwrap();
    assert!(
        graph
            .traversal()
            .e(&into_the_island)
            .first()
            .unwrap()
            .has::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>()
    );

    let planned = plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
        &mut graph,
        &remap,
        &reader,
        "function node `remap` crashed its helper process during its call.",
        &NOTHING_REMOVED,
    )
    .expect("a function node reading a class node is promoted");

    assert!(!runs_inside_the_read_of_each_node_it_feeds(&graph, &remap));
    assert!(planned.links_to_add.contains(&into_the_island));
    assert!(
        !graph
            .traversal()
            .e(&into_the_island)
            .first()
            .unwrap()
            .has::<LinkReadThroughTheReaderOfItsFunctionNodeIslandComponent>(),
        "the read its old reader held is closed"
    );
    assert_eq!(
        the_link_state_of(&graph, &into_the_island),
        Some(LinkState::Pending)
    );
    assert_eq!(
        how_a_link_is_wired(&graph, &into_the_island).unwrap(),
        HowALinkIsWired::ThroughAChannel,
        "the class node's channel reaches the promoted node's own helper"
    );
}

#[test]
fn every_function_node_downstream_of_the_crashed_one_moves_with_it() {
    let OneIslandIntoOneReader {
        mut graph,
        lfo,
        remap,
        reader,
        ..
    } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 10 });

    let planned = plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
        &mut graph,
        &lfo,
        &reader,
        "function node `lfo` stalled.",
        &NOTHING_REMOVED,
    )
    .unwrap();

    assert_eq!(
        planned.processors_to_add.iter().collect::<HashSet<_>>(),
        HashSet::from([&lfo, &remap])
    );
    assert!(!runs_inside_the_read_of_each_node_it_feeds(&graph, &lfo));
    assert!(!runs_inside_the_read_of_each_node_it_feeds(&graph, &remap));
}

#[test]
fn a_promotion_out_of_a_reader_with_no_frame_interval_runs_at_the_default_interval() {
    let OneIslandIntoOneReader {
        mut graph,
        remap,
        reader,
        ..
    } = one_island_into_one_reader_driven_in(ProcessExecution::Reactive);

    plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
        &mut graph,
        &remap,
        &reader,
        "function node `remap` stalled.",
        &NOTHING_REMOVED,
    )
    .unwrap();

    assert_eq!(
        the_process_execution_promoted_to(&graph, &remap),
        Some(ProcessExecution::Continuous {
            interval_ms: PROMOTED_FUNCTION_NODE_INTERVAL_MS_FOR_A_READER_WITH_NONE
        })
    );
}

#[test]
fn a_promotion_that_would_re_island_a_second_running_reader_is_refused_changing_nothing() {
    let OneIslandIntoOneReader {
        mut graph,
        remap,
        reader,
        remap_into_the_reader,
        ..
    } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 10 });
    let second_reader = a_helper_reader_before_its_setup_driven_in(
        &mut graph,
        ProcessExecution::Continuous { interval_ms: 10 },
    );
    link(&mut graph, (&remap, "result"), (&second_reader, "in1"));

    let refusal = plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
        &mut graph,
        &remap,
        &reader,
        "function node `remap` stalled.",
        &NOTHING_REMOVED,
    )
    .unwrap_err()
    .to_string();

    let second_reader_name = the_name_in_the_graph_of(&graph, &second_reader);
    assert!(
        refusal.contains(&format!("`{second_reader_name}`")),
        "{refusal}"
    );
    assert!(runs_inside_the_read_of_each_node_it_feeds(&graph, &remap));
    assert_eq!(
        reader_setup_command_ports(&graph, &reader)["islands"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        the_link_state_of(&graph, &remap_into_the_reader),
        Some(LinkState::Wired)
    );
}

#[test]
fn a_promotion_whose_link_the_same_compile_removes_is_refused_changing_nothing() {
    let OneIslandIntoOneReader {
        mut graph,
        lfo,
        reader,
        lfo_into_remap,
        remap_into_the_reader,
        ..
    } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 10 });

    let refusal = plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
        &mut graph,
        &lfo,
        &reader,
        "function node `lfo` stalled.",
        &WhatTheCompileCarryingAPromotionRemoves {
            processors: &[],
            links: std::slice::from_ref(&lfo_into_remap),
        },
    )
    .unwrap_err()
    .to_string();

    assert!(refusal.contains("the same change removes"), "{refusal}");
    assert!(runs_inside_the_read_of_each_node_it_feeds(&graph, &lfo));
    assert_eq!(
        the_link_state_of(&graph, &remap_into_the_reader),
        Some(LinkState::Wired)
    );
}

#[test]
fn a_link_into_a_promoted_function_node_from_a_node_that_is_not_one_is_a_channel() {
    let OneIslandIntoOneReader {
        mut graph,
        remap,
        reader,
        ..
    } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 10 });
    plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
        &mut graph,
        &remap,
        &reader,
        "function node `remap` stalled.",
        &NOTHING_REMOVED,
    )
    .unwrap();
    let process_node = add_a_node(
        &mut graph,
        crate::core::test_support::MockOutputOnlyProcessor::processor_class_import_path().as_str(),
        JsonValue::Null,
    );

    let into_the_promoted_node = link(&mut graph, (&process_node, "out1"), (&remap, "x"));

    assert_eq!(
        how_a_link_is_wired(&graph, &into_the_promoted_node).unwrap(),
        HowALinkIsWired::ThroughAChannel
    );
}

#[test]
fn a_function_node_already_out_of_its_readers_islands_is_not_promoted_again() {
    let OneIslandIntoOneReader {
        mut graph,
        remap,
        reader,
        ..
    } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 10 });
    plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
        &mut graph,
        &remap,
        &reader,
        "first",
        &NOTHING_REMOVED,
    )
    .unwrap();

    let refusal = plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
        &mut graph,
        &remap,
        &reader,
        "second",
        &NOTHING_REMOVED,
    )
    .unwrap_err()
    .to_string();

    assert!(
        refusal.contains("is not a function node inside"),
        "{refusal}"
    );
}

#[test]
fn graph_shows_each_promotion_under_the_promoted_nodes_metrics() {
    let OneIslandIntoOneReader {
        mut graph,
        lfo,
        remap,
        reader,
        ..
    } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 10 });
    let planned = plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
        &mut graph,
        &lfo,
        &reader,
        "function node `lfo` stalled.",
        &NOTHING_REMOVED,
    )
    .unwrap();

    record_each_promotion_on_its_nodes_metrics(&mut graph, &planned);

    for promoted_id in [&lfo, &remap] {
        let metrics = graph
            .traversal()
            .v(promoted_id)
            .first()
            .and_then(|node| node.get::<ProcessorMetrics>())
            .map(JsonSerializableComponent::to_json)
            .unwrap();
        assert_eq!(
            metrics["promoted_out_of_its_readers_islands"],
            serde_json::json!({
                "promotions": 1,
                "after_crashing_the_helper_of": the_name_in_the_graph_of(&graph, &reader),
                "reason": "function node `lfo` stalled.",
            }),
            "{metrics}"
        );
    }
}

/// A graph resolving `lfo`, `remap` and `mix` with no state, and `counter`
/// and `second_counter`, which carry one.
fn a_graph_resolving_two_stateful_function_node_types() -> Graph {
    let stateful = |import_path| {
        let mut descriptor = a_function_node_type(import_path, &[], &["result"]);
        descriptor.function_node_state_fields = vec!["calls".to_string()];
        descriptor
    };
    a_graph_resolving_these_function_node_types([
        a_function_node_type("test_ops:lfo", &[], &["result"]),
        a_function_node_type("test_ops:remap", &["x"], &["result"]),
        a_function_node_type("test_ops:mix", &["a", "b"], &["result"]),
        stateful("test_ops:counter"),
        stateful("test_ops:second_counter"),
    ])
}

fn every_node_of(graph: &Graph) -> Vec<ProcessorUniqueId> {
    graph.traversal().v(()).ids()
}

#[test]
fn a_stateful_node_two_readers_read_moves_out_of_the_islands_with_what_it_feeds() {
    let mut graph = a_graph_resolving_two_stateful_function_node_types();
    let counter = add_a_node(&mut graph, "test_ops:counter", JsonValue::Null);
    let remap = add_a_node(&mut graph, "test_ops:remap", JsonValue::Null);
    let first_reader = a_helper_reader_before_its_setup(&mut graph);
    let second_reader = a_helper_reader_before_its_setup(&mut graph);
    let counter_into_the_first_reader =
        link(&mut graph, (&counter, "result"), (&first_reader, "in1"));
    link(&mut graph, (&counter, "result"), (&remap, "x"));
    link(&mut graph, (&remap, "result"), (&second_reader, "in1"));
    let added = every_node_of(&graph);

    place_each_stateful_function_node_several_readers_read_in_a_process_of_its_own(
        &mut graph,
        &added,
        &[],
    )
    .unwrap();

    for moved in [&counter, &remap] {
        assert_eq!(
            the_process_execution_promoted_to(&graph, moved),
            Some(ProcessExecution::Continuous {
                interval_ms: STATEFUL_FUNCTION_NODE_SEVERAL_READERS_READ_INTERVAL_MS
            })
        );
    }
    assert_eq!(
        how_a_link_is_wired(&graph, &counter_into_the_first_reader).unwrap(),
        HowALinkIsWired::ThroughAChannel
    );
}

fn the_ran_compiled_graph_renders_on(graph: &Graph, node_id: &ProcessorUniqueId) -> JsonValue {
    graph
        .traversal()
        .v(node_id)
        .first()
        .and_then(|node| node.get::<FunctionNodeRanCompiledComponent>())
        .map(|component| component.to_json())
        .expect("a promoted function node carries the report `graph` renders")
}

fn the_report_its_own_helper_notes_on(
    graph: &Graph,
    node_id: &ProcessorUniqueId,
) -> std::sync::Arc<HowItsHelpersRanAFunctionNode> {
    graph
        .traversal()
        .v(node_id)
        .first()
        .and_then(|node| node.get::<FunctionNodePromotedToItsOwnProcessComponent>())
        .map(|promotion| std::sync::Arc::clone(&promotion.how_its_own_helper_ran_it))
        .expect("the node was promoted")
}

/// A stateful node several readers read is evaluated by no reader, so the
/// helper of its own is what reports how it ran it.
///
/// Fail-without-fix: the node carries no report its own helper notes on,
/// and `graph` shows `ran_compiled: null` for as long as it runs.
#[test]
fn a_stateful_node_two_readers_read_shows_how_its_own_helper_ran_it() {
    let mut graph = a_graph_resolving_two_stateful_function_node_types();
    let counter = add_a_node(&mut graph, "test_ops:counter", JsonValue::Null);
    let first_reader = a_helper_reader_before_its_setup(&mut graph);
    let second_reader = a_helper_reader_before_its_setup(&mut graph);
    link(&mut graph, (&counter, "result"), (&first_reader, "in1"));
    link(&mut graph, (&counter, "result"), (&second_reader, "in1"));
    let added = every_node_of(&graph);

    place_each_stateful_function_node_several_readers_read_in_a_process_of_its_own(
        &mut graph,
        &added,
        &[],
    )
    .unwrap();

    assert_eq!(
        the_ran_compiled_graph_renders_on(&graph, &counter),
        JsonValue::Null,
        "null until its own helper has set up"
    );
    the_report_its_own_helper_notes_on(&graph, &counter).note_it_ran_compiled(true);
    assert_eq!(
        the_ran_compiled_graph_renders_on(&graph, &counter),
        JsonValue::Bool(true)
    );
}

/// A node promoted after a crash shows how its reader ran it until its own
/// helper reports, then how its own helper runs it.
#[test]
fn a_node_promoted_out_of_its_reader_shows_how_its_own_helper_runs_it() {
    let OneIslandIntoOneReader {
        mut graph,
        remap,
        reader,
        ..
    } = one_island_into_one_reader_driven_in(ProcessExecution::Continuous { interval_ms: 25 });
    graph
        .traversal()
        .v(&remap)
        .first()
        .and_then(|node| node.get::<FunctionNodeRanCompiledComponent>())
        .expect("its reader's island attached the report")
        .0
        .note_it_ran_compiled(false);

    plan_the_promotion_of_a_function_node_out_of_its_readers_islands(
        &mut graph,
        &remap,
        &reader,
        "function node `remap` crashed its helper process during its call.",
        &NOTHING_REMOVED,
    )
    .unwrap();

    assert_eq!(
        the_ran_compiled_graph_renders_on(&graph, &remap),
        JsonValue::Bool(false),
        "how its reader ran it, until its own helper reports"
    );
    the_report_its_own_helper_notes_on(&graph, &remap).note_it_ran_compiled(true);
    assert_eq!(
        the_ran_compiled_graph_renders_on(&graph, &remap),
        JsonValue::Bool(true)
    );
}

#[test]
fn a_stateful_node_one_reader_reads_and_a_stateless_one_two_read_stay_inline() {
    let mut graph = a_graph_resolving_two_stateful_function_node_types();
    let counter = add_a_node(&mut graph, "test_ops:counter", JsonValue::Null);
    let lfo = add_a_node(&mut graph, "test_ops:lfo", JsonValue::Null);
    let counters_reader = a_helper_reader_before_its_setup(&mut graph);
    let first_lfo_reader = a_helper_reader_before_its_setup(&mut graph);
    let second_lfo_reader = a_helper_reader_before_its_setup(&mut graph);
    link(&mut graph, (&counter, "result"), (&counters_reader, "in1"));
    link(&mut graph, (&lfo, "result"), (&first_lfo_reader, "in1"));
    link(&mut graph, (&lfo, "result"), (&second_lfo_reader, "in1"));
    let added = every_node_of(&graph);

    place_each_stateful_function_node_several_readers_read_in_a_process_of_its_own(
        &mut graph,
        &added,
        &[],
    )
    .unwrap();

    assert!(runs_inside_the_read_of_each_node_it_feeds(&graph, &counter));
    assert!(runs_inside_the_read_of_each_node_it_feeds(&graph, &lfo));
}

#[test]
fn a_reader_removed_in_the_same_compile_does_not_count_as_one_of_its_readers() {
    let mut graph = a_graph_resolving_two_stateful_function_node_types();
    let counter = add_a_node(&mut graph, "test_ops:counter", JsonValue::Null);
    let kept_reader = a_helper_reader_before_its_setup(&mut graph);
    let removed_reader = a_helper_reader_before_its_setup(&mut graph);
    link(&mut graph, (&counter, "result"), (&kept_reader, "in1"));
    link(&mut graph, (&counter, "result"), (&removed_reader, "in1"));

    place_each_stateful_function_node_several_readers_read_in_a_process_of_its_own(
        &mut graph,
        &[counter.clone(), kept_reader],
        &[removed_reader],
    )
    .unwrap();

    assert!(runs_inside_the_read_of_each_node_it_feeds(&graph, &counter));
}

#[test]
fn a_stateful_node_that_reads_a_moved_one_beside_another_reader_moves_too() {
    let mut graph = a_graph_resolving_two_stateful_function_node_types();
    let counter = add_a_node(&mut graph, "test_ops:counter", JsonValue::Null);
    let second_counter = add_a_node(&mut graph, "test_ops:second_counter", JsonValue::Null);
    let mix = add_a_node(&mut graph, "test_ops:mix", JsonValue::Null);
    let mix_reader = a_helper_reader_before_its_setup(&mut graph);
    let counter_reader = a_helper_reader_before_its_setup(&mut graph);
    let second_counter_reader = a_helper_reader_before_its_setup(&mut graph);
    link(&mut graph, (&counter, "result"), (&mix, "a"));
    link(&mut graph, (&second_counter, "result"), (&mix, "b"));
    link(&mut graph, (&mix, "result"), (&mix_reader, "in1"));
    link(&mut graph, (&counter, "result"), (&counter_reader, "in1"));
    link(
        &mut graph,
        (&second_counter, "result"),
        (&second_counter_reader, "in1"),
    );
    let added = every_node_of(&graph);

    place_each_stateful_function_node_several_readers_read_in_a_process_of_its_own(
        &mut graph,
        &added,
        &[],
    )
    .unwrap();

    for moved in [&counter, &mix, &second_counter] {
        assert!(
            !runs_inside_the_read_of_each_node_it_feeds(&graph, moved),
            "`mix` moved with `counter`, so `second_counter` is read by `mix`'s helper and \
                 its own reader"
        );
    }
}

#[test]
fn a_second_reader_of_a_stateful_node_a_running_reader_evaluates_is_refused_naming_it() {
    let mut graph = a_graph_resolving_two_stateful_function_node_types();
    let counter = add_a_node(&mut graph, "test_ops:counter", JsonValue::Null);
    let running_reader = a_helper_reader_before_its_setup(&mut graph);
    let new_reader = a_helper_reader_before_its_setup(&mut graph);
    link(&mut graph, (&counter, "result"), (&running_reader, "in1"));
    link(&mut graph, (&counter, "result"), (&new_reader, "in1"));
    let running_reader_name = the_name_in_the_graph_of(&graph, &running_reader);

    let refusal = place_each_stateful_function_node_several_readers_read_in_a_process_of_its_own(
        &mut graph,
        &[counter.clone(), new_reader],
        &[],
    )
    .unwrap_err()
    .to_string();

    assert!(
        refusal.contains(&format!("`{running_reader_name}` is already running")),
        "{refusal}"
    );
    assert!(refusal.contains("re-islanded"), "{refusal}");
    assert!(runs_inside_the_read_of_each_node_it_feeds(&graph, &counter));
}
