// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl ProjectRepository {
    /// Refuse deleting `asset` while a graph document other than its own references it, as
    /// the transaction has staged that document or as it is on disk.
    pub(super) fn refuse_deleting_an_asset_a_graph_references(
        &self,
        asset: &ProjectAsset,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        staged_documents: &BTreeMap<Uuid, StreamGraphDocument>,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let asset_text = asset.asset.to_string();
        let mut referencing_documents = Vec::new();
        for (stream, staged_document) in staged_documents {
            if *stream == asset.asset {
                continue;
            }
            let document: Value = serde_json::from_slice(&staged_document.canonical_bytes()?)?;
            if value_references_the_asset(&document, &asset_text) {
                referencing_documents.push(
                    staged_assets
                        .get(stream)
                        .map_or_else(|| stream.to_string(), |held| held.path.to_string()),
                );
            }
        }
        let staged_paths: BTreeSet<String> = staged_documents
            .keys()
            .filter_map(|stream| staged_assets.get(stream))
            .map(|held| held.path.to_string())
            .collect();
        for referencing in self.graph_documents_referencing(asset.asset, &asset.path)? {
            if !staged_paths.contains(&referencing) {
                referencing_documents.push(referencing);
            }
        }
        if referencing_documents.is_empty() {
            Ok(())
        } else {
            Err(ProjectRepositoryRefusal::AssetStillReferenced {
                asset: asset.asset,
                referencing_documents,
            })
        }
    }

    pub(super) fn perform_effect(
        &self,
        operation: &ProjectAssetOperation,
    ) -> Result<PerformedAssetEffect, ProjectRepositoryRefusal> {
        match operation {
            ProjectAssetOperation::ImportAsset { asset } => {
                let destination = asset.path.on_disk_under(&self.project_directory);
                let moved_from = match self.asset_files_waiting_for_their_import.get(&asset.asset) {
                    Some(waiting) if !destination.exists() => {
                        if let Some(parent) = destination.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::rename(&waiting.file, &destination)?;
                        Some(waiting.file.clone())
                    }
                    _ => None,
                };
                if let Err(refusal) = self.write_sidecar(asset) {
                    if let Some(moved_from) = &moved_from {
                        let _ = std::fs::rename(&destination, moved_from);
                    }
                    return Err(refusal);
                }
                Ok(match moved_from {
                    Some(moved_from) => PerformedAssetEffect::ImportThatPlacedItsFile {
                        asset: asset.clone(),
                        moved_from,
                    },
                    None => PerformedAssetEffect::Operation(operation.clone()),
                })
            }
            ProjectAssetOperation::DeleteAsset { asset } => {
                self.remove_the_sidecar_of(asset)?;
                let file = asset.path.on_disk_under(&self.project_directory);
                if !file.exists() {
                    return Ok(PerformedAssetEffect::Operation(operation.clone()));
                }
                let set_aside = self
                    .project_directory
                    .join(PROJECT_UPLOADS_DIRECTORY)
                    .join(format!(".deleted-{}", Uuid::new_v4()));
                let set = set_aside
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| std::fs::rename(&file, &set_aside));
                if let Err(not_set_aside) = set {
                    self.restore_the_sidecar_of(asset);
                    return Err(not_set_aside.into());
                }
                Ok(PerformedAssetEffect::DeleteThatSetItsFileAside {
                    asset: asset.clone(),
                    set_aside,
                })
            }
            ProjectAssetOperation::RelocateAsset { from, to, .. } => {
                let from_file = from.on_disk_under(&self.project_directory);
                let to_file = to.on_disk_under(&self.project_directory);
                if let Some(parent) = to_file.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if from_file.exists() {
                    std::fs::rename(&from_file, &to_file)?;
                }
                if is_a_stream_document(from) {
                    return Ok(PerformedAssetEffect::Operation(operation.clone()));
                }
                if let Err(error) =
                    std::fs::rename(self.sidecar_on_disk(from), self.sidecar_on_disk(to))
                {
                    if to_file.exists() {
                        let _ = std::fs::rename(&to_file, &from_file);
                    }
                    return Err(error.into());
                }
                Ok(PerformedAssetEffect::Operation(operation.clone()))
            }
            ProjectAssetOperation::SetAssetSettings { path, to, .. } => {
                let sidecar_on_disk = self.sidecar_on_disk(path);
                let mut sidecar: ProjectAssetSidecar =
                    serde_json::from_slice(&std::fs::read(&sidecar_on_disk)?)?;
                sidecar.import_settings = to.clone();
                write_file_whole(&sidecar_on_disk, &canonical_project_json_bytes(&sidecar)?)?;
                Ok(PerformedAssetEffect::Operation(operation.clone()))
            }
            ProjectAssetOperation::ReplaceAssetContent {
                asset,
                path,
                from,
                to,
            } => {
                self.swap_in_the_bytes_of_a_replacement(*asset, path, from, to)?;
                let sidecar_on_disk = self.sidecar_on_disk(path);
                let mut sidecar: ProjectAssetSidecar =
                    serde_json::from_slice(&std::fs::read(&sidecar_on_disk)?)?;
                sidecar.content_sha256 = to.content_sha256.clone();
                sidecar.content_byte_len = to.content_byte_len;
                write_file_whole(&sidecar_on_disk, &canonical_project_json_bytes(&sidecar)?)?;
                Ok(PerformedAssetEffect::Operation(operation.clone()))
            }
        }
    }

    /// Put the bytes `to` names at the asset's `path` — the file its replacement waits with, or
    /// the upload of those bytes — keeping the bytes there now as the upload of `from`, so an
    /// undo swaps them back. Bytes neither waits nor is uploaded leave the file as it is.
    pub(super) fn swap_in_the_bytes_of_a_replacement(
        &self,
        asset: Uuid,
        path: &ProjectRelativeAssetPath,
        from: &ProjectAssetContent,
        to: &ProjectAssetContent,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let replacement = self
            .asset_files_waiting_for_their_import
            .get(&asset)
            .map(|waiting| waiting.file.clone())
            .filter(|waiting_file| waiting_file.is_file())
            .unwrap_or_else(|| the_upload_file_of(&self.project_directory, &to.content_sha256));
        if !replacement.is_file() {
            return Ok(());
        }
        let destination = path.on_disk_under(&self.project_directory);
        if destination.is_file() {
            let kept_as_the_upload_of_the_earlier_bytes =
                the_upload_file_of(&self.project_directory, &from.content_sha256);
            if kept_as_the_upload_of_the_earlier_bytes.exists() {
                std::fs::remove_file(&destination)?;
            } else {
                std::fs::rename(&destination, &kept_as_the_upload_of_the_earlier_bytes)?;
            }
        }
        std::fs::rename(&replacement, &destination)?;
        Ok(())
    }

    pub(super) fn undo_performed_effects(&self, performed_effects: Vec<PerformedAssetEffect>) {
        for performed in performed_effects.into_iter().rev() {
            let undone = match performed {
                PerformedAssetEffect::ImportThatPlacedItsFile { asset, moved_from } => {
                    self.remove_the_sidecar_of(&asset).and_then(|()| {
                        Ok(std::fs::rename(
                            asset.path.on_disk_under(&self.project_directory),
                            moved_from,
                        )?)
                    })
                }
                PerformedAssetEffect::Operation(ProjectAssetOperation::ImportAsset { asset }) => {
                    self.remove_the_sidecar_of(&asset)
                }
                PerformedAssetEffect::Operation(ProjectAssetOperation::DeleteAsset { asset }) => {
                    self.write_sidecar(&asset)
                }
                PerformedAssetEffect::DeleteThatSetItsFileAside { asset, set_aside } => {
                    self.restore_the_sidecar_of(&asset);
                    std::fs::rename(set_aside, asset.path.on_disk_under(&self.project_directory))
                        .map_err(ProjectRepositoryRefusal::from)
                }
                PerformedAssetEffect::Operation(operation) => {
                    self.perform_effect(&operation.inverse()).map(|_| ())
                }
            };
            if let Err(refusal) = undone {
                tracing::error!(
                    project = %self.project_directory.display(),
                    %refusal,
                    "could not undo part of a refused project transaction"
                );
            }
        }
    }

    pub(super) fn restore_the_sidecar_of(&self, asset: &ProjectAsset) {
        if let Err(refusal) = self.write_sidecar(asset) {
            tracing::error!(
                project = %self.project_directory.display(),
                asset = %asset.asset,
                %refusal,
                "could not restore an asset's sidecar"
            );
        }
    }

    /// Refuse a path whose folders pass through a symbolic link, which could lead a change
    /// outside the project.
    pub(super) fn refuse_a_path_through_a_symbolic_link(
        &self,
        path: &ProjectRelativeAssetPath,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let mut folder = self.project_directory.clone();
        for segment in path
            .directory()
            .split('/')
            .filter(|segment| !segment.is_empty())
        {
            folder.push(segment);
            if std::fs::symlink_metadata(&folder).is_ok_and(|metadata| metadata.is_symlink()) {
                return Err(ProjectRepositoryRefusal::AssetPathRefused {
                    path: path.to_string(),
                    why: "a folder on its way is a symbolic link, which could lead outside the project",
                });
            }
        }
        Ok(())
    }

    /// Write `asset`'s sidecar; a stream asset has none, its UUID being inside its document.
    pub(super) fn write_sidecar(
        &self,
        asset: &ProjectAsset,
    ) -> Result<(), ProjectRepositoryRefusal> {
        if asset.kind == ProjectAssetKind::Stream {
            return Ok(());
        }
        write_file_whole(&self.sidecar_on_disk(&asset.path), &asset.sidecar_bytes()?)
    }

    pub(super) fn remove_the_sidecar_of(
        &self,
        asset: &ProjectAsset,
    ) -> Result<(), ProjectRepositoryRefusal> {
        if asset.kind == ProjectAssetKind::Stream {
            return Ok(());
        }
        Ok(std::fs::remove_file(self.sidecar_on_disk(&asset.path))?)
    }

    pub(super) fn sidecar_on_disk(&self, path: &ProjectRelativeAssetPath) -> PathBuf {
        self.project_directory.join(path.sidecar_path())
    }

    /// The whole project as its history records it with `assets`: every file the project's
    /// scan records, a media, scene or data asset as its sidecar alone, never its bytes.
    pub(super) fn tracked_files(
        &self,
        assets: &BTreeMap<Uuid, ProjectAsset>,
    ) -> Result<BTreeMap<String, Vec<u8>>, ProjectRepositoryRefusal> {
        let asset_paths_recorded_by_their_sidecar: BTreeSet<&str> = assets
            .values()
            .filter(|asset| asset.kind != ProjectAssetKind::Stream)
            .map(|asset| asset.path.as_str())
            .collect();
        let mut tracked_files = BTreeMap::new();
        for project_file in project_files_the_history_records(&self.project_directory)? {
            if asset_paths_recorded_by_their_sidecar.contains(project_file.as_str()) {
                continue;
            }
            let bytes = std::fs::read(self.project_directory.join(&project_file))?;
            tracked_files.insert(project_file, bytes);
        }
        tracked_files.insert(
            PROJECT_FILE_NAME.to_owned(),
            self.project_file.canonical_bytes()?,
        );
        for asset in assets.values() {
            if asset.kind != ProjectAssetKind::Stream {
                tracked_files.insert(asset.path.sidecar_path(), asset.sidecar_bytes()?);
            }
        }
        Ok(tracked_files)
    }

    /// The project's `*.graph.json` files, leaving out any path an asset holds.
    pub(super) fn graph_documents_beside(
        &self,
        assets: &BTreeMap<Uuid, ProjectAsset>,
    ) -> Result<Vec<String>, ProjectRepositoryRefusal> {
        let mut graph_documents =
            project_files_ending_with(&self.project_directory, GRAPH_DOCUMENT_FILE_SUFFIX)?;
        graph_documents.retain(|graph_document| {
            !assets
                .values()
                .any(|asset| asset.path.as_str() == graph_document)
        });
        Ok(graph_documents)
    }

    /// Refuse an in-place import whose file the filesystem already resolves to another asset's.
    pub(super) fn refuse_a_file_another_asset_already_is(
        &self,
        assets: &BTreeMap<Uuid, ProjectAsset>,
        path: &ProjectRelativeAssetPath,
    ) -> Result<(), ProjectRepositoryRefusal> {
        let destination = path.on_disk_under(&self.project_directory);
        match assets.values().find(|asset| {
            same_file(
                &asset.path.on_disk_under(&self.project_directory),
                &destination,
            )
        }) {
            Some(holder) => Err(ProjectRepositoryRefusal::AssetPathHeldByAnotherAsset {
                path: path.to_string(),
                holder: holder.asset,
            }),
            None => Ok(()),
        }
    }

    /// Every graph document but the asset's own at `asset_path` that references `asset`.
    pub(super) fn graph_documents_referencing(
        &self,
        asset: Uuid,
        asset_path: &ProjectRelativeAssetPath,
    ) -> Result<Vec<String>, ProjectRepositoryRefusal> {
        let asset_text = asset.to_string();
        let mut referencing = Vec::new();
        for graph_document in self.graph_documents_beside(&self.assets_by_uuid)? {
            if graph_document == asset_path.as_str() {
                continue;
            }
            let bytes = std::fs::read(self.project_directory.join(&graph_document))?;
            let document = serde_json::from_slice::<Value>(&bytes).map_err(|not_parsed| {
                ProjectRepositoryRefusal::GraphDocumentUnreadable {
                    path: graph_document.clone(),
                    why: not_parsed.to_string(),
                }
            })?;
            if value_references_the_asset(&document, &asset_text) {
                referencing.push(graph_document);
            }
        }
        Ok(referencing)
    }
}
