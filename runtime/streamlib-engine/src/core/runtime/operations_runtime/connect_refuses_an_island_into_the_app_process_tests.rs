// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `connect` refuses, on a stream no commit builds, each link the engine cannot wire, in the
//! words its wiring would refuse it in at play, so the link is never in the graph a stopped
//! stream saves: among them a link from a function node into a node that runs in the app process
//! and that the stream's function node pool does not evaluate. A saved graph holding such a link
//! sets it aside, made nowhere, so the graph opens and plays without it. One test here covers
//! each refusal a commit returns on a fresh start for a link the graph holds; the refusals a
//! start leaves to one node are covered where that node is: a function node input no value
//! reaches (`input_port_pin_values`) and shader nodes reading their own Image in a cycle
//! (`shader_node_island::tests`).

use crate::core::descriptors::{PinDeclaration, PinType};
use std::sync::Arc;

use serde_json::Value;

use super::connect_on_a_test_thread::{
    connect_a_link_of_a_saved_graph_on_this_thread, connect_on_this_thread, the_links_in,
};
use super::{
    ALinkConnectedOrSetAside, ARemoval, refuse_a_removal_that_strands_a_reactive_reader,
    the_verdict_connect_takes_on_a_link,
};
use crate::core::Error;
use crate::core::compiler::Compiler;
use crate::core::compiler::compiler_ops::function_node_island::the_island_links_into_a_reactive_reader_only_islands_feed;
use crate::core::descriptors::{AudioWindowContract, FunctionNodeLanguage, NodeClassImportPath, NodeClassShortName, NodeDescriptor, NodeKind, PortDescriptor};
use crate::core::execution::ProcessExecution;
use crate::core::graph::input_values::LiveInputValuesOfAStream;
use crate::core::graph::{InputLinkPortRef, NodeRunId, OutputLinkPortRef};
use crate::core::nodes::{HowANodeOfADescribedTypeRuns, NodeSpec, NodeTypesOneStreamResolves};
use crate::core::test_support::{
    MockInputOnlyNode, a_view_of_a_project_reading_records, ensure_test_mocks_registered,
};

const FUNCTION_NODE: &str = "app_process_check:switch";
const HELPER_PROCESS_READER: &str = "app_process_check:meter";
const HELPER_PROCESS_WINDOWED_READER: &str = "app_process_check:windowed_meter";
const HELPER_PROCESS_REACTIVE_READER: &str = "app_process_check:reactive_meter";
const HELPER_PROCESS_SOURCE: &str = "app_process_check:source";
const HELPER_PROCESS_MANUAL_READER: &str = "app_process_check:manual_meter";
const VIEW_READER: &str = "app_process_check:program";

fn a_records_port(name: &str) -> PortDescriptor {
    PortDescriptor::new(name, "", true, PinDeclaration::exactly(PinType::String {}))
    .with_delivery_profile("newest")
}

fn a_described_node_type(
    import_path: &str,
    node_kind: NodeKind,
    inputs: &[&str],
    outputs: &[&str],
) -> NodeDescriptor {
    let import_path = NodeClassImportPath::new(import_path).unwrap();
    let short_name = import_path.as_str().rsplit(':').next().unwrap().to_owned();
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short_name).unwrap(),
        import_path,
        "a fixture",
    );
    descriptor.function_node_runs_as_python = matches!(node_kind, NodeKind::Function { .. });
    descriptor.node_kind = node_kind;
    descriptor.inputs = inputs.iter().map(|name| a_records_port(name)).collect();
    descriptor.outputs = outputs.iter().map(|name| a_records_port(name)).collect();
    descriptor
}

/// A compiler of a stopped stream resolving a function node type, `switch`, a type its node code
/// runs in a helper process, `meter`, and a view of its project, `program`, beside the natively
/// compiled mocks.
fn a_stopped_stream_resolving_a_function_node_type() -> Arc<Compiler> {
    ensure_test_mocks_registered();
    let node_types = NodeTypesOneStreamResolves::new();
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            a_described_node_type(
                FUNCTION_NODE,
                NodeKind::Function {
                    language: FunctionNodeLanguage::Python,
                },
                &["x"],
                &["result"],
            ),
            HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                node_once_promoted_to_its_own_process: Box::new(|_node| {
                    Err(Error::NotSupported("a fixture runs no node".to_owned()))
                }),
            },
        )
        .unwrap();
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            a_described_node_type(HELPER_PROCESS_READER, NodeKind::Process, &["in1"], &[]),
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(Box::new(|_node| {
                Err(Error::NotSupported("a fixture runs no node".to_owned()))
            })),
        )
        .unwrap();
    let mut windowed_reader = a_described_node_type(
        HELPER_PROCESS_WINDOWED_READER,
        NodeKind::Process,
        &["audio"],
        &[],
    );
    windowed_reader.inputs[0].audio_window = Some(AudioWindowContract::MatchDevice {});
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            windowed_reader,
            HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(Box::new(|_node| {
                Err(Error::NotSupported("a fixture runs no node".to_owned()))
            })),
        )
        .unwrap();
    for (import_path, inputs, outputs) in [
        (HELPER_PROCESS_REACTIVE_READER, &["in1", "in2"][..], &[][..]),
        (HELPER_PROCESS_SOURCE, &[][..], &["out"][..]),
        (HELPER_PROCESS_MANUAL_READER, &["in1"][..], &[][..]),
    ] {
        node_types
            .register_a_type_described_in_this_streams_node_interpreter(
                a_described_node_type(import_path, NodeKind::Process, inputs, outputs),
                HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(Box::new(|_node| {
                    Err(Error::NotSupported("a fixture runs no node".to_owned()))
                })),
            )
            .unwrap();
    }
    node_types.record_the_execution_a_described_class_declares(
        NodeClassImportPath::new(HELPER_PROCESS_REACTIVE_READER).unwrap(),
        ProcessExecution::Reactive,
    );
    node_types.record_the_execution_a_described_class_declares(
        NodeClassImportPath::new(HELPER_PROCESS_MANUAL_READER).unwrap(),
        ProcessExecution::Manual,
    );
    node_types.replace_the_view_types_this_streams_project_brings(&[
        a_view_of_a_project_reading_records(VIEW_READER),
    ]);
    assert!(
        node_types.is_a_view_type_of_this_streams_project(
            &NodeClassImportPath::new(VIEW_READER).unwrap()
        ),
        "the fixture's view is a node type"
    );
    Arc::new(Compiler::new_of_a_stream(
        Arc::new(node_types),
        LiveInputValuesOfAStream::default(),
    ))
}

fn a_node_of(compiler: &Compiler, node_type: &str) -> NodeRunId {
    compiler.scope(|graph, _tx| {
        graph
            .traversal_mut()
            .add_v(NodeSpec::new(
                NodeClassImportPath::new(node_type).unwrap(),
                Value::Null,
            ))
            .expect("the node is named")
            .first()
            .expect("the node is created")
            .id
            .clone()
    })
}

/// A natively compiled node runs in the app process, which runs no node code, so the stream's
/// function node pool evaluates the island it reads and `connect` makes the link.
///
/// Mental-revert: refusing every natively compiled reader refuses a link the pool carries at
/// play, and a stream built with one never loads.
#[test]
fn a_function_node_into_a_natively_compiled_node_connects_for_the_pool_to_evaluate() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let switch = a_node_of(&compiler, FUNCTION_NODE);
    let converter = a_node_of(
        &compiler,
        MockInputOnlyNode::node_class_import_path().as_str(),
    );

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(switch, "result"),
        InputLinkPortRef::new(converter, "in1"),
    )
    .expect("the stream's function node pool evaluates the island");
    assert_eq!(the_links_in(&compiler), 1);
}

/// A view of the stream's project runs in the app process and the pool does not evaluate an
/// island it reads, so `connect` refuses the link in the words its wiring would at play.
///
/// Mental-revert: without the refusal, a stopped stream saves the link and fails at play.
#[test]
fn a_function_node_into_a_view_is_refused_and_never_made() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let switch = a_node_of(&compiler, FUNCTION_NODE);
    let program = a_node_of(&compiler, VIEW_READER);

    let refusal = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(switch.clone(), "result"),
        InputLinkPortRef::new(program.clone(), "in1"),
    )
    .expect_err("the app process runs no function node");

    assert!(matches!(refusal, Error::InvalidLink(_)), "{refusal:?}");
    assert_eq!(
        refusal.to_string(),
        Error::InvalidLink(format!(
            "the link from `{switch}.result` to `{program}.in1` feeds a function node island \
             into `{program}`, which runs in the app process; a function node runs inside the \
             read of a node in a helper process, and the app process runs no node code"
        ))
        .to_string()
    );
    assert_eq!(the_links_in(&compiler), 0);
}

/// A saved graph holding a link from a function node into a view opens, that link set aside
/// with why the engine cannot wire it, so the person can see and remove it while the stream
/// plays without it.
///
/// Mental-revert: refusing the link on a saved graph's load too fails the whole graph's open.
#[test]
fn a_saved_graphs_function_node_into_a_view_is_set_aside_as_it_opens() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let switch = a_node_of(&compiler, FUNCTION_NODE);
    let program = a_node_of(&compiler, VIEW_READER);

    let made = connect_a_link_of_a_saved_graph_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(switch, "result"),
        InputLinkPortRef::new(program, "in1"),
    )
    .expect("a saved graph opens with the link it holds");

    assert!(
        matches!(made, ALinkConnectedOrSetAside::SetAside(_)),
        "the link is set aside: {made:?}"
    );
    assert_eq!(the_links_in(&compiler), 0);
}

#[test]
fn a_function_node_into_a_node_its_node_code_runs_in_a_helper_process_connects() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let switch = a_node_of(&compiler, FUNCTION_NODE);
    let meter = a_node_of(&compiler, HELPER_PROCESS_READER);

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(switch, "result"),
        InputLinkPortRef::new(meter, "in1"),
    )
    .expect("a helper process evaluates the island at each read");
    assert_eq!(the_links_in(&compiler), 1);
}

#[test]
fn a_function_node_into_another_function_node_connects() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let upstream = a_node_of(&compiler, FUNCTION_NODE);
    let downstream = a_node_of(&compiler, FUNCTION_NODE);

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(upstream, "result"),
        InputLinkPortRef::new(downstream, "x"),
    )
    .expect("the two run in one island");
    assert_eq!(the_links_in(&compiler), 1);
}

/// A link an island cannot take, a second link into a port an island feeds or one closing a
/// cycle of function nodes, is refused by `connect` on a stream no commit builds, and the
/// judgement a link check makes refuses it in the same words; neither makes it.
///
/// Mental-revert: without the island's own refusals among connect's, a stopped stream would
/// take both links and save them.
#[test]
fn a_link_an_island_cannot_take_is_refused_by_connect_and_its_check_alike() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let first = a_node_of(&compiler, FUNCTION_NODE);
    let second = a_node_of(&compiler, FUNCTION_NODE);
    let third = a_node_of(&compiler, FUNCTION_NODE);
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(first.clone(), "result"),
        InputLinkPortRef::new(second.clone(), "x"),
    )
    .expect("the two run in one island");

    for (output, input, words) in [
        (
            OutputLinkPortRef::new(third.clone(), "result"),
            InputLinkPortRef::new(second.clone(), "x"),
            "shares the port with no other link",
        ),
        (
            OutputLinkPortRef::new(second.clone(), "result"),
            InputLinkPortRef::new(first.clone(), "x"),
            "closes a cycle of function nodes",
        ),
    ] {
        let checked = the_verdict_connect_takes_on_a_link(&compiler, output.clone(), input.clone())
            .expect_err("the check refuses the link");
        let refused =
            connect_on_this_thread(&compiler, output, input).expect_err("connect refuses the link");

        assert_eq!(checked.to_string(), refused.to_string());
        assert!(refused.to_string().contains(words), "{refused}");
    }
    assert_eq!(the_links_in(&compiler), 1);
}

/// Assert connect refuses the link from `from` to `to` made now, and sets it aside from a saved
/// graph, each in words holding `words`, making neither.
fn assert_refused_made_now_and_set_aside_from_a_saved_graph(
    compiler: &Arc<Compiler>,
    from: OutputLinkPortRef,
    to: InputLinkPortRef,
    words: &str,
) {
    let links_before = the_links_in(compiler);
    let refused = connect_on_this_thread(compiler, from.clone(), to.clone())
        .expect_err("connect refuses the link made now")
        .to_string();
    assert!(refused.contains(words), "{refused}");
    assert_eq!(the_links_in(compiler), links_before);
    match connect_a_link_of_a_saved_graph_on_this_thread(compiler, from, to) {
        Ok(ALinkConnectedOrSetAside::SetAside(why)) => {
            assert_eq!(
                without_the_link_ids_named(&why.to_string()),
                without_the_link_ids_named(&refused)
            );
        }
        made => panic!("a saved graph's link is set aside, got {made:?}"),
    }
    assert_eq!(the_links_in(compiler), links_before);
}

/// `words` with each link id they name as `link '<id>'` left out, since a link made twice is
/// given a new id each time.
fn without_the_link_ids_named(words: &str) -> String {
    let mut parts = words.split("link '");
    let mut kept = parts.next().unwrap_or_default().to_owned();
    for part in parts {
        kept.push_str("link '");
        kept.push_str(part.split_once('\'').map_or(part, |(_, after)| after));
    }
    kept
}

/// A second link into a port an island feeds is caught at the edit.
#[test]
fn a_second_link_into_a_port_an_island_feeds_is_caught_at_the_edit() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let first = a_node_of(&compiler, FUNCTION_NODE);
    let second = a_node_of(&compiler, FUNCTION_NODE);
    let meter = a_node_of(&compiler, HELPER_PROCESS_READER);
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(first, "result"),
        InputLinkPortRef::new(meter.clone(), "in1"),
    )
    .expect("one island feeds the port");
    assert_refused_made_now_and_set_aside_from_a_saved_graph(
        &compiler,
        OutputLinkPortRef::new(second, "result"),
        InputLinkPortRef::new(meter, "in1"),
        "shares the port with no other link",
    );
}

/// An island into a port declaring an audio window is caught at the edit.
#[test]
fn an_island_into_a_port_declaring_an_audio_window_is_caught_at_the_edit() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let switch = a_node_of(&compiler, FUNCTION_NODE);
    let windowed = a_node_of(&compiler, HELPER_PROCESS_WINDOWED_READER);
    assert_refused_made_now_and_set_aside_from_a_saved_graph(
        &compiler,
        OutputLinkPortRef::new(switch, "result"),
        InputLinkPortRef::new(windowed, "audio"),
        "declares an `audio_window`",
    );
}

/// A link closing a cycle of function nodes is caught at the edit.
#[test]
fn a_cycle_of_function_nodes_is_caught_at_the_edit() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let first = a_node_of(&compiler, FUNCTION_NODE);
    let second = a_node_of(&compiler, FUNCTION_NODE);
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(first.clone(), "result"),
        InputLinkPortRef::new(second.clone(), "x"),
    )
    .expect("the two run in one island");
    assert_refused_made_now_and_set_aside_from_a_saved_graph(
        &compiler,
        OutputLinkPortRef::new(second, "result"),
        InputLinkPortRef::new(first, "x"),
        "closes a cycle of function nodes",
    );
}

/// An island into a view, a node in the app process the function node pool does not evaluate,
/// is caught at the edit.
///
/// Mental-revert: leaving this judgement to the commit that builds the stream connects the
/// saved graph's link here and fails the stream at its start.
#[test]
fn an_island_into_a_node_in_the_app_process_is_caught_at_the_edit() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let switch = a_node_of(&compiler, FUNCTION_NODE);
    let program = a_node_of(&compiler, VIEW_READER);
    assert_refused_made_now_and_set_aside_from_a_saved_graph(
        &compiler,
        OutputLinkPortRef::new(switch, "result"),
        InputLinkPortRef::new(program, "in1"),
        "runs in the app process",
    );
}

/// An island into a reactive reader no other node feeds is caught at the edit, and connects
/// once the reader has a source.
#[test]
fn an_island_into_a_reactive_reader_only_islands_feed_is_caught_at_the_edit() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let switch = a_node_of(&compiler, FUNCTION_NODE);
    let source = a_node_of(&compiler, HELPER_PROCESS_SOURCE);
    let reader = a_node_of(&compiler, HELPER_PROCESS_REACTIVE_READER);
    let refused = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(switch.clone(), "result"),
        InputLinkPortRef::new(reader.clone(), "in1"),
    )
    .expect_err("connect refuses the link made now");
    assert!(refused.to_string().contains("would never run"), "{refused}");
    assert_eq!(the_links_in(&compiler), 0);

    let made = connect_a_link_of_a_saved_graph_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(switch.clone(), "result"),
        InputLinkPortRef::new(reader.clone(), "in1"),
    )
    .expect("a saved graph's link leaves the reader rule to the sweep")
    .connected()
    .expect("the link is made");
    let swept: Vec<_> = compiler.scope_reading(|graph| {
        the_island_links_into_a_reactive_reader_only_islands_feed(graph).collect()
    });
    assert_eq!(swept.len(), 1);
    assert_eq!(swept[0].0, made);
    assert_eq!(swept[0].1.to_string(), refused.to_string());
    compiler.scope(|graph, _tx| {
        graph.traversal_mut().e(&made).drop();
    });

    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source, "out"),
        InputLinkPortRef::new(reader.clone(), "in2"),
    )
    .expect("a source wakes the reader");
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(switch, "result"),
        InputLinkPortRef::new(reader, "in1"),
    )
    .expect("the reader's source wakes it, and it evaluates the island at each read");
}

/// Taking out a reactive reader's only source, by its link or its node, is refused at the edit
/// when only islands would feed it, and taking out the reader too is not.
#[test]
fn a_removal_leaving_a_reactive_reader_fed_only_by_islands_is_caught_at_the_edit() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let switch = a_node_of(&compiler, FUNCTION_NODE);
    let source = a_node_of(&compiler, HELPER_PROCESS_SOURCE);
    let reader = a_node_of(&compiler, HELPER_PROCESS_REACTIVE_READER);
    let source_link = connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(source.clone(), "out"),
        InputLinkPortRef::new(reader.clone(), "in2"),
    )
    .expect("a source wakes the reader");
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(switch, "result"),
        InputLinkPortRef::new(reader.clone(), "in1"),
    )
    .expect("the island connects beside the source");

    for removal in [ARemoval::OfALink(&source_link), ARemoval::OfANode(&source)] {
        let refused = compiler
            .scope_reading(|graph| refuse_a_removal_that_strands_a_reactive_reader(graph, removal))
            .expect_err("the reader would never run again")
            .to_string();
        assert!(refused.contains("never run again"), "{refused}");
        assert!(
            refused.contains(&format!(
                "or disconnect the function node link into `{reader}`"
            )),
            "{refused}"
        );
        let removal_named = match removal {
            ARemoval::OfALink(link_id) => format!("removing link `{link_id}`"),
            ARemoval::OfANode(node_run_id) => format!("removing node `{node_run_id}`"),
        };
        assert!(refused.contains(&removal_named), "{refused}");
    }
    compiler
        .scope_reading(|graph| {
            refuse_a_removal_that_strands_a_reactive_reader(graph, ARemoval::OfANode(&reader))
        })
        .expect("taking out the reader strands nothing");
}

/// A second link into a port declaring an audio window is caught at the edit.
#[test]
fn a_second_link_into_a_windowed_port_is_caught_at_the_edit() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let first = a_node_of(&compiler, HELPER_PROCESS_SOURCE);
    let second = a_node_of(&compiler, HELPER_PROCESS_SOURCE);
    let windowed = a_node_of(&compiler, HELPER_PROCESS_WINDOWED_READER);
    connect_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(first, "out"),
        InputLinkPortRef::new(windowed.clone(), "audio"),
    )
    .expect("a windowed port takes one link");
    assert_refused_made_now_and_set_aside_from_a_saved_graph(
        &compiler,
        OutputLinkPortRef::new(second, "out"),
        InputLinkPortRef::new(windowed, "audio"),
        "accepts exactly one inbound link",
    );
}

/// An output port past the destinations a channel carries is caught at the edit.
#[test]
fn an_output_past_its_destination_cap_is_caught_at_the_edit() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let source = a_node_of(&compiler, HELPER_PROCESS_SOURCE);
    for _ in 0..streamlib_ipc_types::MAX_DESTINATIONS_PER_CHANNEL {
        let meter = a_node_of(&compiler, HELPER_PROCESS_READER);
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(source.clone(), "out"),
            InputLinkPortRef::new(meter, "in1"),
        )
        .expect("the channel carries one more");
    }
    let one_too_many = a_node_of(&compiler, HELPER_PROCESS_READER);
    assert_refused_made_now_and_set_aside_from_a_saved_graph(
        &compiler,
        OutputLinkPortRef::new(source, "out"),
        InputLinkPortRef::new(one_too_many, "in1"),
        "destinations, and a channel carries at most",
    );
}

/// A destination past the inbound links it carries is caught at the edit.
#[test]
fn a_destination_past_its_inbound_cap_is_caught_at_the_edit() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let meter = a_node_of(&compiler, HELPER_PROCESS_READER);
    for _ in 0..streamlib_ipc_types::MAX_INBOUND_LINKS_PER_DESTINATION {
        let source = a_node_of(&compiler, HELPER_PROCESS_SOURCE);
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(source, "out"),
            InputLinkPortRef::new(meter.clone(), "in1"),
        )
        .expect("the destination holds one more");
    }
    let one_too_many = a_node_of(&compiler, HELPER_PROCESS_SOURCE);
    assert_refused_made_now_and_set_aside_from_a_saved_graph(
        &compiler,
        OutputLinkPortRef::new(one_too_many, "out"),
        InputLinkPortRef::new(meter, "in1"),
        "inbound links, and a destination carries at most",
    );
}

/// The stream's function node pool, one destination for every island it carries, is caught at
/// the edit past its inbound cap, though no reader it feeds is.
///
/// Mental-revert: counting only each reader's own inbound links takes every island link and
/// the start refuses the pool.
#[test]
fn the_function_node_pool_past_its_inbound_cap_is_caught_at_the_edit() {
    const READS_OF_EACH_ISLAND: usize = 30;
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let an_island_link_reading_from_outside = |compiler: &Arc<Compiler>| {
        let switch = a_node_of(compiler, FUNCTION_NODE);
        for _ in 0..READS_OF_EACH_ISLAND {
            let source = a_node_of(compiler, HELPER_PROCESS_SOURCE);
            connect_on_this_thread(
                compiler,
                OutputLinkPortRef::new(source, "out"),
                InputLinkPortRef::new(switch.clone(), "x"),
            )
            .expect("the island reads one more source");
        }
        (
            OutputLinkPortRef::new(switch, "result"),
            InputLinkPortRef::new(a_node_of(compiler, HELPER_PROCESS_MANUAL_READER), "in1"),
        )
    };
    let islands_the_pool_takes =
        streamlib_ipc_types::MAX_INBOUND_LINKS_PER_DESTINATION / READS_OF_EACH_ISLAND;
    for _ in 0..islands_the_pool_takes {
        let (from, to) = an_island_link_reading_from_outside(&compiler);
        connect_on_this_thread(&compiler, from, to).expect("the pool carries one more island");
    }
    let (from, to) = an_island_link_reading_from_outside(&compiler);
    assert_refused_made_now_and_set_aside_from_a_saved_graph(
        &compiler,
        from,
        to,
        &format!(
            "the stream's function node pool would hold {} inbound links",
            (islands_the_pool_takes + 1) * READS_OF_EACH_ISLAND
        ),
    );
}

/// The function node pool's cap is judged on a link into an island too: an island linked into
/// its reader before the sources it reads are linked into it is caught at the source link that
/// takes the pool past its cap.
///
/// Mental-revert: judging the pool's cap only on a link out of an island takes every source
/// link, and the start refuses the pool.
#[test]
fn the_function_node_pool_past_its_inbound_cap_is_caught_at_a_link_into_an_island() {
    const READS_OF_EACH_ISLAND: usize = 30;
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let islands_the_pool_takes =
        streamlib_ipc_types::MAX_INBOUND_LINKS_PER_DESTINATION / READS_OF_EACH_ISLAND;
    let mut refused = None;
    'islands: for _ in 0..=islands_the_pool_takes {
        let switch = a_node_of(&compiler, FUNCTION_NODE);
        connect_on_this_thread(
            &compiler,
            OutputLinkPortRef::new(switch.clone(), "result"),
            InputLinkPortRef::new(a_node_of(&compiler, HELPER_PROCESS_MANUAL_READER), "in1"),
        )
        .expect("an island that reads nothing yet holds the pool no link");
        for _ in 0..READS_OF_EACH_ISLAND {
            let source = a_node_of(&compiler, HELPER_PROCESS_SOURCE);
            if let Err(refusal) = connect_on_this_thread(
                &compiler,
                OutputLinkPortRef::new(source, "out"),
                InputLinkPortRef::new(switch.clone(), "x"),
            ) {
                refused = Some(refusal);
                break 'islands;
            }
        }
    }

    let refused = refused.expect("a link into an island takes the pool past its cap");
    assert!(
        refused.to_string().contains(&format!(
            "the stream's function node pool would hold {} inbound links",
            streamlib_ipc_types::MAX_INBOUND_LINKS_PER_DESTINATION + 1
        )),
        "{refused}"
    );
}

/// A saved graph's link the engine can wire is connected as any other.
#[test]
fn a_saved_graphs_link_the_engine_can_wire_is_connected() {
    let compiler = a_stopped_stream_resolving_a_function_node_type();
    let switch = a_node_of(&compiler, FUNCTION_NODE);
    let meter = a_node_of(&compiler, HELPER_PROCESS_READER);

    let made = connect_a_link_of_a_saved_graph_on_this_thread(
        &compiler,
        OutputLinkPortRef::new(switch, "result"),
        InputLinkPortRef::new(meter, "in1"),
    )
    .expect("a helper process evaluates the island at each read");

    assert!(
        matches!(made, ALinkConnectedOrSetAside::Connected(_)),
        "{made:?}"
    );
    assert_eq!(the_links_in(&compiler), 1);
}
