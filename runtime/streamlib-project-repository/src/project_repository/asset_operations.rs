// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

impl ProjectRepository {
    /// Where an import that names no path lands: `file_name`, or the source file's own name, in
    /// its kind's standard folder, numbered `<stem>-2.<extension>`, `-3`, … past a name an asset
    /// or another file already takes there.
    pub(super) fn the_free_path_in_the_standard_folder_of_an_import(
        &self,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        source: &ProjectAssetImportSource,
        file_name: Option<String>,
        kind: Option<ProjectAssetKind>,
    ) -> Result<ProjectRelativeAssetPath, ProjectRepositoryRefusal> {
        let file_name = match (file_name, source) {
            (Some(file_name), _) => file_name,
            (None, ProjectAssetImportSource::FileOnThisMachine(source_path)) => source_path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .ok_or_else(|| ProjectRepositoryRefusal::ImportSourceUnreadable {
                    path: source_path.clone(),
                    why: "an import source names a file".to_owned(),
                })?,
            (None, ProjectAssetImportSource::UploadedContent(_)) => {
                return Err(ProjectRepositoryRefusal::UploadImportedWithNoPathNamesNoFileName);
            }
        };
        refuse_an_import_file_name(&file_name)?;
        let kind = kind.unwrap_or_else(|| ProjectAssetKind::inferred_from_file_name(&file_name));
        if kind == ProjectAssetKind::Stream {
            return Err(ProjectRepositoryRefusal::StreamAssetsAreNotImported);
        }
        let folder = kind.standard_folder_in_a_project();
        let (stem, extension) = match file_name.rsplit_once('.') {
            Some((stem, extension)) if !stem.is_empty() => (stem, Some(extension)),
            _ => (file_name.as_str(), None),
        };
        let mut number: u64 = 1;
        loop {
            let numbered_file_name = match (number, extension) {
                (1, _) => file_name.clone(),
                (_, Some(extension)) => format!("{stem}-{number}.{extension}"),
                (_, None) => format!("{stem}-{number}"),
            };
            let candidate =
                ProjectRelativeAssetPath::parse(&format!("{folder}/{numbered_file_name}"))?;
            let taken_by_an_asset =
                refuse_a_path_another_asset_holds(staged_assets, &candidate, None).is_err();
            let taken_on_disk = candidate.on_disk_under(&self.project_directory).exists()
                || self
                    .project_directory
                    .join(candidate.sidecar_path())
                    .exists();
            if !taken_by_an_asset && !taken_on_disk {
                return Ok(candidate);
            }
            number += 1;
        }
    }

    pub(super) fn operation_for_change(
        &self,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        change: ProjectAssetAction,
    ) -> Result<Option<StagedAssetChange>, ProjectRepositoryRefusal> {
        let staged_asset = |asset: Uuid| {
            staged_assets
                .get(&asset)
                .ok_or(ProjectRepositoryRefusal::NoSuchAsset(asset))
        };
        match change {
            ProjectAssetAction::ImportAsset {
                source,
                asset_path,
                file_name,
                kind,
                import_settings,
            } => {
                let path = match asset_path {
                    Some(asset_path) => ProjectRelativeAssetPath::parse(&asset_path)?,
                    None => self.the_free_path_in_the_standard_folder_of_an_import(
                        staged_assets,
                        &source,
                        file_name,
                        kind,
                    )?,
                };
                self.refuse_a_path_through_a_symbolic_link(&path)?;
                let kind = kind
                    .unwrap_or_else(|| ProjectAssetKind::inferred_from_file_name(path.file_name()));
                if kind == ProjectAssetKind::Stream {
                    return Err(ProjectRepositoryRefusal::StreamAssetsAreNotImported);
                }
                refuse_a_graph_document_path(&path)?;
                refuse_a_path_another_asset_holds(staged_assets, &path, None)?;
                let destination = path.on_disk_under(&self.project_directory);
                let asset = Uuid::new_v4();
                let (recorded, waiting) = match &source {
                    ProjectAssetImportSource::FileOnThisMachine(source_path) => {
                        if !source_path.is_absolute() {
                            return Err(ProjectRepositoryRefusal::ImportSourceUnreadable {
                                path: source_path.clone(),
                                why: "an import source is named by its absolute path".to_owned(),
                            });
                        }
                        if destination.exists() && !same_file(source_path, &destination) {
                            return Err(ProjectRepositoryRefusal::AssetPathHoldsAnotherFile {
                                path: path.to_string(),
                            });
                        }
                        if destination.exists() {
                            self.refuse_a_file_another_asset_already_is(staged_assets, &path)?;
                            (the_bytes_recorded_of_the_file_at(&destination)?, None)
                        } else {
                            let (copy, recorded) = copy_the_file_into_the_uploads_hashing_it(
                                &self.project_directory,
                                source_path,
                            )?;
                            (
                                recorded,
                                Some(AssetFileWaitingForItsImport {
                                    file: copy,
                                    removed_when_the_import_is_refused: true,
                                }),
                            )
                        }
                    }
                    ProjectAssetImportSource::UploadedContent(content_sha256) => {
                        if destination.exists() {
                            return Err(ProjectRepositoryRefusal::AssetPathHoldsAnotherFile {
                                path: path.to_string(),
                            });
                        }
                        (
                            self.the_bytes_recorded_of_the_upload(content_sha256)?,
                            Some(AssetFileWaitingForItsImport {
                                file: the_upload_file_of(&self.project_directory, content_sha256),
                                removed_when_the_import_is_refused: false,
                            }),
                        )
                    }
                };
                Ok(Some(StagedAssetChange {
                    operation: ProjectOperation::Asset(ProjectAssetOperation::ImportAsset {
                        asset: ProjectAsset {
                            asset,
                            kind,
                            path,
                            content_sha256: recorded.content_sha256,
                            content_byte_len: recorded.content_byte_len,
                            import_settings,
                        },
                    }),
                    asset_file_waiting_for_its_import: waiting.map(|waiting| (asset, waiting)),
                }))
            }
            ProjectAssetAction::RelocateAsset { asset, to_path } => {
                let current = staged_asset(asset)?;
                let to = ProjectRelativeAssetPath::parse(&to_path)?;
                Ok(self
                    .relocation(staged_assets, current, to)?
                    .map(StagedAssetChange::of))
            }
            ProjectAssetAction::DeleteAsset { asset } => {
                let current = staged_asset(asset)?;
                self.refuse_a_path_through_a_symbolic_link(&current.path)?;
                if current.kind == ProjectAssetKind::Stream {
                    return self.deletion_of_the_stream_document(staged_assets, asset);
                }
                Ok(Some(StagedAssetChange::of(
                    ProjectAssetOperation::DeleteAsset {
                        asset: current.clone(),
                    },
                )))
            }
            ProjectAssetAction::SetAssetSettings {
                asset,
                import_settings,
            } => {
                let current = staged_asset(asset)?;
                if current.kind == ProjectAssetKind::Stream {
                    return Err(ProjectRepositoryRefusal::StreamAssetHasNoImportSettings(
                        asset,
                    ));
                }
                self.refuse_a_path_through_a_symbolic_link(&current.path)?;
                if current.import_settings == import_settings {
                    return Ok(None);
                }
                Ok(Some(StagedAssetChange::of(
                    ProjectAssetOperation::SetAssetSettings {
                        asset,
                        path: current.path.clone(),
                        from: current.import_settings.clone(),
                        to: import_settings,
                    },
                )))
            }
            ProjectAssetAction::ReplaceAsset { asset, source } => {
                let current = staged_assets
                    .get(&asset)
                    .ok_or(ProjectRepositoryRefusal::NoSuchAsset(asset))?;
                if current.kind == ProjectAssetKind::Stream {
                    return Err(ProjectRepositoryRefusal::StreamAssetsAreNotReplaced(asset));
                }
                self.refuse_a_path_through_a_symbolic_link(&current.path)?;
                let (recorded, waiting) = match &source {
                    ProjectAssetImportSource::FileOnThisMachine(source_path) => {
                        if !source_path.is_absolute() {
                            return Err(ProjectRepositoryRefusal::ImportSourceUnreadable {
                                path: source_path.clone(),
                                why: "a replacement's source is named by its absolute path"
                                    .to_owned(),
                            });
                        }
                        let (copy, recorded) = copy_the_file_into_the_uploads_hashing_it(
                            &self.project_directory,
                            source_path,
                        )?;
                        (
                            recorded,
                            AssetFileWaitingForItsImport {
                                file: copy,
                                removed_when_the_import_is_refused: true,
                            },
                        )
                    }
                    ProjectAssetImportSource::UploadedContent(content_sha256) => (
                        self.the_bytes_recorded_of_the_upload(content_sha256)?,
                        AssetFileWaitingForItsImport {
                            file: the_upload_file_of(&self.project_directory, content_sha256),
                            removed_when_the_import_is_refused: false,
                        },
                    ),
                };
                if recorded.content_sha256 == current.content_sha256 {
                    if waiting.removed_when_the_import_is_refused {
                        let _ = std::fs::remove_file(&waiting.file);
                    }
                    return Ok(None);
                }
                Ok(Some(StagedAssetChange {
                    operation: ProjectOperation::Asset(
                        ProjectAssetOperation::ReplaceAssetContent {
                            asset,
                            path: current.path.clone(),
                            from: ProjectAssetContent {
                                content_sha256: current.content_sha256.clone(),
                                content_byte_len: current.content_byte_len,
                            },
                            to: ProjectAssetContent {
                                content_sha256: recorded.content_sha256,
                                content_byte_len: recorded.content_byte_len,
                            },
                        },
                    ),
                    asset_file_waiting_for_its_import: Some((asset, waiting)),
                }))
            }
            ProjectAssetAction::CreateStream { path, display_name } => {
                let display_name = refuse_a_display_name_that_shows_nothing(display_name)?;
                let path = ProjectRelativeAssetPath::parse(&path)?;
                let stream = Uuid::new_v4();
                let mut document = StreamGraphDocument::from_value(serde_json::json!({
                    STREAM_DOCUMENT_ASSET_KEY: stream,
                }))
                .map_err(|why| {
                    ProjectRepositoryRefusal::GraphDocumentUnreadable {
                        path: path.to_string(),
                        why,
                    }
                })?;
                document.set_display_name(Some(&display_name));
                Ok(Some(StagedAssetChange {
                    operation: ProjectOperation::StreamDocument(
                        ProjectStreamDocumentOperation::CreateStreamDocument {
                            stream,
                            path,
                            document: document.to_value(),
                        },
                    ),
                    asset_file_waiting_for_its_import: None,
                }))
            }
            ProjectAssetAction::DeleteStream { stream_asset } => {
                let current = staged_assets
                    .get(&stream_asset)
                    .filter(|asset| asset.kind == ProjectAssetKind::Stream)
                    .ok_or(ProjectRepositoryRefusal::NoSuchStream(stream_asset))?;
                self.refuse_a_path_through_a_symbolic_link(&current.path)?;
                self.deletion_of_the_stream_document(staged_assets, stream_asset)
            }
            ProjectAssetAction::SetStreamDisplayName {
                stream_asset,
                display_name,
            } => {
                let display_name = refuse_a_display_name_that_shows_nothing(display_name)?;
                let (path, document) =
                    self.read_stream_graph_document(staged_assets, stream_asset)?;
                self.refuse_a_path_through_a_symbolic_link(&path)?;
                let from = document.display_name().map(str::to_owned);
                if from.as_deref() == Some(display_name.as_str()) {
                    return Ok(None);
                }
                Ok(Some(StagedAssetChange {
                    operation: ProjectOperation::StreamDocument(
                        ProjectStreamDocumentOperation::SetStreamDisplayName {
                            stream: stream_asset,
                            path,
                            from,
                            to: Some(display_name),
                        },
                    ),
                    asset_file_waiting_for_its_import: None,
                }))
            }
        }
    }

    /// The operation taking the stream asset `stream`'s graph document out, as it is on disk.
    pub(super) fn deletion_of_the_stream_document(
        &self,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        stream: Uuid,
    ) -> Result<Option<StagedAssetChange>, ProjectRepositoryRefusal> {
        let (path, document) = self.read_stream_graph_document(staged_assets, stream)?;
        Ok(Some(StagedAssetChange {
            operation: ProjectOperation::StreamDocument(
                ProjectStreamDocumentOperation::DeleteStreamDocument {
                    stream,
                    path,
                    document: serde_json::from_slice(&document.canonical_bytes()?)?,
                },
            ),
            asset_file_waiting_for_its_import: None,
        }))
    }

    /// What an upload named `content_sha256` records, refused when the project holds no such
    /// upload or its bytes hash otherwise.
    pub(super) fn the_bytes_recorded_of_the_upload(
        &self,
        content_sha256: &str,
    ) -> Result<AssetFileBytesRecorded, ProjectRepositoryRefusal> {
        let no_such_upload = || ProjectRepositoryRefusal::NoSuchUpload(content_sha256.to_owned());
        if !is_a_sha256_digest(content_sha256) {
            return Err(no_such_upload());
        }
        let upload = the_upload_file_of(&self.project_directory, content_sha256);
        if !upload.is_file() {
            return Err(no_such_upload());
        }
        let recorded = the_bytes_recorded_of_the_file_at(&upload)?;
        if !recorded.content_sha256.eq_ignore_ascii_case(content_sha256) {
            return Err(ProjectRepositoryRefusal::UploadHoldsOtherBytes {
                content_sha256: content_sha256.to_owned(),
                held_sha256: recorded.content_sha256,
            });
        }
        Ok(recorded)
    }

    pub(super) fn relocation(
        &self,
        staged_assets: &BTreeMap<Uuid, ProjectAsset>,
        current: &ProjectAsset,
        to: ProjectRelativeAssetPath,
    ) -> Result<Option<ProjectAssetOperation>, ProjectRepositoryRefusal> {
        if to == current.path {
            return Ok(None);
        }
        self.refuse_a_path_through_a_symbolic_link(&current.path)?;
        self.refuse_a_path_through_a_symbolic_link(&to)?;
        if current.kind == ProjectAssetKind::Stream {
            refuse_a_stream_document_path_without_its_suffix(&to)?;
        } else {
            refuse_a_graph_document_path(&to)?;
        }
        refuse_a_path_another_asset_holds(staged_assets, &to, Some(current.asset))?;
        if to.on_disk_under(&self.project_directory).exists() {
            return Err(ProjectRepositoryRefusal::AssetPathHoldsAnotherFile {
                path: to.to_string(),
            });
        }
        Ok(Some(ProjectAssetOperation::RelocateAsset {
            asset: current.asset,
            from: current.path.clone(),
            to,
        }))
    }
}
