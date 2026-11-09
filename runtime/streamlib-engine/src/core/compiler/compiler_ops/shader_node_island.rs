// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shader node islands: a shader node whose Image only one shader node reads
//! is fused into that reader's island, which one evaluator runs on the node
//! whose Image leaves it.

use std::collections::HashSet;
use std::sync::Arc;

use super::shader_node_island_program::{
    ShaderNodeIslandMember, ShaderNodeIslandMemberInput, ShaderNodeIslandProgram,
};
use super::shader_node_program::ShaderNodeInputValueType;
use crate::core::descriptors::NodeKind;
use crate::core::error::{Error, Result};
use crate::core::graph::{
    Graph, GraphNodeWithComponents, InputLinkPortRef, Link, LinkUniqueId, ProcessorNode,
    ProcessorUniqueId, ShaderNodeFusedGroupComponent, ShaderNodeFusedGroupOfAnIsland,
};

/// The island a shader node evaluates, on the node whose Image leaves it.
pub(crate) struct ShaderNodeIslandHostedHereComponent {
    pub(crate) island: ShaderNodeIslandProgram,
    pub(crate) fused_group: Arc<ShaderNodeFusedGroupOfAnIsland>,
}

/// On a shader node fused into the island another node evaluates.
pub(crate) struct ShaderNodeFusedIntoTheIslandOfAnotherComponent;

/// Whether `node_id` is a shader node.
pub(crate) fn is_a_shader_node(graph: &Graph, node_id: &ProcessorUniqueId) -> bool {
    graph
        .traversal()
        .v(node_id)
        .first()
        .is_some_and(|node| node.node_kind() == Some(NodeKind::Shader))
}

/// The shader node `node_id` is fused into: the one shader node reading its
/// Image, when nothing else reads it.
fn the_shader_node_a_shader_node_is_fused_into(
    graph: &Graph,
    node_id: &ProcessorUniqueId,
) -> Option<ProcessorUniqueId> {
    if !is_a_shader_node(graph, node_id) {
        return None;
    }
    let links_out: Vec<&Link> = graph.traversal().v(node_id).out_e().iter().collect();
    let [only_link_out] = links_out.as_slice() else {
        return None;
    };
    let reader = only_link_out.to_port().processor_id();
    is_a_shader_node(graph, reader).then(|| reader.clone())
}

/// Whether `link` is a call inside one shader node island.
pub(crate) fn is_a_call_inside_a_shader_node_island(graph: &Graph, link: &Link) -> bool {
    the_shader_node_a_shader_node_is_fused_into(graph, link.source().processor_id()).is_some()
}

/// The node evaluating the island `node_id` belongs to; `node_id` itself when
/// its Image leaves its island.
pub(crate) fn the_host_of_the_shader_node_island_of(
    graph: &Graph,
    node_id: &ProcessorUniqueId,
) -> ProcessorUniqueId {
    let mut host = node_id.clone();
    let mut visited = HashSet::new();
    while visited.insert(host.clone()) {
        match the_shader_node_a_shader_node_is_fused_into(graph, &host) {
            Some(reader) => host = reader,
            None => break,
        }
    }
    host
}

/// Whether `node_id` is a shader node another node evaluates in its island.
pub(crate) fn is_fused_into_the_shader_node_island_of_another(
    graph: &Graph,
    node_id: &ProcessorUniqueId,
) -> bool {
    the_shader_node_a_shader_node_is_fused_into(graph, node_id).is_some()
}

/// The input port of its island's host that input `port` of fused shader node
/// `node_id` is read at.
pub(crate) fn the_host_input_port_a_fused_shader_node_input_is_read_at(
    host_id: &ProcessorUniqueId,
    node_id: &ProcessorUniqueId,
    port: &str,
) -> InputLinkPortRef {
    InputLinkPortRef::new(
        host_id.clone(),
        format!("shader_node_island_input_{node_id}_{port}"),
    )
}

/// Where a link from outside an island into one of its members is read: the
/// host's input port it is wired to.
pub(crate) fn where_a_link_into_a_fused_shader_node_is_read(
    graph: &Graph,
    link: &Link,
) -> Option<InputLinkPortRef> {
    let reader = link.to_port().processor_id();
    if is_a_call_inside_a_shader_node_island(graph, link)
        || !is_fused_into_the_shader_node_island_of_another(graph, reader)
    {
        return None;
    }
    Some(the_host_input_port_a_fused_shader_node_input_is_read_at(
        &the_host_of_the_shader_node_island_of(graph, reader),
        reader,
        link.to_port().port_name(),
    ))
}

/// Read the island hosted by `host_id`, members in evaluation order, each
/// member's inputs in signature order, with the node each member is.
pub(crate) fn the_shader_node_island_hosted_by(
    graph: &Graph,
    host_id: &ProcessorUniqueId,
) -> Result<(ShaderNodeIslandProgram, Vec<ProcessorUniqueId>)> {
    let mut members: Vec<ShaderNodeIslandMember> = Vec::new();
    let mut member_nodes_in_evaluation_order: Vec<ProcessorUniqueId> = Vec::new();
    add_the_member_after_every_member_it_reads(
        graph,
        host_id,
        host_id,
        &mut members,
        &mut member_nodes_in_evaluation_order,
    )?;
    let island = ShaderNodeIslandProgram::of_the_members(members).map_err(Error::Configuration)?;
    Ok((island, member_nodes_in_evaluation_order))
}

fn add_the_member_after_every_member_it_reads(
    graph: &Graph,
    host_id: &ProcessorUniqueId,
    node_id: &ProcessorUniqueId,
    members: &mut Vec<ShaderNodeIslandMember>,
    member_nodes_in_evaluation_order: &mut Vec<ProcessorUniqueId>,
) -> Result<usize> {
    let node = graph
        .traversal()
        .v(node_id)
        .first()
        .ok_or_else(|| Error::ProcessorNotFound(node_id.to_string()))?;
    let program = graph
        .node_types_this_stream_resolves()
        .shader_node_program_of(&node.processor_type)
        .ok_or_else(|| {
            Error::Configuration(format!(
                "shader node `{}` is of type `{}`, which this stream holds no shader program for",
                node.display_name, node.processor_type
            ))
        })?;
    let links_in: Vec<&Link> = graph.traversal().v(node_id).in_e().iter().collect();
    let mut inputs = Vec::with_capacity(program.inputs.len());
    for signature_input in &program.inputs {
        let link_in = links_in
            .iter()
            .find(|link| link.to_port().port_name() == signature_input.port);
        let read_at = if node_id == host_id {
            signature_input.port.clone()
        } else {
            the_host_input_port_a_fused_shader_node_input_is_read_at(
                host_id,
                node_id,
                &signature_input.port,
            )
            .port_name()
            .to_string()
        };
        let input = match signature_input.value_type {
            ShaderNodeInputValueType::ImageFrame => match link_in {
                Some(link) if is_a_call_inside_a_shader_node_island(graph, link) => {
                    ShaderNodeIslandMemberInput::ImageFromTheMember(
                        add_the_member_after_every_member_it_reads(
                            graph,
                            host_id,
                            link.source().processor_id(),
                            members,
                            member_nodes_in_evaluation_order,
                        )?,
                    )
                }
                _ => ShaderNodeIslandMemberInput::ImageReadAt(read_at),
            },
            ShaderNodeInputValueType::Float => ShaderNodeIslandMemberInput::FloatReadAt {
                input_port: read_at,
                value_until_one_is_read: pin_value_of_a_shader_node_input(
                    node,
                    &signature_input.port,
                ),
            },
        };
        inputs.push(input);
    }
    members.push(ShaderNodeIslandMember {
        node_name: node.display_name.clone(),
        program,
        inputs,
    });
    member_nodes_in_evaluation_order.push(node_id.clone());
    Ok(members.len() - 1)
}

/// The value input `port` of shader node `node` takes while nothing arrives
/// on it: its pin value, else its declared default, else zero.
pub(crate) fn pin_value_of_a_shader_node_input(node: &ProcessorNode, port: &str) -> f32 {
    node.pins
        .get(port)
        .or_else(|| {
            node.ports
                .inputs
                .iter()
                .find(|input| input.name == port)
                .and_then(|input| input.pin_default.as_ref())
        })
        .and_then(serde_json::Value::as_f64)
        .unwrap_or(0.0) as f32
}

/// Read each island among `processors_to_add` and hand it to its host, noting
/// every member's fused group; the shader nodes fused into another's island,
/// which have no processor of their own.
pub(crate) fn place_each_shader_node_island_on_its_host(
    graph: &mut Graph,
    processors_to_add: &[ProcessorUniqueId],
) -> Result<HashSet<ProcessorUniqueId>> {
    let mut fused_into_another: HashSet<ProcessorUniqueId> = HashSet::new();
    let mut islands_read = Vec::new();
    for node_id in processors_to_add {
        if !is_a_shader_node(graph, node_id) {
            continue;
        }
        if is_fused_into_the_shader_node_island_of_another(graph, node_id) {
            if is_fused_into_the_shader_node_island_of_another(
                graph,
                &the_host_of_the_shader_node_island_of(graph, node_id),
            ) {
                return Err(Error::Configuration(format!(
                    "shader node `{node_id}` reads its own Image through the shader nodes it \
                     feeds, so no node evaluates their island. To fix: break the cycle"
                )));
            }
            fused_into_another.insert(node_id.clone());
            continue;
        }
        islands_read.push((
            node_id.clone(),
            the_shader_node_island_hosted_by(graph, node_id)?,
        ));
    }
    for (host_id, (island, member_ids)) in islands_read {
        let host_name = island.output_member().node_name.clone();
        let member_names: Vec<String> = island
            .members()
            .iter()
            .map(|member| member.node_name.clone())
            .collect();
        let fused_group = Arc::new(ShaderNodeFusedGroupOfAnIsland::of_the_members(
            host_name,
            member_names,
        ));
        for member_id in member_ids {
            let Some(member) = graph.traversal_mut().v(&member_id).first_mut() else {
                continue;
            };
            member.insert(ShaderNodeFusedGroupComponent(Arc::clone(&fused_group)));
            if member_id != host_id {
                member.insert_component_without_rendering_it(
                    ShaderNodeFusedIntoTheIslandOfAnotherComponent,
                );
            }
        }
        if let Some(host) = graph.traversal_mut().v(&host_id).first_mut() {
            host.insert_component_without_rendering_it(ShaderNodeIslandHostedHereComponent {
                island,
                fused_group,
            });
        }
    }
    Ok(fused_into_another)
}

/// Refuse a link added or removed while the stream runs that would change a
/// shader node island already evaluating: one between two shader nodes, or
/// one into or out of a node fused into another's island. A link going with
/// a node the same edit removes changes no island that keeps running.
pub(crate) fn refuse_a_live_edit_of_a_shader_node_island(
    graph: &Graph,
    processors_to_add: &[ProcessorUniqueId],
    processors_to_remove: &[ProcessorUniqueId],
    links_changed: &[LinkUniqueId],
) -> Result<()> {
    let already_running = |node_id: &ProcessorUniqueId| !processors_to_add.contains(node_id);
    for link_id in links_changed {
        let Some(link) = graph.traversal().e(link_id).first() else {
            continue;
        };
        let (source_id, reader_id) = (link.source().processor_id(), link.to_port().processor_id());
        if processors_to_remove.contains(source_id) || processors_to_remove.contains(reader_id) {
            continue;
        }
        let ends_in_a_running_island = [source_id, reader_id].into_iter().find(|node_id| {
            already_running(node_id)
                && graph.traversal().v(*node_id).first().is_some_and(|node| {
                    node.has::<ShaderNodeFusedIntoTheIslandOfAnotherComponent>()
                })
        });
        let joins_running_shader_nodes = is_a_shader_node(graph, source_id)
            && is_a_shader_node(graph, reader_id)
            && (already_running(source_id) || already_running(reader_id));
        if let Some(node_id) =
            ends_in_a_running_island.or(joins_running_shader_nodes.then_some(source_id))
        {
            let node_name = graph
                .traversal()
                .v(node_id)
                .first()
                .map_or_else(|| node_id.to_string(), |node| node.display_name.clone());
            return Err(Error::Configuration(format!(
                "link `{link_id}` changes the fused group of running shader node `{node_name}`, \
                 and a fused group is read once, when its nodes start. To fix: stop the stream, \
                 make the edit, and run it again"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::function_node_island::tests::{add_a_node, link};
    use super::super::function_node_island::{HowALinkIsWired, how_a_link_is_wired};
    use super::super::shader_node_island_program::tests::shader_program;
    use super::*;
    use crate::core::descriptors::{
        PortDescriptor, PortFamily, PortFamilyDeclaration, ProcessorClassImportPath,
        ProcessorClassShortName, ProcessorDescriptor,
    };
    use crate::core::processors::{HowANodeOfADescribedTypeRuns, NodeTypesOneStreamResolves};

    fn a_descriptor(
        import_path: &str,
        node_kind: NodeKind,
        inputs: &[(&str, PortFamily)],
        outputs: &[&str],
    ) -> ProcessorDescriptor {
        let import_path = ProcessorClassImportPath::new(import_path).unwrap();
        let short_name = import_path.as_str().rsplit(':').next().unwrap().to_string();
        let mut descriptor = ProcessorDescriptor::new(
            ProcessorClassShortName::new(short_name).unwrap(),
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

    fn a_constructor_that_builds_nothing() -> crate::core::processors::DynamicProcessorConstructorFn
    {
        Box::new(|node| {
            Err(Error::Configuration(format!(
                "`{}` builds no processor in this test",
                node.id
            )))
        })
    }

    /// A graph resolving `source` and `sink`, two process types carrying
    /// Images, and `tint`, `blur` and `blend`, three shader types.
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
                .register_a_type_described_in_this_streams_processor_interpreter(
                    descriptor,
                    HowANodeOfADescribedTypeRuns::AsAProcessorItsConstructorBuilds(
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
                .register_a_type_described_in_this_streams_processor_interpreter(
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
        crate::core::test_support::ensure_test_mocks_registered();
        Graph::new_resolving_node_types_through(Arc::new(node_types))
    }

    fn add(graph: &mut Graph, import_path: &str) -> ProcessorUniqueId {
        add_a_node(graph, import_path, serde_json::json!({}))
    }

    struct SourceIntoTintIntoBlurIntoTintIntoSink {
        graph: Graph,
        source: ProcessorUniqueId,
        tint: ProcessorUniqueId,
        blur: ProcessorUniqueId,
        tint_again: ProcessorUniqueId,
        sink: ProcessorUniqueId,
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
        let fused =
            place_each_shader_node_island_on_its_host(&mut stream.graph, &every_node).unwrap();
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

    #[test]
    fn a_live_link_into_a_running_island_is_refused_and_a_teardown_is_not() {
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
        let refusal = refuse_a_live_edit_of_a_shader_node_island(
            &stream.graph,
            std::slice::from_ref(&second_sink),
            &[],
            std::slice::from_ref(&into_a_fused_node_output),
        )
        .unwrap_err()
        .to_string();
        assert!(refusal.contains("changes the fused group"), "{refusal}");
        assert!(refusal.contains("stop the stream"), "{refusal}");

        refuse_a_live_edit_of_a_shader_node_island(&stream.graph, &[], &every_node, &stream.links)
            .unwrap();
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
}
