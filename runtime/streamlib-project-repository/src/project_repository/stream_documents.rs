// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;
use crate::stream_fragment_references::the_stream_document_of_an_import;
use crate::stream_graph_placed_stream_ports::{
    PortsAPlacedStreamDeclares, each_link_onto_a_placed_stream_named_by_its_interface_node,
};

impl ProjectRepository {
    /// Write each staged graph document at the path its stream asset holds once the
    /// transaction applies, each link onto a port of a placed stream naming the port by the
    /// UUID of the interface node declaring it, answering what each held before so a refusal
    /// can restore it.
    pub(super) fn write_staged_documents(
        &self,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        staged_documents: &BTreeMap<Uuid, StreamGraphDocument>,
    ) -> Result<Vec<ProjectFileAsItWasBeforeAWrite>, ProjectRepositoryRefusal> {
        let mut written_documents = Vec::with_capacity(staged_documents.len());
        for (stream, staged_document) in staged_documents {
            let Some(stream_asset) = staged_assets.get(stream) else {
                continue;
            };
            let on_disk = stream_asset.path.on_disk_under(&self.project_directory);
            let bytes_before = std::fs::read(&on_disk).ok();
            let mut document_on_disk = staged_document.clone();
            let (nodes, links) = document_on_disk.node_and_link_values_mut();
            each_link_onto_a_placed_stream_named_by_its_interface_node(
                nodes,
                links,
                &mut |held_stream| {
                    self.the_ports_a_placed_stream_declares(
                        staged_assets,
                        Some(staged_documents),
                        held_stream,
                    )
                },
            );
            let written = document_on_disk
                .canonical_bytes()
                .map_err(ProjectRepositoryRefusal::from)
                .and_then(|bytes| write_file_whole(&on_disk, &bytes));
            if let Err(refusal) = written {
                self.restore_written_documents(written_documents);
                return Err(refusal);
            }
            written_documents.push((on_disk, bytes_before));
        }
        Ok(written_documents)
    }

    /// The ports the stream `held_stream`, the input value `stream` of a node placing one, places
    /// declare: a stream of the project as `staged_documents` holds it, or as it is on disk, or
    /// a stream of an import as the import's folder holds it; `None` when it does not read.
    pub(super) fn the_ports_a_placed_stream_declares(
        &self,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        staged_documents: Option<&BTreeMap<Uuid, StreamGraphDocument>>,
        held_stream: &Value,
    ) -> Option<PortsAPlacedStreamDeclares> {
        let stream = Uuid::parse_str(held_stream.get("asset")?.as_str()?).ok()?;
        if let Some(from) = held_stream.get("from") {
            let from = Uuid::parse_str(from.as_str()?).ok()?;
            let (document, _) =
                the_stream_document_of_an_import(&self.project_directory, from, stream).ok()?;
            let nodes = document.get("nodes")?.as_array()?;
            return Some(PortsAPlacedStreamDeclares::of(
                nodes.iter().filter_map(Value::as_object),
            ));
        }
        if let Some(staged) = staged_documents.and_then(|staged| staged.get(&stream)) {
            return Some(PortsAPlacedStreamDeclares::of(staged.node_objects()));
        }
        let stream_asset = staged_assets
            .get(&stream)
            .filter(|asset| asset.kind == ProjectAssetKind::Stream)?;
        let bytes = std::fs::read(stream_asset.path.on_disk_under(&self.project_directory)).ok()?;
        let document = StreamGraphDocument::from_bytes(&bytes).ok()?;
        Some(PortsAPlacedStreamDeclares::of(document.node_objects()))
    }

    /// Remove each graph document at `paths`, answering what each held so a refusal can
    /// restore it.
    pub(super) fn take_out_stream_documents(
        &self,
        paths: &BTreeSet<ProjectRelativeAssetPath>,
    ) -> Result<Vec<ProjectFileAsItWasBeforeAWrite>, ProjectRepositoryRefusal> {
        let mut taken_out = Vec::with_capacity(paths.len());
        for path in paths {
            let on_disk = path.on_disk_under(&self.project_directory);
            let removed = std::fs::read(&on_disk)
                .and_then(|bytes_before| std::fs::remove_file(&on_disk).map(|()| bytes_before));
            match removed {
                Ok(bytes_before) => taken_out.push((on_disk, Some(bytes_before))),
                Err(not_removed) => {
                    self.restore_written_documents(taken_out);
                    return Err(not_removed.into());
                }
            }
        }
        Ok(taken_out)
    }

    /// Stage `operation` on the stream assets and their graph documents.
    pub(super) fn apply_operation_to_stream_documents(
        &self,
        staged_assets: &mut BTreeMap<Uuid, ProjectAsset>,
        staged_documents: &mut BTreeMap<Uuid, StreamGraphDocument>,
        stream_documents_taken_out: &mut BTreeSet<ProjectRelativeAssetPath>,
        operation: &ProjectStreamDocumentOperation,
    ) -> Result<(), ProjectRepositoryRefusal> {
        match operation {
            ProjectStreamDocumentOperation::CreateStreamDocument {
                stream,
                path,
                document,
            } => {
                refuse_a_stream_document_path_without_its_suffix(path)?;
                self.refuse_a_path_through_a_symbolic_link(path)?;
                if let Some(holder) = staged_assets.values().find(|asset| asset.path == *path) {
                    return Err(ProjectRepositoryRefusal::AssetPathHeldByAnotherAsset {
                        path: path.to_string(),
                        holder: holder.asset,
                    });
                }
                if !stream_documents_taken_out.contains(path)
                    && path.on_disk_under(&self.project_directory).exists()
                {
                    return Err(ProjectRepositoryRefusal::AssetPathHoldsAnotherFile {
                        path: path.to_string(),
                    });
                }
                if let Some(earlier) = staged_assets.get(stream) {
                    return Err(ProjectRepositoryRefusal::AssetNamedTwice {
                        asset: *stream,
                        first: earlier.path.to_string(),
                        second: path.to_string(),
                    });
                }
                let unreadable = |why: String| ProjectRepositoryRefusal::GraphDocumentUnreadable {
                    path: path.to_string(),
                    why,
                };
                if document
                    .get(STREAM_DOCUMENT_ASSET_KEY)
                    .and_then(Value::as_str)
                    != Some(stream.to_string().as_str())
                {
                    return Err(unreadable(format!(
                        "its `{STREAM_DOCUMENT_ASSET_KEY}` does not name its stream asset {stream}"
                    )));
                }
                let staged_document =
                    StreamGraphDocument::from_value(document.clone()).map_err(unreadable)?;
                let bytes = staged_document.canonical_bytes()?;
                for (node, input_values) in
                    staged_document.input_values_of_every_node_at_every_level()
                {
                    refuse_input_values_referencing_an_asset_the_project_does_not_hold(
                        staged_assets,
                        node,
                        input_values,
                    )?;
                }
                staged_assets.insert(
                    *stream,
                    ProjectAsset {
                        asset: *stream,
                        kind: ProjectAssetKind::Stream,
                        content_sha256: content_sha256_of(&bytes),
                        content_byte_len: bytes.len() as u64,
                        import_settings: serde_json::Map::new(),
                        path: path.clone(),
                    },
                );
                staged_documents.insert(*stream, staged_document);
                stream_documents_taken_out.remove(path);
            }
            ProjectStreamDocumentOperation::DeleteStreamDocument {
                stream,
                path,
                document,
            } => {
                let stream_asset = staged_assets
                    .get(stream)
                    .filter(|asset| asset.kind == ProjectAssetKind::Stream && asset.path == *path)
                    .cloned()
                    .ok_or(ProjectRepositoryRefusal::NoSuchStream(*stream))?;
                let document_as_it_is = match staged_documents.get(stream) {
                    Some(staged_document) => staged_document.clone(),
                    None => self.read_stream_graph_document(staged_assets, *stream)?.1,
                };
                let document_as_it_was_written = StreamGraphDocument::from_value(document.clone())
                    .map_err(|why| ProjectRepositoryRefusal::GraphDocumentUnreadable {
                        path: path.to_string(),
                        why,
                    })?;
                if document_as_it_is.canonical_bytes()?
                    != document_as_it_was_written.canonical_bytes()?
                {
                    return Err(
                        ProjectRepositoryRefusal::StreamDocumentEditedSinceItWasWritten {
                            path: path.to_string(),
                        },
                    );
                }
                self.refuse_deleting_an_asset_a_graph_references(
                    &stream_asset,
                    staged_assets,
                    staged_documents,
                )?;
                staged_assets.remove(stream);
                staged_documents.remove(stream);
                stream_documents_taken_out.insert(path.clone());
            }
            ProjectStreamDocumentOperation::SetStreamDisplayName {
                stream,
                path,
                from,
                to,
            } => {
                if !staged_assets.get(stream).is_some_and(|asset| {
                    asset.kind == ProjectAssetKind::Stream && asset.path == *path
                }) {
                    return Err(ProjectRepositoryRefusal::NoSuchStream(*stream));
                }
                let staged_document = match staged_documents.entry(*stream) {
                    Entry::Occupied(staged) => staged.into_mut(),
                    Entry::Vacant(unstaged) => {
                        unstaged.insert(self.read_stream_graph_document(staged_assets, *stream)?.1)
                    }
                };
                if staged_document.display_name() != from.as_deref() {
                    return Err(
                        ProjectRepositoryRefusal::StreamDocumentEditedSinceItWasWritten {
                            path: path.to_string(),
                        },
                    );
                }
                staged_document.set_display_name(to.as_deref());
            }
        }
        Ok(())
    }

    pub(super) fn restore_written_documents(
        &self,
        written_documents: Vec<ProjectFileAsItWasBeforeAWrite>,
    ) {
        for (on_disk, bytes_before) in written_documents.into_iter().rev() {
            let restored = match bytes_before {
                Some(bytes) => write_file_whole(&on_disk, &bytes),
                None => std::fs::remove_file(&on_disk).map_err(ProjectRepositoryRefusal::from),
            };
            if let Err(refusal) = restored {
                tracing::error!(
                    project = %self.project_directory.display(),
                    document = %on_disk.display(),
                    %refusal,
                    "could not restore a graph document a refused transaction wrote"
                );
            }
        }
    }
}
