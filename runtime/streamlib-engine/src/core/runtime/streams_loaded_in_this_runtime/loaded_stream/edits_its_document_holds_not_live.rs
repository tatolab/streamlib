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
    GraphNodeOutput, GraphNodePortsOnTheWire, GraphResponse, LinkOutput, LinkPortRefOutput,
    redraw_a_node_over_its_pins,
};
use crate::core::nodes::NodeTypesOneStreamResolves;
use crate::core::runtime::streams_loaded_in_this_runtime::live_graph_reconciled_to_a_graph_document::LinksAReconcileMakes;
use crate::core::runtime::streams_loaded_in_this_runtime::stream_actions_of_this_runtime::{
    the_graph_document_json_of_the_stream_at, the_graph_the_document_of_the_stream_at_holds,
};

impl LoadedStreamInThisRuntime {
    /// Hold `operation`, which this stream's graph document holds and its running graph refused
    /// in the runtime's words `refusal`, as an edit not live yet, at the part it changes.
    pub(in crate::core::runtime) fn hold_an_edit_its_document_holds_not_live(
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
    pub(in crate::core::runtime) fn the_edits_its_document_holds_not_live(
        &self,
    ) -> Vec<DiagnosticOnTheWire> {
        self.edits_its_document_holds_not_live.lock().clone()
    }

    /// Bring this stream, its playback stopped, to the graph its document holds when that holds
    /// edits its running graph did not take, and forget them; a document that does not read, or
    /// that the stopped graph does not reconcile to, is logged and its edits kept. An edit held
    /// while the document is read is kept for the next stop.
    pub(in crate::core::runtime) fn bring_the_stopped_stream_to_its_document(self: &Arc<Self>) {
        let edits_taken_back = std::mem::take(&mut *self.edits_its_document_holds_not_live.lock());
        if edits_taken_back.is_empty() {
            return;
        }
        let brought = the_graph_document_json_of_the_stream_at(self.stream_location()).and_then(
            |graph_document| {
                self.reconcile_the_live_graph_to_the_graph_document_from_synchronous_code(
                    graph_document,
                    LinksAReconcileMakes::EachAsASavedGraphsLink,
                )
                .map_err(|refusal| refusal.to_string())
            },
        );
        match brought {
            Ok(()) => tracing::info!(
                "the stream `{}` stopped and took the edits its graph document holds",
                self.stream_id()
            ),
            Err(why) => {
                let mut held = self.edits_its_document_holds_not_live.lock();
                let held_meanwhile = std::mem::replace(&mut *held, edits_taken_back);
                held.extend(held_meanwhile);
                tracing::warn!(
                    "the stream `{}` stopped and did not take the edits its graph document \
                     holds, which it keeps as not live: {why}",
                    self.stream_id()
                );
            }
        }
    }

    /// Forget every edit held not live: the running graph holds the stream's document again.
    pub(in crate::core::runtime) fn forget_the_edits_its_document_holds_not_live(&self) {
        self.edits_its_document_holds_not_live.lock().clear();
    }

    /// Answer, in `graph_response`, which renders the running graph, the graph this stream's
    /// document holds while it holds edits the running graph did not take, as
    /// [`render_the_document_over_the_live_graph`] draws it. A document that does not read leaves
    /// the running graph's answer, logged.
    pub(super) fn render_the_document_over_the_live_graph_while_edits_are_not_live(
        &self,
        graph_response: &mut GraphResponse,
    ) {
        if self.edits_its_document_holds_not_live.lock().is_empty() {
            return;
        }
        match the_graph_the_document_of_the_stream_at_holds(self.stream_location()) {
            Ok(document) => render_the_document_over_the_live_graph(
                graph_response,
                &document,
                &self.node_types_this_stream_resolves,
            ),
            Err(why) => tracing::warn!(
                "the stream `{}` holds edits not live, and its graph document does not read, \
                 so `graph` answers its running graph: {why}",
                self.stream_id()
            ),
        }
    }

    /// Where `operation` applies: the link it connects or disconnects, the node it adds or the
    /// fragment node it folds or unfolds under the name the operation gives it, any other node it
    /// acts on as the running graph names it, or else the stream.
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
            StreamGraphOperation::FoldIntoFragment { fold }
            | StreamGraphOperation::UnfoldFragment { fold } => match fold.fragment_node() {
                Some(fragment_node) => DiagnosticPlaceOnTheWire::Node {
                    node_id: fragment_node.to_string(),
                    node_name: fold.fragment_name().to_string(),
                },
                None => DiagnosticPlaceOnTheWire::Stream,
            },
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

/// The two ends of a link by the ids of the nodes it joins and its ports: how a link the
/// running graph holds is matched to one its document draws.
#[derive(PartialEq, Eq, Hash)]
struct LinkEnds {
    from_node: String,
    from_port: String,
    to_node: String,
    to_port: String,
}

impl LinkEnds {
    /// The ends of the link from `source` to `target`, each node's name read as its id through
    /// `id_of_each_name`; `None` when either node is not there.
    fn of(
        source: &LinkPortRefOutput,
        target: &LinkPortRefOutput,
        id_of_each_name: &HashMap<String, String>,
    ) -> Option<Self> {
        Some(Self {
            from_node: id_of_each_name.get(&source.node)?.clone(),
            from_port: source.port.clone(),
            to_node: id_of_each_name.get(&target.node)?.clone(),
            to_port: target.port.clone(),
        })
    }
}

/// The id of each node `nodes` renders, by its name.
fn the_id_of_each_name(nodes: &[GraphNodeOutput]) -> HashMap<String, String> {
    nodes
        .iter()
        .map(|node| (node.name.clone(), node.id.clone()))
        .collect()
}

/// Answer `document` in `graph_response`, which renders the running graph: each node of the
/// document's own level the running graph holds by its UUID as it renders it, under the
/// document's name, pins and studio fields, and each other as it is stored, with the ports
/// `node_types` declares for its type, carrying `live: false`; each link the running graph holds
/// between the same ports of the same nodes as it renders it, and each other as the document
/// draws it, pending and carrying `live: false`. A node the running graph holds and the document
/// does not is left out.
fn render_the_document_over_the_live_graph(
    graph_response: &mut GraphResponse,
    document: &GraphSnapshot,
    node_types: &NodeTypesOneStreamResolves,
) {
    let live_id_of_each_name = the_id_of_each_name(&graph_response.nodes);
    let mut live_node_of_each_id: HashMap<String, GraphNodeOutput> =
        std::mem::take(&mut graph_response.nodes)
            .into_iter()
            .map(|node| (node.id.clone(), node))
            .collect();
    graph_response.nodes = document
        .nodes
        .iter()
        .map(|node| {
            let live = node
                .id
                .and_then(|node_uuid| live_node_of_each_id.remove(&node_uuid.to_string()));
            match live {
                Some(mut live) => {
                    redraw_a_node_over_its_pins(&mut live, &node.input_values);
                    live.studio = node.studio.clone();
                    live.name = cast_or_as_written(&node.name);
                    live
                }
                None => {
                    let mut drawn = rendered_as_stored(node, node_types);
                    if let Some(ports) = node_types.port_info(&node.node_type) {
                        drawn.ports = GraphNodePortsOnTheWire::from(&ports);
                    }
                    drawn.live = Some(false);
                    drawn
                }
            }
        })
        .collect();
    let document_id_of_each_name = the_id_of_each_name(&graph_response.nodes);
    let mut live_link_of_each_ends: HashMap<LinkEnds, LinkOutput> =
        std::mem::take(&mut graph_response.links)
            .into_iter()
            .filter_map(|link| {
                LinkEnds::of(&link.source, &link.target, &live_id_of_each_name)
                    .map(|ends| (ends, link))
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
            let live = LinkEnds::of(&source, &target, &document_id_of_each_name)
                .and_then(|ends| live_link_of_each_ends.remove(&ends));
            match live {
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
