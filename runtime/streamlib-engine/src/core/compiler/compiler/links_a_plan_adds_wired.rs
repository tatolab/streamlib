// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The wire phase of a compile: each link its plan adds, wired the way its ends take it.

use parking_lot::RwLock;

use super::*;

/// Wire each of `links_to_add` in `graph_arc`, telling the stream's topic
/// before and after; how many were wired. A link whose ends no longer carry
/// types that connect stays unwired, in error; one of `links_to_wire_again`
/// that cannot be wired stays refused; any other that cannot be wired fails
/// the phase.
pub(super) fn wire_every_link_a_plan_adds(
    graph_arc: &RwLock<Graph>,
    links_to_add: &[LinkUniqueId],
    links_to_wire_again: &[LinkUniqueId],
    iceoryx2_node: &Iceoryx2Node,
    the_stream_the_compile_belongs_to: &LoadedStreamIdentity,
) -> Result<usize> {
    let mut links_wired = 0;
    for link_id in links_to_add {
        if graph_arc.read().the_pin_types_of_the_link_differ(link_id) {
            links_whose_pin_types_differ::stamp_the_link_in_error(&mut graph_arc.write(), link_id);
            continue;
        }
        let (from_port, to_port) = {
            let graph = graph_arc.read();
            let link = graph
                .traversal()
                .e(link_id)
                .first()
                .ok_or_else(|| Error::LinkNotFound(format!("Link '{}' not found", link_id)))?;
            (link.source().to_string(), link.to_port().to_string())
        };

        the_stream_the_compile_belongs_to.publish_on_this_streams_topic(
            RuntimeEvent::CompilerWillWireLink {
                link_id: link_id.to_string(),
                from_port: from_port.clone(),
                to_port: to_port.clone(),
            },
        );
        match wire_one_link(graph_arc, link_id, iceoryx2_node) {
            Ok(()) => {}
            Err(refusal) if links_to_wire_again.contains(link_id) => {
                tracing::warn!(
                    "[{}] {} an end refused could not be wired again, so it stays refused: \
                     {refusal}",
                    CompilePhase::Wire,
                    link_id
                );
                links_an_end_refused_wired_again::hold_the_link_refused_because(
                    &mut graph_arc.write(),
                    link_id,
                    &refusal.to_string(),
                );
                continue;
            }
            Err(refusal) => return Err(refusal),
        }

        the_stream_the_compile_belongs_to.publish_on_this_streams_topic(
            RuntimeEvent::CompilerDidWireLink {
                link_id: link_id.to_string(),
                from_port,
                to_port,
            },
        );

        links_wired += 1;
    }
    Ok(links_wired)
}

/// Wire link `link_id` the way its ends take it.
fn wire_one_link(
    graph_arc: &RwLock<Graph>,
    link_id: &LinkUniqueId,
    iceoryx2_node: &Iceoryx2Node,
) -> Result<()> {
    let how_the_link_is_wired = how_a_link_is_wired(&graph_arc.read(), link_id)?;
    match how_the_link_is_wired {
        HowALinkIsWired::ThroughAChannel => {
            tracing::info!("[{}] Opening service {}", CompilePhase::Wire, link_id);
            crate::core::compiler::compiler_ops::open_iceoryx2_service(
                &mut graph_arc.write(),
                link_id,
                iceoryx2_node,
            )?;
        }
        HowALinkIsWired::AsACallInsideAFunctionNodeIsland => {
            wire_a_call_inside_a_function_node_island(&mut graph_arc.write(), link_id)?;
        }
        HowALinkIsWired::AsTheIslandItsReaderEvaluatesAtEachRead => {
            tracing::info!(
                "[{}] Carrying the function node island of {} to its reader",
                CompilePhase::Wire,
                link_id
            );
            wire_a_link_from_a_function_node_island_into_its_reader(
                &mut graph_arc.write(),
                link_id,
                iceoryx2_node,
            )?;
        }
        HowALinkIsWired::ThroughAChannelReadByTheReaderOfItsFunctionNodeIsland => {
            wire_a_link_into_a_function_node_island_through_its_reader(
                &mut graph_arc.write(),
                link_id,
                iceoryx2_node,
            )?;
        }
        HowALinkIsWired::AsACallInsideAShaderNodeIsland => {
            let mut graph = graph_arc.write();
            if let Some(link) = graph.traversal_mut().e(link_id).first_mut() {
                link.insert(LinkStateComponent(LinkState::Wired));
            }
        }
        HowALinkIsWired::ThroughAChannelIntoTheHostOfItsShaderNodeIsland(host_input_port) => {
            tracing::info!(
                "[{}] Opening service {} into {}",
                CompilePhase::Wire,
                link_id,
                host_input_port
            );
            crate::core::compiler::compiler_ops::open_iceoryx2_service_into_the_host_of_a_shader_node_island(
                &mut graph_arc.write(),
                link_id,
                &host_input_port,
                iceoryx2_node,
            )?;
        }
    }
    Ok(())
}
