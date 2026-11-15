// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Map, json};
use streamlib_project_repository::{
    PROJECT_FILE_NAME, PROJECT_HISTORY_DIRECTORY, PROJECT_TRANSACTION_COMMIT_TRAILER,
    PROJECT_UPLOADS_DIRECTORY, ProjectAssetAction, ProjectAssetImportSource, ProjectAssetKind,
    ProjectAssetOperation, ProjectOperation, ProjectRepository, ProjectRepositoryRefusal,
    ProjectStreamDocumentOperation, ProjectTransaction, ProjectTransactionAuthor,
    ProjectTransactionAuthorKind, the_file_of_an_asset, the_upload_file_of,
};
use uuid::Uuid;

mod asset_import_paths_and_refusals_tests;
mod asset_lifecycle_and_history_tests;
mod stream_assets_and_asset_files_tests;

struct ProjectBesideItsImportSources {
    _root: tempfile::TempDir,
    project_directory: PathBuf,
    import_sources_directory: PathBuf,
}

impl ProjectBesideItsImportSources {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let project_directory = root.path().join("robot-cell");
        let import_sources_directory = root.path().join("downloads");
        std::fs::create_dir_all(&import_sources_directory).unwrap();
        Self {
            _root: root,
            project_directory,
            import_sources_directory,
        }
    }

    fn import_source(&self, file_name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.import_sources_directory.join(file_name);
        std::fs::write(&path, bytes).unwrap();
        path
    }
}

fn agent() -> ProjectTransactionAuthor {
    ProjectTransactionAuthor {
        kind: ProjectTransactionAuthorKind::Agent,
        name: "rig agent".to_owned(),
    }
}

fn import_from(source: &Path, asset_path: &str) -> ProjectAssetAction {
    ProjectAssetAction::ImportAsset {
        source: ProjectAssetImportSource::FileOnThisMachine(source.to_path_buf()),
        asset_path: Some(asset_path.to_owned()),
        file_name: None,
        kind: None,
        import_settings: Map::new(),
    }
}

fn imported_asset(applied: &streamlib_project_repository::AppliedProjectTransaction) -> Uuid {
    match &applied.transaction.operations[0] {
        ProjectOperation::Asset(ProjectAssetOperation::ImportAsset { asset }) => asset.asset,
        other => panic!("expected an import, got {other:?}"),
    }
}

fn transactions_in_history(repository: &ProjectRepository) -> Vec<ProjectTransaction> {
    repository
        .history()
        .commits_newest_first()
        .unwrap()
        .into_iter()
        .filter_map(|commit| {
            commit
                .message
                .lines()
                .find_map(|line| {
                    line.strip_prefix(&format!("{PROJECT_TRANSACTION_COMMIT_TRAILER}: "))
                })
                .map(|json| serde_json::from_str(json).unwrap())
        })
        .collect()
}

fn write_stream_document(project_directory: &Path, path: &str, document: &serde_json::Value) {
    let on_disk = project_directory.join(path);
    std::fs::create_dir_all(on_disk.parent().unwrap()).unwrap();
    std::fs::write(on_disk, serde_json::to_vec_pretty(document).unwrap()).unwrap();
}
