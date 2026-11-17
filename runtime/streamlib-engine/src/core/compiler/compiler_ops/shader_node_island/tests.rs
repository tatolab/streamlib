// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::super::function_node_island::tests::{a_function_node_type, add_a_node, link};
use super::super::function_node_island::{HowALinkIsWired, how_a_link_is_wired};
use super::super::shader_node_island_program::tests::shader_program;
use super::*;
use crate::core::descriptors::{
    NodeClassImportPath, NodeClassShortName, NodeDescriptor, PortDescriptor, PortFamily,
    PortFamilyDeclaration,
};
use crate::core::nodes::{HowANodeOfADescribedTypeRuns, NodeTypesOneStreamResolves};

fn a_descriptor(
    import_path: &str,
    node_kind: NodeKind,
    inputs: &[(&str, PortFamily)],
    outputs: &[&str],
) -> NodeDescriptor {
    let import_path = NodeClassImportPath::new(import_path).unwrap();
    let short_name = import_path.as_str().rsplit(':').next().unwrap().to_string();
    let mut descriptor = NodeDescriptor::new(
        NodeClassShortName::new(short_name).unwrap(),
        import_path,
        "a node type for a test",
    );
    descriptor.node_kind = node_kind;
    for (port, family) in inputs {
        descriptor = descriptor.with_input(
            PortDescriptor::new(*port, "", true, PortFamilyDeclaration::of_family(*family))
                .with_delivery_profile("newest"),
        );
    }
    for port in outputs {
        descriptor = descriptor.with_output(PortDescriptor::new(
            *port,
            "",
            true,
            PortFamilyDeclaration::of_family(PortFamily::Image),
        ));
    }
    descriptor
}

fn a_constructor_that_builds_nothing() -> crate::core::nodes::DynamicNodeConstructorFn {
    Box::new(|node| {
        Err(Error::Configuration(format!(
            "`{}` builds no node in this test",
            node.id
        )))
    })
}

/// A graph resolving `source` and `sink`, two process types carrying
/// Images, `tint`, `blur` and `blend`, three shader types, and `lfo`, a
/// function node type.
fn a_graph_resolving_shader_and_process_types() -> Graph {
    let node_types = NodeTypesOneStreamResolves::new();
    for descriptor in [
        a_descriptor("test_streams:source", NodeKind::Process, &[], &["video"]),
        a_descriptor(
            "test_streams:sink",
            NodeKind::Process,
            &[("video", PortFamily::Image)],
            &[],
        ),
    ] {
        node_types
            .register_a_type_described_in_this_streams_node_interpreter(
                descriptor,
                HowANodeOfADescribedTypeRuns::AsANodeItsConstructorBuilds(
                    a_constructor_that_builds_nothing(),
                ),
            )
            .unwrap();
    }
    for (function_name, image_ports, float_ports, slang_body) in [
        (
            "tint",
            &["image"][..],
            &["amount"][..],
            "return image.pixel * amount;",
        ),
        (
            "blur",
            &["image"][..],
            &[][..],
            "return image.sample(float2(0.5, 0.5));",
        ),
        (
            "blend",
            &["a", "b"][..],
            &[][..],
            "return lerp(a.pixel, b.pixel, 0.5);",
        ),
    ] {
        let inputs: Vec<(&str, PortFamily)> = image_ports
            .iter()
            .map(|port| (*port, PortFamily::Image))
            .chain(float_ports.iter().map(|port| (*port, PortFamily::Channels)))
            .collect();
        node_types
            .register_a_type_described_in_this_streams_node_interpreter(
                a_descriptor(
                    &format!("test_shaders:{function_name}"),
                    NodeKind::Shader,
                    &inputs,
                    &["result"],
                ),
                HowANodeOfADescribedTypeRuns::OnTheNativeShaderEvaluatorOfItsIsland {
                    shader_node_program: shader_program(
                        function_name,
                        slang_body,
                        image_ports,
                        float_ports,
                    ),
                    evaluator_of_the_island_it_hosts: a_constructor_that_builds_nothing(),
                },
            )
            .unwrap();
    }
    node_types
        .register_a_type_described_in_this_streams_node_interpreter(
            a_function_node_type("test_ops:lfo", &[], &["result"]),
            HowANodeOfADescribedTypeRuns::InsideTheReadOfEachNodeItFeeds {
                node_once_promoted_to_its_own_process: a_constructor_that_builds_nothing(),
            },
        )
        .unwrap();
    crate::core::test_support::ensure_test_mocks_registered();
    Graph::new_resolving_node_types_through(Arc::new(node_types))
}

fn add(graph: &mut Graph, import_path: &str) -> NodeRunId {
    add_a_node(graph, import_path, serde_json::json!({}))
}

struct SourceIntoTintIntoBlurIntoTintIntoSink {
    graph: Graph,
    source: NodeRunId,
    tint: NodeRunId,
    blur: NodeRunId,
    tint_again: NodeRunId,
    sink: NodeRunId,
    links: Vec<LinkUniqueId>,
}

fn source_into_tint_into_blur_into_tint_into_sink() -> SourceIntoTintIntoBlurIntoTintIntoSink {
    let mut graph = a_graph_resolving_shader_and_process_types();
    let source = add(&mut graph, "test_streams:source");
    let tint = add(&mut graph, "test_shaders:tint");
    let blur = add(&mut graph, "test_shaders:blur");
    let tint_again = add(&mut graph, "test_shaders:tint");
    let sink = add(&mut graph, "test_streams:sink");
    let links = vec![
        link(&mut graph, (&source, "video"), (&tint, "image")),
        link(&mut graph, (&tint, "result"), (&blur, "image")),
        link(&mut graph, (&blur, "result"), (&tint_again, "image")),
        link(&mut graph, (&tint_again, "result"), (&sink, "video")),
    ];
    SourceIntoTintIntoBlurIntoTintIntoSink {
        graph,
        source,
        tint,
        blur,
        tint_again,
        sink,
        links,
    }
}

#[test]
fn a_shader_chain_is_one_island_evaluated_by_the_node_whose_image_leaves_it() {
    let stream = source_into_tint_into_blur_into_tint_into_sink();
    for member in [&stream.tint, &stream.blur, &stream.tint_again] {
        assert_eq!(
            the_host_of_the_shader_node_island_of(&stream.graph, member),
            stream.tint_again
        );
    }
    assert!(is_fused_into_the_shader_node_island_of_another(
        &stream.graph,
        &stream.tint
    ));
    assert!(!is_fused_into_the_shader_node_island_of_another(
        &stream.graph,
        &stream.tint_again
    ));
    let (island, member_nodes) =
        the_shader_node_island_hosted_by(&stream.graph, &stream.tint_again).unwrap();
    assert_eq!(
        member_nodes,
        [
            stream.tint.clone(),
            stream.blur.clone(),
            stream.tint_again.clone()
        ]
    );
    assert_eq!(
        island.the_input_port_each_evaluation_waits_on(),
        the_host_input_port_a_fused_shader_node_input_is_read_at(
            &stream.tint_again,
            &stream.tint,
            "image"
        )
        .port_name()
    );
}

#[test]
fn links_inside_an_island_are_calls_and_a_link_into_a_fused_node_reads_at_its_host() {
    let stream = source_into_tint_into_blur_into_tint_into_sink();
    let how_each_link_is_wired: Vec<HowALinkIsWired> = stream
        .links
        .iter()
        .map(|link_id| how_a_link_is_wired(&stream.graph, link_id).unwrap())
        .collect();
    assert_eq!(
        how_each_link_is_wired,
        [
            HowALinkIsWired::ThroughAChannelIntoTheHostOfItsShaderNodeIsland(
                the_host_input_port_a_fused_shader_node_input_is_read_at(
                    &stream.tint_again,
                    &stream.tint,
                    "image"
                )
            ),
            HowALinkIsWired::AsACallInsideAShaderNodeIsland,
            HowALinkIsWired::AsACallInsideAShaderNodeIsland,
            HowALinkIsWired::ThroughAChannel,
        ]
    );
}

#[test]
fn a_shader_whose_image_fans_out_or_reaches_a_non_shader_node_is_not_fused() {
    let mut stream = source_into_tint_into_blur_into_tint_into_sink();
    let second_sink = add(&mut stream.graph, "test_streams:sink");
    link(
        &mut stream.graph,
        (&stream.tint, "result"),
        (&second_sink, "video"),
    );
    assert!(!is_fused_into_the_shader_node_island_of_another(
        &stream.graph,
        &stream.tint
    ));
    assert_eq!(
        the_host_of_the_shader_node_island_of(&stream.graph, &stream.tint),
        stream.tint
    );
    assert_eq!(
        the_host_of_the_shader_node_island_of(&stream.graph, &stream.blur),
        stream.tint_again
    );
    assert!(!is_fused_into_the_shader_node_island_of_another(
        &stream.graph,
        &stream.source
    ));
}

#[test]
fn placing_the_islands_hands_the_island_to_its_host_and_its_group_to_every_member() {
    let mut stream = source_into_tint_into_blur_into_tint_into_sink();
    let every_node = [
        stream.source.clone(),
        stream.tint.clone(),
        stream.blur.clone(),
        stream.tint_again.clone(),
        stream.sink.clone(),
    ];
    let fused = place_each_shader_node_island_on_its_host(&mut stream.graph, &every_node).unwrap();
    assert_eq!(
        fused,
        HashSet::from([stream.tint.clone(), stream.blur.clone()])
    );
    let host = stream
        .graph
        .traversal()
        .v(&stream.tint_again)
        .first()
        .unwrap();
    let hosted = host.get::<ShaderNodeIslandHostedHereComponent>().unwrap();
    assert_eq!(hosted.island.members().len(), 3);
    for member in [&stream.tint, &stream.blur, &stream.tint_again] {
        let node = stream.graph.traversal().v(member).first().unwrap();
        let fused_group = node.get::<ShaderNodeFusedGroupComponent>().unwrap();
        assert!(Arc::ptr_eq(&fused_group.0, &hosted.fused_group));
    }
    let rendered = stream
        .graph
        .traversal()
        .v(&stream.tint)
        .first()
        .unwrap()
        .serialize_components();
    assert_eq!(
        rendered["fused_group"]["evaluated_by"],
        stream
            .graph
            .traversal()
            .v(&stream.tint_again)
            .first()
            .unwrap()
            .display_name
    );
}

fn sorted<T: Ord + Clone>(items: &[T]) -> Vec<T> {
    let mut sorted = items.to_vec();
    sorted.sort();
    sorted
}

#[test]
fn a_live_link_into_a_fused_nodes_output_restarts_its_whole_island_and_a_teardown_nothing() {
    let mut stream = source_into_tint_into_blur_into_tint_into_sink();
    let every_node = [
        stream.source.clone(),
        stream.tint.clone(),
        stream.blur.clone(),
        stream.tint_again.clone(),
        stream.sink.clone(),
    ];
    place_each_shader_node_island_on_its_host(&mut stream.graph, &every_node).unwrap();
    let second_sink = add(&mut stream.graph, "test_streams:sink");
    let into_a_fused_node_output = link(
        &mut stream.graph,
        (&stream.blur, "result"),
        (&second_sink, "video"),
    );
    let restarted = the_running_shader_nodes_a_live_edit_restarts(
        &stream.graph,
        std::slice::from_ref(&second_sink),
        &[],
        std::slice::from_ref(&into_a_fused_node_output),
    )
    .unwrap();
    assert_eq!(
        restarted,
        RunningShaderNodesALiveEditRestarts {
            nodes: sorted(&[
                stream.tint.clone(),
                stream.blur.clone(),
                stream.tint_again.clone()
            ]),
            links: sorted(&stream.links),
        }
    );

    let teardown = the_running_shader_nodes_a_live_edit_restarts(
        &stream.graph,
        &[],
        &every_node,
        &stream.links,
    )
    .unwrap();
    assert!(teardown.is_empty(), "{teardown:?}");
}

#[test]
fn a_live_link_joining_two_running_shader_nodes_restarts_both_with_their_other_links() {
    let mut graph = a_graph_resolving_shader_and_process_types();
    let camera = add(&mut graph, "test_streams:source");
    let test_pattern = add(&mut graph, "test_streams:source");
    let mix = add(&mut graph, "test_shaders:blend");
    let kaleidoscope = add(&mut graph, "test_shaders:tint");
    let program = add(&mut graph, "test_streams:sink");
    let links_already_running = vec![
        link(&mut graph, (&camera, "video"), (&mix, "a")),
        link(&mut graph, (&test_pattern, "video"), (&mix, "b")),
        link(&mut graph, (&kaleidoscope, "result"), (&program, "video")),
    ];
    place_each_shader_node_island_on_its_host(
        &mut graph,
        &[
            camera.clone(),
            test_pattern.clone(),
            mix.clone(),
            kaleidoscope.clone(),
            program.clone(),
        ],
    )
    .unwrap();
    let mix_into_kaleidoscope = link(&mut graph, (&mix, "result"), (&kaleidoscope, "image"));

    let restarted = the_running_shader_nodes_a_live_edit_restarts(
        &graph,
        &[],
        &[],
        std::slice::from_ref(&mix_into_kaleidoscope),
    )
    .unwrap();
    assert_eq!(
        restarted,
        RunningShaderNodesALiveEditRestarts {
            nodes: sorted(&[mix, kaleidoscope]),
            links: sorted(&links_already_running),
        }
    );
}

#[test]
fn a_live_link_from_a_process_into_a_running_islands_host_restarts_nothing() {
    let mut stream = source_into_tint_into_blur_into_tint_into_sink();
    let every_node = [
        stream.source.clone(),
        stream.tint.clone(),
        stream.blur.clone(),
        stream.tint_again.clone(),
        stream.sink.clone(),
    ];
    place_each_shader_node_island_on_its_host(&mut stream.graph, &every_node).unwrap();
    let second_source = add(&mut stream.graph, "test_streams:source");
    let lone_tint = add(&mut stream.graph, "test_shaders:tint");
    place_each_shader_node_island_on_its_host(&mut stream.graph, std::slice::from_ref(&lone_tint))
        .unwrap();
    let into_a_host = link(
        &mut stream.graph,
        (&second_source, "video"),
        (&lone_tint, "image"),
    );
    let restarted = the_running_shader_nodes_a_live_edit_restarts(
        &stream.graph,
        std::slice::from_ref(&second_source),
        &[],
        std::slice::from_ref(&into_a_host),
    )
    .unwrap();
    assert!(restarted.is_empty(), "{restarted:?}");
}

#[test]
fn a_live_edit_restarting_shader_nodes_a_function_node_feeds_is_refused_naming_it() {
    let mut graph = a_graph_resolving_shader_and_process_types();
    let source = add(&mut graph, "test_streams:source");
    let mix = add(&mut graph, "test_shaders:blend");
    let tint = add(&mut graph, "test_shaders:tint");
    let lfo = add(&mut graph, "test_ops:lfo");
    link(&mut graph, (&source, "video"), (&mix, "a"));
    link(&mut graph, (&lfo, "result"), (&tint, "amount"));
    let mix_into_tint = link(&mut graph, (&mix, "result"), (&tint, "image"));
    let refusal = the_running_shader_nodes_a_live_edit_restarts(
        &graph,
        &[],
        &[],
        std::slice::from_ref(&mix_into_tint),
    )
    .unwrap_err()
    .to_string();
    assert!(refusal.contains("joins function node"), "{refusal}");
    assert!(refusal.contains("stop the stream"), "{refusal}");
}

#[test]
fn a_live_link_from_a_function_node_into_a_running_island_is_refused_naming_it() {
    let mut stream = source_into_tint_into_blur_into_tint_into_sink();
    let every_node = [
        stream.source.clone(),
        stream.tint.clone(),
        stream.blur.clone(),
        stream.tint_again.clone(),
        stream.sink.clone(),
    ];
    place_each_shader_node_island_on_its_host(&mut stream.graph, &every_node).unwrap();
    let lfo = add(&mut stream.graph, "test_ops:lfo");
    let into_a_fused_node = link(
        &mut stream.graph,
        (&lfo, "result"),
        (&stream.tint, "amount"),
    );
    let refusal = the_running_shader_nodes_a_live_edit_restarts(
        &stream.graph,
        std::slice::from_ref(&lfo),
        &[],
        std::slice::from_ref(&into_a_fused_node),
    )
    .unwrap_err()
    .to_string();
    assert!(refusal.contains("joins function node"), "{refusal}");
}

#[test]
fn a_restarted_island_reads_its_old_host_and_forgets_every_part_of_its_build() {
    let mut stream = source_into_tint_into_blur_into_tint_into_sink();
    let every_node = [
        stream.source.clone(),
        stream.tint.clone(),
        stream.blur.clone(),
        stream.tint_again.clone(),
        stream.sink.clone(),
    ];
    place_each_shader_node_island_on_its_host(&mut stream.graph, &every_node).unwrap();
    assert_eq!(
        the_host_the_island_of_a_fused_node_was_placed_on(&stream.graph, &stream.tint),
        Some(stream.tint_again.clone())
    );
    let island = [
        stream.tint.clone(),
        stream.blur.clone(),
        stream.tint_again.clone(),
    ];
    for node_id in &island {
        stream
            .graph
            .traversal_mut()
            .v(node_id)
            .first_mut()
            .unwrap()
            .insert(NodeMetrics::default());
    }
    forget_the_build_of_each_shader_node_restarted(&mut stream.graph, &island);
    for node_id in &island {
        let node = stream.graph.traversal().v(node_id).first().unwrap();
        assert!(!node.has::<NodeMetrics>());
        assert!(!node.has::<ShaderNodeIslandHostedHereComponent>());
        assert!(!node.has::<ShaderNodeFusedIntoTheIslandOfAnotherComponent>());
        assert!(!node.has::<ShaderNodeFusedGroupComponent>());
    }
}

#[test]
fn the_host_input_port_of_a_fused_input_is_already_in_cast_form() {
    let port = the_host_input_port_a_fused_shader_node_input_is_read_at(
        &NodeRunId::from("HostNodeId"),
        &NodeRunId::from("Plh868rqg4MCMX"),
        "image",
    );
    assert_eq!(
        cast_exposed_name_to_url_safe(port.port_name()).unwrap(),
        port.port_name()
    );
}

#[test]
fn shader_nodes_reading_their_own_image_in_a_cycle_are_refused() {
    let mut graph = a_graph_resolving_shader_and_process_types();
    let first = add(&mut graph, "test_shaders:blend");
    let second = add(&mut graph, "test_shaders:tint");
    link(&mut graph, (&first, "result"), (&second, "image"));
    link(&mut graph, (&second, "result"), (&first, "a"));
    let refusal = place_each_shader_node_island_on_its_host(&mut graph, &[first, second])
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("break the cycle"), "{refusal}");
}
