// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The edits a stream's graph document holds that its running graph did not take: each kept as
//! a diagnostic at the part it changes, `graph` answering the document's graph over the live one
//! while any is held, until the stream is brought to its document once its playback stops.

use std::collections::HashMap;
use std::sync::Arc;

use streamlib_project_repository::StreamGraphOperation;
use streamlib_runtime_client_contract::local_api_wire_contract::{
    DiagnosticKindOnTheWire, DiagnosticOnTheWire, DiagnosticPlaceOnTheWire,
};
use uuid::Uuid;

use super::LoadedStreamInThisRuntime;
use crate::core::graph::graph_fragment_flattening::{cast_or_as_written, rendered_as_stored};
use crate::core::graph::graph_snapshot::GraphSnapshot;
use crate::core::json_schema::{
    GraphNodeOutput, GraphNodePortOnTheWire, GraphNodePortsOnTheWire, GraphResponse, LinkOutput,
    LinkPortRefOutput, redraw_a_node_over_its_pins,
};
use crate::core::runtime::streams_loaded_in_this_runtime::live_graph_reconciled_to_a_graph_document::LinksAReconcileMakes;
use crate::core::runtime::streams_loaded_in_this_runtime::stream_actions_of_this_runtime::{
    the_graph_document_of_the_stream_at, the_graph_the_document_of_the_stream_at_holds,
};

impl LoadedStreamInThisRuntime {
    /// Hold `operation`, which this stream's graph document holds and its running graph refused
    /// in the runtime's words `refusal`, as an edit not live yet, at the part it changes.
    pub fn hold_an_edit_its_document_holds_not_live(
        &self,
        operation: &StreamGraphOperation,
        refusal: &str,
    ) {
        let diagnostic = DiagnosticOnTheWire::of(
            DiagnosticKindOnTheWire::EditNotLive,
            self.the_place_an_edit_changes(operation),
            format!(
                "its graph document holds an edit its running graph did not take, which it \
                 takes when its playback stops: {refusal}"
            ),
        );
        self.edits_its_document_holds_not_live
            .lock()
            .push(diagnostic);
    }

    /// Each edit this stream's graph document holds that its running graph did not take, as the
    /// diagnostic at the part it changes; empty while its running graph holds its document.
    pub fn the_edits_its_document_holds_not_live(&self) -> Vec<DiagnosticOnTheWire> {
        self.edits_its_document_holds_not_live.lock().clone()
    }

    /// Bring this stream, its playback stopped, to the graph its document holds when that holds
    /// edits its running graph did not take, and forget them; a document that does not read, or
    /// that the stopped graph does not reconcile to, is logged and its edits kept.
    pub(in crate::core::runtime) fn bring_the_stopped_stream_to_its_document(self: &Arc<Self>) {
        if self.edits_its_document_holds_not_live.lock().is_empty() {
            return;
        }
        let brought = the_graph_document_it_holds_on_disk(self).and_then(|graph_document| {
            self.reconcile_the_live_graph_to_the_graph_document_from_synchronous_code(
                graph_document,
                LinksAReconcileMakes::EachAsASavedGraphsLink,
            )
            .map_err(|refusal| refusal.to_string())
        });
        match brought {
            Ok(()) => {
                self.forget_the_edits_its_document_holds_not_live();
                tracing::info!(
                    "the stream `{}` stopped and took the edits its graph document holds",
                    self.stream_id()
                );
            }
            Err(why) => tracing::warn!(
                "the stream `{}` stopped and did not take the edits its graph document holds, \
                 which it keeps as not live: {why}",
                self.stream_id()
            ),
        }
    }

    /// Forget every edit held not live: the running graph holds the stream's document again.
    pub(in crate::core::runtime) fn forget_the_edits_its_document_holds_not_live(&self) {
        self.edits_its_document_holds_not_live.lock().clear();
    }

    /// Answer, in `graph_response`, which renders the running graph, the graph this stream's
    /// document holds while it holds edits the running graph did not take: each node and link
    /// the running graph also holds as it renders them, under the document's names, and each
    /// other drawn as the document writes it, carrying `live: false`. A document that does not
    /// read leaves the running graph's answer, logged.
    pub(super) fn render_the_document_over_the_live_graph_while_edits_are_not_live(
        &self,
        graph_response: &mut GraphResponse,
    ) {
        if self.edits_its_document_holds_not_live.lock().is_empty() {
            return;
        }
        let document = match the_graph_the_document_of_the_stream_at_holds(self.stream_location()) {
            Ok(document) => document,
            Err(why) => {
                tracing::warn!(
                    "the stream `{}` holds edits not live, and its graph document does not read, \
                     so `graph` answers its running graph: {why}",
                    self.stream_id()
                );
                return;
            }
        };
        render_the_document_over_the_live_graph(
            graph_response,
            &document,
            |node_type| {
                self.node_types_this_stream_resolves
                    .port_info(node_type)
                    .map(|ports| GraphNodePortsOnTheWire {
                        inputs: ports
                            .inputs
                            .iter()
                            .map(GraphNodePortOnTheWire::from)
                            .collect(),
                        outputs: ports
                            .outputs
                            .iter()
                            .map(GraphNodePortOnTheWire::from)
                            .collect(),
                    })
            },
            |node| rendered_as_stored(node, &self.node_types_this_stream_resolves),
        );
    }

    /// Where `operation` applies: the link it connects or disconnects, the node it acts on, or
    /// else the stream; each node named as the running graph names it.
    fn the_place_an_edit_changes(
        &self,
        operation: &StreamGraphOperation,
    ) -> DiagnosticPlaceOnTheWire {
        let name_of = |node: Uuid| {
            self.compiler
                .scope_reading(|graph| {
                    graph
                        .traversal()
                        .v(())
                        .iter()
                        .find(|held| held.node_uuid == node)
                        .map(|held| held.display_name.clone())
                })
                .unwrap_or_else(|| node.to_string())
        };
        match operation {
            StreamGraphOperation::Connect { link } | StreamGraphOperation::Disconnect { link } => {
                DiagnosticPlaceOnTheWire::Link {
                    from_node: name_of(link.source.node),
                    from_port: link.source.port.clone(),
                    to_node: name_of(link.target.node),
                    to_port: link.target.port.clone(),
                }
            }
            StreamGraphOperation::AddNode { node, .. } if !node.name.is_empty() => {
                DiagnosticPlaceOnTheWire::Node {
                    node_id: node.id.to_string(),
                    node_name: node.name.clone(),
                }
            }
            _ => match operation.node() {
                Some(node) => DiagnosticPlaceOnTheWire::Node {
                    node_id: node.to_string(),
                    node_name: name_of(node),
                },
                None => DiagnosticPlaceOnTheWire::Stream,
            },
        }
    }
}

/// The graph document of `stream`, as its project holds it on disk now.
fn the_graph_document_it_holds_on_disk(
    stream: &LoadedStreamInThisRuntime,
) -> std::result::Result<serde_json::Value, String> {
    let (_, stream_document) = the_graph_document_of_the_stream_at(stream.stream_location())?;
    let bytes = stream_document
        .bytes
        .as_ref()
        .map_err(|why| format!("its graph document does not read: {why}"))?;
    serde_json::from_slice(bytes).map_err(|why| format!("its graph document is not JSON: {why}"))
}

/// Answer `document` in `graph_response`, which renders the running graph: each node of the
/// document's own level the running graph holds by its UUID as it renders it, under the
/// document's name, pins and studio fields, and each other as `drawn_as_stored` draws it with
/// the ports `declared_ports` answers for its type, carrying `live: false`; each link the running
/// graph holds between the same ports of the same nodes as it renders it, and each other as the
/// document draws it, pending and carrying `live: false`. A node the running graph holds and the
/// document does not is left out.
pub(crate) fn render_the_document_over_the_live_graph(
    graph_response: &mut GraphResponse,
    document: &GraphSnapshot,
    declared_ports: impl Fn(
        &crate::core::descriptors::NodeClassImportPath,
    ) -> Option<GraphNodePortsOnTheWire>,
    drawn_as_stored: impl Fn(&crate::core::graph::graph_snapshot::GraphSnapshotNode) -> GraphNodeOutput,
) {
    let live_id_of_each_name: HashMap<String, String> = graph_response
        .nodes
        .iter()
        .map(|node| (node.name.clone(), node.id.clone()))
        .collect();
    let mut live_node_of_each_id: HashMap<String, GraphNodeOutput> =
        std::mem::take(&mut graph_response.nodes)
            .into_iter()
            .map(|node| (node.id.clone(), node))
            .collect();
    let mut document_id_of_each_name: HashMap<String, String> = HashMap::new();
    graph_response.nodes = document
        .nodes
        .iter()
        .map(|node| {
            let id = node
                .id
                .map(|node_uuid| node_uuid.to_string())
                .unwrap_or_default();
            let rendered = match live_node_of_each_id.remove(&id) {
                Some(mut live) => {
                    redraw_a_node_over_its_pins(&mut live, &node.input_values);
                    live.studio = node.studio.clone();
                    live.name = cast_or_as_written(&node.name);
                    live
                }
                None => {
                    let mut drawn = drawn_as_stored(node);
                    if let Some(ports) = declared_ports(&node.node_type) {
                        drawn.ports = ports;
                    }
                    drawn.live = Some(false);
                    drawn
                }
            };
            document_id_of_each_name.insert(rendered.name.clone(), id);
            rendered
        })
        .collect();
    let ends_of = |source: &LinkPortRefOutput,
                   target: &LinkPortRefOutput,
                   id_of_each_name: &HashMap<String, String>| {
        (
            id_of_each_name.get(&source.node).cloned(),
            source.port.clone(),
            id_of_each_name.get(&target.node).cloned(),
            target.port.clone(),
        )
    };
    let mut live_link_of_each_ends: HashMap<_, LinkOutput> =
        std::mem::take(&mut graph_response.links)
            .into_iter()
            .map(|link| {
                (
                    ends_of(&link.source, &link.target, &live_id_of_each_name),
                    link,
                )
            })
            .collect();
    graph_response.links = document
        .links
        .iter()
        .map(|drawn| {
            let source = LinkPortRefOutput {
                node: cast_or_as_written(&drawn.source.node),
                port: drawn.source.port.clone(),
            };
            let target = LinkPortRefOutput {
                node: cast_or_as_written(&drawn.target.node),
                port: drawn.target.port.clone(),
            };
            match live_link_of_each_ends.remove(&ends_of(
                &source,
                &target,
                &document_id_of_each_name,
            )) {
                Some(live) => LinkOutput {
                    source,
                    target,
                    ..live
                },
                None => LinkOutput {
                    source,
                    target,
                    live: Some(false),
                    ..LinkOutput::default()
                },
            }
        })
        .collect();
}
