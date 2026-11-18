// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use crate::core::runtime::a_project_on_disk_for_a_test::{
    options_loading_a_stream_of_the_test_project_at,
    options_loading_a_stream_of_the_test_project_in,
};
use serde_json::json;
use serial_test::serial;

use super::*;
use crate::core::compiler::PendingOperation;
use crate::core::runtime::Runner;
use crate::core::test_support::{
    MockInputOnlyNode, MockNode, MockOutputOnlyNode, ensure_test_mocks_registered,
};

const NODE_A: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
const NODE_B: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

/// A graph document of `a_name` (node A, an output-only mock) linked from `out1` into
/// `in1` of `b_name` (node B, of `b_type`).
fn a_document_of_a_linked_into_b(a_name: &str, b_name: &str, b_type: NodeClassImportPath) -> Value {
    json!({
        "nodes": [
            {
                "id": NODE_A,
                "name": a_name,
                "type": MockOutputOnlyNode::node_class_import_path().as_str(),
                "config": {},
            },
            { "id": NODE_B, "name": b_name, "type": b_type.as_str(), "config": {} },
        ],
        "links": [{
            "source": { "node": a_name, "port": "out1" },
            "target": { "node": b_name, "port": "in1" },
        }],
    })
}

/// Each live node as `(UUID, name, type)`, and each live link by node UUID, sorted.
fn the_live_graph_by_node_uuid(
    stream: &LoadedStreamInThisRuntime,
) -> (Vec<(String, String, String)>, Vec<String>) {
    let mut nodes: Vec<_> = stream
        .the_live_nodes()
        .into_iter()
        .map(|node| {
            (
                node.node_uuid.to_string(),
                node.display_name,
                node.node_type.as_str().to_owned(),
            )
        })
        .collect();
    nodes.sort();
    let mut links: Vec<_> = stream
        .the_live_links_by_node_uuid()
        .into_iter()
        .map(
            |LiveLinkAsGiven {
                 link_as_given: link,
                 ..
             }| {
                format!(
                    "{}/{} -> {}/{}",
                    link.source_node, link.source_port, link.target_node, link.target_port
                )
            },
        )
        .collect();
    links.sort();
    (nodes, links)
}

fn the_graph_node_count(stream: &LoadedStreamInThisRuntime) -> usize {
    stream
        .compiler
        .scope_reading(|graph| graph.traversal().v(()).iter().count())
}

fn reconcile(stream: &LoadedStreamInThisRuntime, graph_document: Value) -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(stream.reconcile_the_live_graph_to_the_graph_document(
            graph_document,
            LinksAReconcileMakes::ThoseTheLiveGraphHeldAsASavedGraphs,
        ))
}

fn a_stream_loaded_from(
    runner: &Runner,
    project_directory: &tempfile::TempDir,
    graph_document: Value,
) -> Arc<LoadedStreamInThisRuntime> {
    runner
        .load_stream_from_graph_snapshot(
            &GraphSnapshot::from_graph_document(graph_document).expect("the document reads"),
            options_loading_a_stream_of_the_test_project_at(project_directory.path(), "main"),
        )
        .expect("the stream loads")
}

const PICTURE_SOURCE_OF_TWO_FORMATS: &str = "SavedLinkCheckPictureSourceOfTwoFormats";
const PICTURE_SINK_OF_ONE_FORMAT: &str = "SavedLinkCheckPictureSinkOfOneFormat";
const ANOTHER_PICTURE_SINK_OF_ONE_FORMAT: &str = "SavedLinkCheckAnotherPictureSinkOfOneFormat";
const LEVEL_SOURCE: &str = "SavedLinkCheckLevelSource";

fn a_saved_link_fixture_type(class_name: &str) -> NodeClassImportPath {
    NodeClassImportPath::new(format!("{}::{class_name}", module_path!())).unwrap()
}

/// Register a picture source stating two pixel formats, two picture sinks taking one of
/// them, and a Float source whose output carries the name of the sinks' input, each with
/// a constructor no test reaches.
fn register_the_saved_link_fixture_node_types() {
    use crate::core::descriptors::{
        NodeClassShortName, NodeDescriptor, PinDeclaration, PinType, PortDescriptor, PortFamily,
        PortFamilyDeclaration, TexturePixelFormat,
    };
    use crate::core::nodes::NODE_REGISTRY;
    static REGISTERED_ONCE_PER_PROCESS: std::sync::Once = std::sync::Once::new();
    REGISTERED_ONCE_PER_PROCESS.call_once(|| {
        let picture = |pin_type: PinType| {
            let family = PortFamily::carrying(&pin_type).expect("a typed port has a family");
            PortDescriptor::new(
                "picture",
                "picture",
                true,
                PortFamilyDeclaration::of_family(family),
            )
            .with_pin_declaration(PinDeclaration::exactly(pin_type))
        };
        let texture = |pixel_formats: &[TexturePixelFormat]| PinType::Texture {
            pixel_formats: pixel_formats.to_vec(),
        };
        let node_type = |class_name: &str| {
            NodeDescriptor::new(
                NodeClassShortName::new(class_name).unwrap(),
                a_saved_link_fixture_type(class_name),
                "a fixture",
            )
        };
        for descriptor in [
            node_type(PICTURE_SOURCE_OF_TWO_FORMATS).with_output(picture(texture(&[
                TexturePixelFormat::Bgra8Unorm,
                TexturePixelFormat::Rgba8Unorm,
            ]))),
            node_type(PICTURE_SINK_OF_ONE_FORMAT)
                .with_input(picture(texture(&[TexturePixelFormat::Bgra8Unorm]))),
            node_type(ANOTHER_PICTURE_SINK_OF_ONE_FORMAT)
                .with_input(picture(texture(&[TexturePixelFormat::Bgra8Unorm]))),
            node_type(LEVEL_SOURCE).with_output(picture(PinType::Float { exact_size: None })),
        ] {
            NODE_REGISTRY
                .register_with_a_constructor_for_a_test(
                    descriptor,
                    Box::new(|_node| Err(Error::NotSupported("a fixture runs no node".to_owned()))),
                )
                .expect("register the fixture node type");
        }
    });
}

/// A graph document of node A of `source` and node B of `sink`, the `picture` of A linked
/// into the `picture` of B when `linked`.
fn a_document_of_a_picture_link(source: &str, sink: &str, linked: bool) -> Value {
    let links = if linked {
        json!([{
            "source": { "node": "source", "port": "picture" },
            "target": { "node": "sink", "port": "picture" },
        }])
    } else {
        json!([])
    };
    json!({
        "nodes": [
            {
                "id": NODE_A, "name": "source", "config": {},
                "type": a_saved_link_fixture_type(source).as_str(),
            },
            {
                "id": NODE_B, "name": "sink", "config": {},
                "type": a_saved_link_fixture_type(sink).as_str(),
            },
        ],
        "links": links,
    })
}

fn reconcile_making(
    stream: &LoadedStreamInThisRuntime,
    graph_document: Value,
    links_it_makes: LinksAReconcileMakes,
) -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(
            stream.reconcile_the_live_graph_to_the_graph_document(graph_document, links_it_makes),
        )
}

/// A link saved before the pin link verdict judged it, a picture of two formats into an input
/// taking one, is kept by a load and by a reconcile of a saved document, and by an edit that
/// remakes it into a node it replaces; an edit making it anew is refused as `connect` is.
///
/// Mental-revert: making each link of a load or a reconcile as `connect` does refuses the
/// load, and every reconcile here but the one taking the link out.
#[test]
#[serial]
fn a_saved_link_its_pin_types_refuse_is_kept_and_an_edit_making_it_anew_is_refused() {
    ensure_test_mocks_registered();
    register_the_saved_link_fixture_node_types();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let the_picture_link = vec![format!("{NODE_A}/picture -> {NODE_B}/picture")];
    let stream = a_stream_loaded_from(
        &runner,
        &project_directory,
        a_document_of_a_picture_link(
            PICTURE_SOURCE_OF_TWO_FORMATS,
            PICTURE_SINK_OF_ONE_FORMAT,
            true,
        ),
    );
    assert_eq!(the_live_graph_by_node_uuid(&stream).1, the_picture_link);

    reconcile_making(
        &stream,
        a_document_of_a_picture_link(
            PICTURE_SOURCE_OF_TWO_FORMATS,
            ANOTHER_PICTURE_SINK_OF_ONE_FORMAT,
            true,
        ),
        LinksAReconcileMakes::ThoseTheLiveGraphHeldAsASavedGraphs,
    )
    .expect("an edit replacing the sink remakes the link the live graph held");
    assert_eq!(the_live_graph_by_node_uuid(&stream).1, the_picture_link);

    let unlinked = a_document_of_a_picture_link(
        PICTURE_SOURCE_OF_TWO_FORMATS,
        ANOTHER_PICTURE_SINK_OF_ONE_FORMAT,
        false,
    );
    reconcile_making(
        &stream,
        unlinked,
        LinksAReconcileMakes::ThoseTheLiveGraphHeldAsASavedGraphs,
    )
    .expect("taking the link out is an edit");
    assert!(the_live_graph_by_node_uuid(&stream).1.is_empty());

    let linked = a_document_of_a_picture_link(
        PICTURE_SOURCE_OF_TWO_FORMATS,
        ANOTHER_PICTURE_SINK_OF_ONE_FORMAT,
        true,
    );
    let refusal = reconcile_making(
        &stream,
        linked.clone(),
        LinksAReconcileMakes::ThoseTheLiveGraphHeldAsASavedGraphs,
    )
    .expect_err("an edit making the link anew is judged as connect judges it");
    assert!(matches!(refusal, Error::PinLinkRefused(_)), "{refusal:?}");

    reconcile_making(
        &stream,
        linked,
        LinksAReconcileMakes::EachAsASavedGraphsLink,
    )
    .expect("a reconcile of a saved document keeps the link");
    assert_eq!(the_live_graph_by_node_uuid(&stream).1, the_picture_link);
}

/// A saved link whose ports' families do not match is refused by a load, as it was before the
/// pin link verdict judged it.
#[test]
#[serial]
fn a_saved_link_whose_families_do_not_match_is_refused_by_a_load() {
    ensure_test_mocks_registered();
    register_the_saved_link_fixture_node_types();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let refusal = runner
        .load_stream_from_graph_snapshot(
            &GraphSnapshot::from_graph_document(a_document_of_a_picture_link(
                LEVEL_SOURCE,
                PICTURE_SINK_OF_ONE_FORMAT,
                true,
            ))
            .expect("the document reads"),
            options_loading_a_stream_of_the_test_project_at(project_directory.path(), "main"),
        )
        .err()
        .expect("a Float into a Texture is refused by its families too");
    assert!(
        refusal
            .to_string()
            .contains("a Float output does not connect to a Texture"),
        "{refusal}"
    );
}

#[test]
fn a_reconcile_compares_a_link_carried_through_the_pool_as_the_link_it_was_given() {
    let (graph, lfo, reader, lfo_into_the_reader) =
        crate::core::compiler::compiler_ops::function_node_pool::tests::a_graph_carrying_an_lfo_into_its_reader_through_the_pool();
    let node_uuid_of =
        |node_run_id: &NodeRunId| graph.traversal().v(node_run_id).first().unwrap().node_uuid;

    let live_nodes = the_live_nodes_as_given_of(&graph);
    let live_links = the_live_links_as_given_by_node_uuid_of(&graph);

    assert_eq!(
        live_nodes
            .iter()
            .map(|live_node| live_node.node_uuid)
            .collect::<HashSet<_>>(),
        HashSet::from([node_uuid_of(&lfo), node_uuid_of(&reader)]),
        "the pool is no node a document holds"
    );
    assert_eq!(live_links.len(), 1);
    let LiveLinkAsGiven {
        link_as_given,
        wired_as,
    } = &live_links[0];
    assert_eq!(
        link_as_given,
        &RunningLinkBetweenTwoNodesByUuid {
            source_node: node_uuid_of(&lfo),
            source_port: "result".to_string(),
            target_node: node_uuid_of(&reader),
            target_port: "in1".to_string(),
        }
    );
    assert_eq!(
        wired_as.iter().cloned().collect::<HashSet<_>>(),
        crate::core::compiler::compiler_ops::function_node_pool::the_links_a_link_is_wired_as(
            &graph,
            &lfo_into_the_reader
        )
        .into_iter()
        .collect::<HashSet<_>>(),
        "a disconnect takes out both links it is wired as"
    );
}

#[test]
#[serial]
fn two_kept_nodes_that_swap_names_are_renamed_and_keep_their_link() {
    ensure_test_mocks_registered();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let stream = a_stream_loaded_from(
        &runner,
        &project_directory,
        a_document_of_a_linked_into_b("left", "right", MockNode::node_class_import_path()),
    );

    let swapped =
        a_document_of_a_linked_into_b("right", "left", MockNode::node_class_import_path());
    reconcile(&stream, swapped).expect("the swap applies");

    assert_eq!(
        the_live_graph_by_node_uuid(&stream),
        (
            vec![
                (
                    NODE_A.to_owned(),
                    "right".to_owned(),
                    MockOutputOnlyNode::node_class_import_path()
                        .as_str()
                        .to_owned()
                ),
                (
                    NODE_B.to_owned(),
                    "left".to_owned(),
                    MockNode::node_class_import_path().as_str().to_owned()
                ),
            ],
            vec![format!("{NODE_A}/out1 -> {NODE_B}/in1")],
        )
    );
}

#[test]
#[serial]
fn a_node_whose_type_changes_on_a_stream_not_started_is_replaced_under_its_uuid() {
    ensure_test_mocks_registered();
    let project_directory = tempfile::tempdir().expect("a project directory");
    let runner = Runner::new().expect("Runner::new");
    let stream = a_stream_loaded_from(
        &runner,
        &project_directory,
        a_document_of_a_linked_into_b("a", "b", MockNode::node_class_import_path()),
    );
    let replaced_node = stream
        .the_live_nodes()
        .into_iter()
        .find(|node| node.node_uuid.to_string() == NODE_B)
        .expect("node B is live")
        .node_run_id;

    reconcile(
        &stream,
        a_document_of_a_linked_into_b("a", "b", MockInputOnlyNode::node_class_import_path()),
    )
    .expect("the type change applies");

    assert_eq!(
        the_live_graph_by_node_uuid(&stream),
        (
            vec![
                (
                    NODE_A.to_owned(),
                    "a".to_owned(),
                    MockOutputOnlyNode::node_class_import_path()
                        .as_str()
                        .to_owned()
                ),
                (
                    NODE_B.to_owned(),
                    "b".to_owned(),
                    MockInputOnlyNode::node_class_import_path()
                        .as_str()
                        .to_owned()
                ),
            ],
            vec![format!("{NODE_A}/out1 -> {NODE_B}/in1")],
        )
    );
    assert_eq!(
        the_graph_node_count(&stream),
        2,
        "the replaced node is out of the graph"
    );
    assert!(
        !stream
            .compiler
            .logged_pending_operations()
            .iter()
            .any(|operation| matches!(
                operation,
                PendingOperation::AddNode(id) | PendingOperation::RemoveNode(id, _)
                    if *id == replaced_node
            )),
        "the start builds nothing of the replaced node"
    );
}

/// A stand-in venv interpreter in `project_directory` that answers every describe with
/// what `describe_answer` holds when it runs, so a test can change the answer between
/// describes.
fn an_interpreter_answering_each_describe_from(
    project_directory: &tempfile::TempDir,
    describe_answer: &std::path::Path,
) -> crate::core::runtime::StreamEnvironment {
    let interpreter = project_directory.path().join("stub-python");
    crate::core::test_support::write_an_executable_script_from_a_child_process(
        &interpreter,
        &format!("#!/bin/sh\ncat '{}'\n", describe_answer.display()),
    );
    crate::core::runtime::StreamEnvironment {
        project_directory: project_directory.path().to_path_buf(),
        interpreter,
    }
}

fn a_describe_answer_of(described_node_types: &[(&NodeClassImportPath, &str)]) -> String {
    json!({
        "described_node_types": described_node_types
            .iter()
            .map(|(node_type, input_port)| json!({
                "import_path": node_type.as_str(),
                "short_name": "Blur",
                "description": "blurs",
                "kind": "process",
                "execution": {"mode": "reactive"},
                "scheduling_priority": null,
                "paced_by_real_time": false,
                "config_schema": {"type": "object"},
                "input_ports": [{"name": input_port, "description": "", "family": {"family": "records"}, "delivery_profile": "newest"}],
                "output_ports": [],
            }))
            .collect::<Vec<_>>(),
        "refused_node_types": [],
    })
    .to_string()
}

/// A document refused after its describe leaves every node type the stream resolves as it
/// was: a type it re-described keeps its earlier ports, and a type it newly described is
/// not resolved at all.
#[test]
#[serial]
fn a_refused_document_leaves_the_node_types_the_stream_resolves_as_they_were() {
    ensure_test_mocks_registered();
    let kept_type = NodeClassImportPath::new("my_app.kept_across_a_refused_reload:Blur")
        .expect("the test path names a class");
    let newly_named_type = NodeClassImportPath::new("my_app.named_by_a_refused_reload:Blur")
        .expect("the test path names a class");
    let project_directory = tempfile::tempdir().expect("a project directory");
    let describe_answer = project_directory.path().join("describe-answer.json");
    std::fs::write(
        &describe_answer,
        a_describe_answer_of(&[(&kept_type, "video")]),
    )
    .expect("the first describe answer is written");
    let stream_environment =
        an_interpreter_answering_each_describe_from(&project_directory, &describe_answer);
    let runner = Runner::new().expect("Runner::new");
    runner
        .set_node_interpreter_lend_directory("/opt/tatolab/lib/tatolab/lend".into())
        .expect("the runner's first lend directory");
    let stream = runner
        .load_stream_from_graph_snapshot(
            &GraphSnapshot::from_graph_document(json!({
                "nodes": [{ "id": NODE_A, "name": "kept", "type": kept_type.as_str() }],
            }))
            .expect("the document reads"),
            options_loading_a_stream_of_the_test_project_in(stream_environment, "main"),
        )
        .expect("the stream loads");

    std::fs::write(
        &describe_answer,
        a_describe_answer_of(&[(&kept_type, "frames"), (&newly_named_type, "audio")]),
    )
    .expect("the later describe answer is written");
    let refusal = reconcile(
        &stream,
        json!({
            "nodes": [
                { "id": NODE_A, "name": "kept", "type": kept_type.as_str() },
                { "id": NODE_B, "name": "added", "type": newly_named_type.as_str() },
                {
                    "name": "source",
                    "type": MockOutputOnlyNode::node_class_import_path().as_str(),
                },
            ],
            "links": [{
                "source": { "node": "source", "port": "out1" },
                "target": { "node": "added", "port": "no_such_port" },
            }],
        }),
    )
    .expect_err("a link into a port the type does not declare is refused");

    let node_types = stream.node_types_this_stream_resolves();
    let kept_input_names: Vec<String> = node_types
        .descriptor(&kept_type)
        .expect("the stream still resolves the kept type")
        .inputs
        .into_iter()
        .map(|port| port.name)
        .collect();
    assert_eq!(kept_input_names, ["video"], "refused with: {refusal}");
    assert!(
        node_types.descriptor(&newly_named_type).is_none(),
        "the refused document's new type stayed resolved; refused with: {refusal}"
    );
    assert_eq!(
        the_live_graph_by_node_uuid(&stream).0,
        vec![(
            NODE_A.to_owned(),
            "kept".to_owned(),
            kept_type.as_str().to_owned()
        )]
    );
}
