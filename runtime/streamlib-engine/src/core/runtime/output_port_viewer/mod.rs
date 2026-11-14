// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A viewer's read of any output port on the machine: the runtime holds the
//! port's channel open and hands the viewer what it needs to subscribe to the
//! channel's reserved slot from its own process, reading the latest message
//! the port's node wrote without changing the stream's graph.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use streamlib_processor_schema::PortFamily;
use streamlib_runtime_client_contract::local_api_wire_contract::OutputPortViewerReads;

use super::LoadedStreamInThisRuntime;
use super::node_of_a_stream_not_running::the_refusal_of_a_reach_for_a_node_not_running;
use crate::core::compiler::compiler_ops::{
    SourceSideHeldOpenForAReaderOnTheReservedSlot,
    open_the_source_side_for_a_reader_on_the_reserved_slot,
};
use crate::core::error::{Error, Result, WhyANodeOfAStreamIsNotRunning};
use crate::core::graph::{
    Graph, OutputLinkPortRef, ProcessorNode, names_listed_for_a_refusal,
    port_names_listed_for_a_refusal,
};
use crate::core::processors::{OutOfProcessLinkWireOutcome, OutOfProcessLinkWireReply};

/// How long an open waits for a helper process to open the port it runs.
const A_HELPER_SOURCE_OPENS_ITS_PORT_WITHIN: Duration = Duration::from_secs(10);

/// How often an open asks whether a helper source has answered.
const HOW_OFTEN_AN_OPEN_ASKS_FOR_A_HELPERS_ANSWER: Duration = Duration::from_millis(10);

/// Whether the source of a viewed port has opened its end of the channel.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SourceWiringOfAViewedOutputPort {
    /// The source publishes on the channel.
    Wired,
    /// The source's helper process has not yet said whether it opened its port.
    AwaitingItsHelpersAnswer,
    /// The source's helper process could not open its port, and said why.
    RefusedByItsHelper { reason: String },
}

/// What a helper source's answer, or its absence for a source with nothing to
/// answer, says of the port's wiring.
fn the_source_wiring_its_answer_says(
    wire_reply: Option<&OutOfProcessLinkWireReply>,
) -> SourceWiringOfAViewedOutputPort {
    let Some(wire_reply) = wire_reply else {
        return SourceWiringOfAViewedOutputPort::Wired;
    };
    match wire_reply.the_far_sides_answer() {
        None => SourceWiringOfAViewedOutputPort::AwaitingItsHelpersAnswer,
        Some(OutOfProcessLinkWireOutcome::OpenedByTheFarSide) => {
            SourceWiringOfAViewedOutputPort::Wired
        }
        Some(OutOfProcessLinkWireOutcome::RefusedByTheFarSide { reason }) => {
            SourceWiringOfAViewedOutputPort::RefusedByItsHelper { reason }
        }
    }
}

/// What a viewer needs to read an output port's channel from its own process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPortViewerChannelHandOff {
    /// The stream the port belongs to.
    pub stream: String,
    /// The UUID of the node the port belongs to, as the stream's graph lists it under `id`.
    pub node_id: String,
    /// The name of the node the port belongs to: its label, not its identity.
    pub node_name: String,
    /// The port's name.
    pub port: String,
    /// The root of the iceoryx2 domain the channel lives in.
    pub iceoryx2_domain_root: PathBuf,
    /// The channel's iceoryx2 service name.
    pub channel_service_name: String,
    /// The iceoryx2 notify service the port's node notifies on each write,
    /// whose one listener the viewer takes.
    pub wake_service_name: String,
    /// Which of the port's messages the viewer reads: every one for a Channels
    /// port, so a viewer can play its sound, the latest for any other family.
    pub viewer_reads: OutputPortViewerReads,
    /// Where the viewer imports an Image frame's surface, as a helper process
    /// does: the surface-sharing socket's path on Linux, the surface-sharing
    /// Mach service's name on macOS. `None` on macOS when no viewer process
    /// was admitted, which leaves the viewer nothing it may connect to.
    pub surface_share_address: Option<String>,
}

/// What lets a viewer's process import an Image frame's surface while the
/// viewer stays open.
struct SurfaceShareAccessOfAViewer {
    surface_share_address: Option<String>,
    #[cfg(target_os = "macos")]
    _admission_of_the_viewer_process:
        Option<crate::apple::surface_share::SurfaceShareHelperProcessAdmission>,
}

impl SurfaceShareAccessOfAViewer {
    /// The access process `process_id_of_the_viewer` gets: on Linux, the
    /// socket its file permission already opens to this user; on macOS, the
    /// Mach service once the process is admitted, refused unless it is a
    /// running process of this user.
    fn granted_to(
        engine_resources: &super::runtime::EngineResourcesSharedByEveryStream,
        process_id_of_the_viewer: Option<u32>,
    ) -> Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let _ = process_id_of_the_viewer;
            Ok(Self {
                surface_share_address: Some(engine_resources.surface_share_address()),
            })
        }
        #[cfg(target_os = "macos")]
        {
            let Some(viewer_process_id) = process_id_of_the_viewer else {
                return Ok(Self {
                    surface_share_address: None,
                    _admission_of_the_viewer_process: None,
                });
            };
            let admission = crate::apple::surface_share::admit_a_viewer_process(
                &engine_resources.surface_share_mach_service_rendezvous,
                viewer_process_id,
            )?;
            Ok(Self {
                surface_share_address: Some(engine_resources.surface_share_address()),
                _admission_of_the_viewer_process: Some(admission),
            })
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (engine_resources, process_id_of_the_viewer);
            Ok(Self {
                surface_share_address: None,
            })
        }
    }
}

/// A viewer's read of one output port, held open by the runtime: the port's
/// channel and its source's side stay open until this drops.
pub struct OutputPortViewerHeldOpen {
    hand_off: OutputPortViewerChannelHandOff,
    source_side: SourceSideHeldOpenForAReaderOnTheReservedSlot,
    _surface_share_access_of_the_viewer: SurfaceShareAccessOfAViewer,
}

impl std::fmt::Debug for OutputPortViewerHeldOpen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputPortViewerHeldOpen")
            .field("hand_off", &self.hand_off)
            .finish_non_exhaustive()
    }
}

impl OutputPortViewerHeldOpen {
    /// What the viewer subscribes with.
    pub fn hand_off(&self) -> &OutputPortViewerChannelHandOff {
        &self.hand_off
    }

    fn source_wiring(&self) -> SourceWiringOfAViewedOutputPort {
        the_source_wiring_its_answer_says(
            self.source_side
                .wire_reply_awaited_from_the_source
                .as_deref(),
        )
    }
}

impl LoadedStreamInThisRuntime {
    /// Hold output port `port_name` of the node whose UUID is `node_id`, as the stream's graph
    /// lists it under `id`, open for a viewer and
    /// say how to subscribe to its channel's reserved slot. The graph is not
    /// changed. Answers once the port's source has opened its end of the
    /// channel, and refused while the node does not run or when its helper
    /// process could not open the port. On macOS, process
    /// `process_id_of_the_viewer` may open new connections to the
    /// surface-sharing service while the viewer stays open, and is refused
    /// unless it is a running process of this user.
    pub(crate) async fn open_a_viewer_of_an_output_port_holding_its_channel(
        &self,
        node_id: &str,
        port_name: &str,
        process_id_of_the_viewer: Option<u32>,
    ) -> Result<OutputPortViewerHeldOpen> {
        let engine_resources = self.engine_resources_shared_by_every_stream();
        let surface_share_access_of_the_viewer =
            SurfaceShareAccessOfAViewer::granted_to(engine_resources, process_id_of_the_viewer)?;
        let iceoryx2_node = engine_resources.iceoryx2_node.clone();
        let iceoryx2_domain_root = engine_resources.runtime_directory.iceoryx2_domain_root();
        let compiler = Arc::clone(&self.compiler);
        let stream_id = self.stream_id().to_string();
        let node_id = node_id.to_string();
        let port_name = port_name.to_string();
        let why_a_node_that_has_not_failed_does_not_run =
            self.why_a_node_of_this_stream_that_has_not_failed_does_not_run();
        // A stream's graph lock is held across a compile, which may take a
        // processor's whole setup, so the open waits off the async workers.
        let viewer_held_open = tokio::task::spawn_blocking(
            crate::core::logging::carrying_this_threads_project_log_route_entry(move || {
                let (node_name, viewer_reads, source_side) =
                    compiler.scope(|graph, _tx| -> Result<_> {
                        let node = the_running_node_with_its_output_port(
                            graph,
                            &stream_id,
                            &node_id,
                            &port_name,
                            why_a_node_that_has_not_failed_does_not_run,
                        )?;
                        let node_name = node.display_name.clone();
                        let viewer_reads =
                            what_a_viewer_of_the_port_reads(graph, &node.id, &port_name);
                        let source = OutputLinkPortRef::new(node.id.clone(), port_name.as_str());
                        let source_side = open_the_source_side_for_a_reader_on_the_reserved_slot(
                            graph,
                            &source,
                            &iceoryx2_node,
                        )?;
                        Ok((node_name, viewer_reads, source_side))
                    })?;
                Ok::<_, Error>(OutputPortViewerHeldOpen {
                    hand_off: OutputPortViewerChannelHandOff {
                        stream: stream_id,
                        node_id,
                        node_name,
                        port: port_name,
                        iceoryx2_domain_root,
                        channel_service_name: source_side.channel_service_name.clone(),
                        wake_service_name: source_side.wake_service_name.clone(),
                        viewer_reads,
                        surface_share_address: surface_share_access_of_the_viewer
                            .surface_share_address
                            .clone(),
                    },
                    source_side,
                    _surface_share_access_of_the_viewer: surface_share_access_of_the_viewer,
                })
            }),
        )
        .await
        .map_err(|join_failure| {
            Error::Runtime(format!(
                "a viewer's open of a port failed to join: {join_failure}"
            ))
        })??;
        Self::once_its_source_opened_its_end(viewer_held_open).await
    }

    async fn once_its_source_opened_its_end(
        viewer_held_open: OutputPortViewerHeldOpen,
    ) -> Result<OutputPortViewerHeldOpen> {
        let gave_up_at = tokio::time::Instant::now() + A_HELPER_SOURCE_OPENS_ITS_PORT_WITHIN;
        let refusal = loop {
            match viewer_held_open.source_wiring() {
                SourceWiringOfAViewedOutputPort::Wired => return Ok(viewer_held_open),
                SourceWiringOfAViewedOutputPort::RefusedByItsHelper { reason } => {
                    break format!("its helper process could not open the port: {reason}");
                }
                SourceWiringOfAViewedOutputPort::AwaitingItsHelpersAnswer
                    if tokio::time::Instant::now() >= gave_up_at =>
                {
                    break format!(
                        "its helper process did not open the port within {}s",
                        A_HELPER_SOURCE_OPENS_ITS_PORT_WITHIN.as_secs()
                    );
                }
                SourceWiringOfAViewedOutputPort::AwaitingItsHelpersAnswer => {
                    tokio::time::sleep(HOW_OFTEN_AN_OPEN_ASKS_FOR_A_HELPERS_ANSWER).await
                }
            }
        };
        let hand_off = viewer_held_open.hand_off.clone();
        tokio::task::spawn_blocking(move || drop(viewer_held_open))
            .await
            .map_err(|join_failure| {
                Error::Runtime(format!(
                    "a refused viewer's release failed to join: {join_failure}"
                ))
            })?;
        Err(Error::Link(format!(
            "port `{}` of node `{}` ({}) of the stream `{}` opens no viewer: {refusal}",
            hand_off.port, hand_off.node_id, hand_off.node_name, hand_off.stream
        )))
    }
}

/// The node of the stream's graph whose UUID is `node_id`, refused unless it runs and has
/// output port `port_name`; a node that does not run is refused for its failure, or else for
/// `why_a_node_that_has_not_failed_does_not_run`.
fn the_running_node_with_its_output_port<'graph>(
    graph: &'graph Graph,
    stream_id: &str,
    node_id: &str,
    port_name: &str,
    why_a_node_that_has_not_failed_does_not_run: WhyANodeOfAStreamIsNotRunning,
) -> Result<&'graph ProcessorNode> {
    let Some(node) = graph
        .traversal()
        .v(())
        .iter()
        .find(|node| node.node_uuid.to_string() == node_id)
    else {
        return Err(Error::ProcessorNotFound(format!(
            "no node in the stream `{stream_id}` has the id `{node_id}`. The stream \
             `{stream_id}` holds: {}",
            names_listed_for_a_refusal(
                graph
                    .traversal()
                    .v(())
                    .iter()
                    .map(|node| format!("`{}` ({})", node.node_uuid, node.display_name)),
                "no node",
            )
        )));
    };
    if !node.has_output(port_name) {
        return Err(Error::NodeInAStreamHasNoOutputPortNamed {
            stream: stream_id.to_string(),
            node: node.display_name.clone(),
            port: port_name.to_string(),
            output_ports_the_node_has: port_names_listed_for_a_refusal(
                node.ports.outputs.iter().map(|output| output.name.as_str()),
            ),
        });
    }
    if !node.runs_its_processor() {
        return Err(the_refusal_of_a_reach_for_a_node_not_running(
            node,
            why_a_node_that_has_not_failed_does_not_run,
            format!(
                "node `{node_id}` ({}) of the stream `{stream_id}` is not running, so its port \
                 `{port_name}` writes nothing yet",
                node.display_name,
            ),
        ));
    }
    Ok(node)
}

/// What a viewer of output `port_name` of `node_id` reads: every message for a
/// port carrying Channels, the latest for any other family or one not settled.
fn what_a_viewer_of_the_port_reads(
    graph: &crate::core::graph::Graph,
    node_id: &crate::core::ProcessorUniqueId,
    port_name: &str,
) -> OutputPortViewerReads {
    match crate::core::graph::the_family_a_link_out_of_an_output_carries(graph, node_id, port_name)
    {
        Some(declaration) if declaration.family == PortFamily::Channels => {
            OutputPortViewerReads::EveryMessage
        }
        _ => OutputPortViewerReads::LatestMessage,
    }
}

#[cfg(test)]
mod tests;
