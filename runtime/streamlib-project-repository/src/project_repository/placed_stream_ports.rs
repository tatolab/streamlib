// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A link onto a port of a node placing a stream, as the project's files and history hold it:
//! by the UUID of the placed stream's interface node declaring the port. Each graph document,
//! and each operation of a recorded transaction, is read naming the port as the placed stream
//! declares it now, and written naming it by that UUID.

use super::*;
use crate::stream_fragment_references::the_stream_document_of_an_import_as_its_file_holds_it;
use crate::stream_graph_document::StreamGraphOperation;
use crate::stream_graph_placed_stream_ports::{
    PortsAPlacedStreamDeclares, each_link_onto_a_placed_stream_named_by_its_interface_node,
    each_link_onto_a_placed_stream_named_by_its_port, each_placing_node_in,
};

impl ProjectRepository {
    /// The ports the stream `held_stream`, the input value `stream` of a node placing one, places
    /// declare: a stream of the project as `staged_documents` holds it, or as it is on disk, or
    /// a stream of an import as the import's folder holds it; `None` when it does not read.
    fn the_ports_a_placed_stream_declares(
        &self,
        assets: &BTreeMap<Uuid, ProjectAsset>,
        staged_documents: Option<&BTreeMap<Uuid, StreamGraphDocument>>,
        held_stream: &Value,
    ) -> Option<PortsAPlacedStreamDeclares> {
        let stream = Uuid::parse_str(held_stream.get("asset")?.as_str()?).ok()?;
        if let Some(from) = held_stream.get("from") {
            let from = Uuid::parse_str(from.as_str()?).ok()?;
            let (document, _) = the_stream_document_of_an_import_as_its_file_holds_it(
                &self.project_directory,
                from,
                stream,
            )
            .ok()?;
            let nodes = document.get("nodes")?.as_array()?;
            return Some(PortsAPlacedStreamDeclares::of(
                nodes.iter().filter_map(Value::as_object),
            ));
        }
        if let Some(staged) = staged_documents.and_then(|staged| staged.get(&stream)) {
            return Some(PortsAPlacedStreamDeclares::of(staged.node_objects()));
        }
        Some(PortsAPlacedStreamDeclares::of(
            self.the_document_on_disk(assets, stream)?.node_objects(),
        ))
    }

    /// The graph document of the stream asset `stream` among `assets` as its file holds it now.
    fn the_document_on_disk(
        &self,
        assets: &BTreeMap<Uuid, ProjectAsset>,
        stream: Uuid,
    ) -> Option<StreamGraphDocument> {
        let stream_asset = assets
            .get(&stream)
            .filter(|asset| asset.kind == ProjectAssetKind::Stream)?;
        let bytes = std::fs::read(stream_asset.path.on_disk_under(&self.project_directory)).ok()?;
        StreamGraphDocument::from_bytes(&bytes).ok()
    }

    /// `document`, read from its file, with each link onto a placed stream naming its port as
    /// the placed stream, among `assets` or an import, declares it now.
    pub(super) fn each_placed_stream_port_named_by_its_name(
        &self,
        assets: &BTreeMap<Uuid, ProjectAsset>,
        document: &mut StreamGraphDocument,
    ) {
        document.edit_as_one_value(|value| {
            each_link_onto_a_placed_stream_named_by_its_port(value, &[], &mut |held_stream| {
                self.the_ports_a_placed_stream_declares(assets, None, held_stream)
            })
        });
    }

    /// The ports `held_stream` places declare as of `named_as_of` of a transaction staging
    /// `staged_documents`, followed by those it declares as of the transaction's other end, so a
    /// name a link holds from that other end keeps the interface node it named.
    fn the_ports_a_placed_stream_declares_once_staged(
        &self,
        assets_before: &BTreeMap<Uuid, ProjectAsset>,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        staged_documents: &BTreeMap<Uuid, StreamGraphDocument>,
        held_stream: &Value,
        named_as_of: PortsNamedAsOf,
    ) -> Option<PortsAPlacedStreamDeclares> {
        let staged = self.the_ports_a_placed_stream_declares(
            staged_assets,
            Some(staged_documents),
            held_stream,
        );
        let before = self.the_ports_a_placed_stream_declares(assets_before, None, held_stream);
        let (first, then) = match named_as_of {
            PortsNamedAsOf::TheTransactionsEnd => (staged, before),
            PortsNamedAsOf::TheTransactionsStart => (before, staged),
        };
        match (first, then) {
            (Some(first), Some(then)) => Some(first.followed_by(then)),
            (first, then) => first.or(then),
        }
    }

    /// Each of `staged_documents` as its file is to hold it, each link onto a placed stream
    /// naming its port by the UUID of the interface node declaring it. Read before any file of
    /// the transaction is written.
    pub(super) fn the_staged_documents_as_written(
        &self,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        staged_documents: &BTreeMap<Uuid, StreamGraphDocument>,
    ) -> Result<BTreeMap<Uuid, StreamGraphDocument>, ProjectRepositoryRefusal> {
        let assets_before = self.every_asset()?;
        Ok(staged_documents
            .iter()
            .map(|(stream, staged_document)| {
                let mut as_written = staged_document.clone();
                as_written.edit_as_one_value(|value| {
                    each_link_onto_a_placed_stream_named_by_its_interface_node(
                        value,
                        &[],
                        &mut |held_stream| {
                            self.the_ports_a_placed_stream_declares_once_staged(
                                &assets_before,
                                staged_assets,
                                staged_documents,
                                held_stream,
                                PortsNamedAsOf::TheTransactionsEnd,
                            )
                        },
                    )
                });
                (*stream, as_written)
            })
            .collect())
    }

    /// `transaction` as history records it: each link onto a placed stream an operation names,
    /// in a graph edit or a whole graph document, naming its port by the UUID of the interface
    /// node declaring it once `staged_documents` apply. Read before any file of the transaction
    /// is written.
    pub(super) fn the_transaction_as_recorded(
        &self,
        transaction: &ProjectTransaction,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        staged_documents: &BTreeMap<Uuid, StreamGraphDocument>,
    ) -> Result<ProjectTransaction, ProjectRepositoryRefusal> {
        let assets_before = self.every_asset()?;
        let placing_nodes_of = |stream: Uuid| {
            let mut placing_nodes = Vec::new();
            if let Some(document) = staged_documents
                .get(&stream)
                .cloned()
                .or_else(|| self.the_document_on_disk(&assets_before, stream))
            {
                each_placing_node_in(&document.to_value(), &mut placing_nodes);
            }
            placing_nodes
        };
        each_operation_of_the_transaction_renamed(transaction, &mut |value, stream, named_as_of| {
            each_link_onto_a_placed_stream_named_by_its_interface_node(
                value,
                &placing_nodes_of(stream),
                &mut |held_stream| {
                    self.the_ports_a_placed_stream_declares_once_staged(
                        &assets_before,
                        staged_assets,
                        staged_documents,
                        held_stream,
                        named_as_of,
                    )
                },
            )
        })
    }

    /// `applied`, as history records it, with each link onto a placed stream an operation names
    /// naming its port as the placed stream declares it now.
    pub(super) fn the_transaction_as_read_now(
        &self,
        mut applied: AppliedProjectTransaction,
        assets: &BTreeMap<Uuid, ProjectAsset>,
    ) -> Result<AppliedProjectTransaction, ProjectRepositoryRefusal> {
        let placing_nodes_of = |stream: Uuid| {
            let mut placing_nodes = Vec::new();
            if let Some(document) = self.the_document_on_disk(assets, stream) {
                each_placing_node_in(&document.to_value(), &mut placing_nodes);
            }
            placing_nodes
        };
        applied.transaction = each_operation_of_the_transaction_renamed(
            &applied.transaction,
            &mut |value, stream, _| {
                each_link_onto_a_placed_stream_named_by_its_port(
                    value,
                    &placing_nodes_of(stream),
                    &mut |held_stream| {
                        self.the_ports_a_placed_stream_declares(assets, None, held_stream)
                    },
                )
            },
        )?;
        Ok(applied)
    }
}

/// Which end of a transaction an operation's links name each placed stream's ports as of.
#[derive(Clone, Copy)]
enum PortsNamedAsOf {
    TheTransactionsStart,
    TheTransactionsEnd,
}

/// `transaction` with `rename` made on each of its operations and inverse operations that
/// edits a stream's graph, or writes or takes out its document whole, as one JSON value, with
/// the stream it is on and the end of the transaction its links name ports as of.
///
/// A carry of the links onto a placed stream's ports is renamed in the links it takes out
/// alone: the links it puts back name each port as the edit inside the placed stream beside it
/// in the transaction leaves it, which its undo or redo leaves it as again, while the links it
/// takes out name the port as it is when the carry applies, which another edit may change. A
/// carry's own links name ports as of the transaction's start, and its inverse's as of its end.
fn each_operation_of_the_transaction_renamed(
    transaction: &ProjectTransaction,
    rename: &mut dyn FnMut(&mut Value, Uuid, PortsNamedAsOf),
) -> Result<ProjectTransaction, ProjectRepositoryRefusal> {
    let mut renamed = transaction.clone();
    let each_operation = renamed
        .operations
        .iter_mut()
        .map(|operation| (operation, PortsNamedAsOf::TheTransactionsStart))
        .chain(
            renamed
                .inverse_operations
                .iter_mut()
                .map(|operation| (operation, PortsNamedAsOf::TheTransactionsEnd)),
        );
    for (operation, a_carry_names_ports_as_of) in each_operation {
        let stream = match operation {
            ProjectOperation::StreamGraph(StreamGraphOperationOnAStream {
                stream,
                graph_operation:
                    StreamGraphOperation::CarryTheLinksOntoThePortsOfAPlacedStream {
                        port_links_taken_out,
                        ..
                    },
            }) => {
                for taken_out in port_links_taken_out {
                    rename(&mut taken_out.value, *stream, a_carry_names_ports_as_of);
                }
                continue;
            }
            ProjectOperation::StreamGraph(on_a_stream) => on_a_stream.stream,
            ProjectOperation::StreamDocument(
                ProjectStreamDocumentOperation::CreateStreamDocument { stream, .. }
                | ProjectStreamDocumentOperation::DeleteStreamDocument { stream, .. },
            ) => *stream,
            _ => continue,
        };
        let mut value = serde_json::to_value(&*operation)?;
        rename(&mut value, stream, PortsNamedAsOf::TheTransactionsEnd);
        *operation = serde_json::from_value(value)?;
    }
    Ok(renamed)
}
