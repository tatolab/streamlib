// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::Path;

use serde_json::Value;
use uuid::Uuid;

use crate::project_asset::{
    GRAPH_DOCUMENT_FILE_SUFFIX, ProjectRelativeAssetPath, project_files_ending_with,
};
use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// The top-level key of a graph document that holds its stream asset's UUID.
pub const STREAM_DOCUMENT_ASSET_KEY: &str = "asset";

/// One stream asset's graph document as it sits in its project: its path and its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectStreamDocument {
    /// Where the document sits in the project: `streams/main.graph.json`.
    pub path: ProjectRelativeAssetPath,
    /// The document's bytes, as read.
    pub bytes: Vec<u8>,
}

impl ProjectStreamDocument {
    /// The stream's name: the document's file name without `.graph.json`.
    pub fn stream_name(&self) -> &str {
        let file_name = self.path.file_name();
        file_name
            .strip_suffix(GRAPH_DOCUMENT_FILE_SUFFIX)
            .unwrap_or(file_name)
    }

    /// The stream asset's UUID the document holds under `asset`, or why it holds none.
    pub fn asset_written_in_it(&self) -> Result<Uuid, String> {
        let document: Value = serde_json::from_slice(&self.bytes)
            .map_err(|not_parsed| format!("the graph document does not parse: {not_parsed}"))?;
        let written = document
            .get(STREAM_DOCUMENT_ASSET_KEY)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                format!(
                    "the graph document holds no `{STREAM_DOCUMENT_ASSET_KEY}`, the UUID of its \
                     stream asset"
                )
            })?;
        Uuid::parse_str(written).map_err(|not_a_uuid| {
            format!("the graph document's `{STREAM_DOCUMENT_ASSET_KEY}` `{written}` is not a UUID: {not_a_uuid}")
        })
    }

    /// The stream asset's UUID: the one the document holds, or, while it holds none it can be
    /// read by, one derived from `project` and the document's path, so the stream still has an
    /// identity to be recorded failed under.
    pub fn stream_asset(&self, project: Uuid) -> Uuid {
        self.asset_written_in_it()
            .unwrap_or_else(|_| Uuid::new_v5(&project, self.path.as_str().as_bytes()))
    }
}

/// Every stream asset's graph document in the project at `project_directory`, sorted by path.
pub fn project_stream_documents(
    project_directory: &Path,
) -> Result<Vec<ProjectStreamDocument>, ProjectRepositoryRefusal> {
    let mut stream_documents = Vec::new();
    for relative_path in project_files_ending_with(project_directory, GRAPH_DOCUMENT_FILE_SUFFIX)? {
        let path = ProjectRelativeAssetPath::parse(&relative_path)?;
        let bytes = std::fs::read(path.on_disk_under(project_directory))?;
        stream_documents.push(ProjectStreamDocument { path, bytes });
    }
    Ok(stream_documents)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document_at(path: &str, bytes: &str) -> ProjectStreamDocument {
        ProjectStreamDocument {
            path: ProjectRelativeAssetPath::parse(path).unwrap(),
            bytes: bytes.as_bytes().to_vec(),
        }
    }

    #[test]
    fn a_document_names_its_stream_asset_and_its_stream() {
        let asset = Uuid::new_v4();
        let document = document_at(
            "streams/arm-control.graph.json",
            &format!("{{\"asset\": \"{asset}\", \"nodes\": []}}"),
        );
        assert_eq!(document.stream_name(), "arm-control");
        assert_eq!(document.asset_written_in_it(), Ok(asset));
        assert_eq!(document.stream_asset(Uuid::new_v4()), asset);
    }

    #[test]
    fn a_document_holding_no_readable_asset_still_has_one_stable_identity() {
        let project = Uuid::new_v4();
        for unreadable in ["{", "{\"nodes\": []}", "{\"asset\": \"not-a-uuid\"}"] {
            let document = document_at("streams/main.graph.json", unreadable);
            assert!(document.asset_written_in_it().is_err(), "{unreadable}");
            assert_eq!(
                document.stream_asset(project),
                document_at("streams/main.graph.json", "[]").stream_asset(project)
            );
        }
        assert_ne!(
            document_at("streams/main.graph.json", "{").stream_asset(project),
            document_at("streams/other.graph.json", "{").stream_asset(project)
        );
    }

    #[test]
    fn the_project_scan_finds_every_document_outside_what_the_runtime_keeps() {
        let project_directory = tempfile::tempdir().unwrap();
        for path in [
            "streams/main.graph.json",
            "streams/nested/second.graph.json",
            ".tatolab/hidden.graph.json",
            "node_modules/skipped.graph.json",
            "streams/notes.json",
        ] {
            let on_disk = project_directory.path().join(path);
            std::fs::create_dir_all(on_disk.parent().unwrap()).unwrap();
            std::fs::write(on_disk, "{}").unwrap();
        }
        let found: Vec<String> = project_stream_documents(project_directory.path())
            .unwrap()
            .into_iter()
            .map(|document| document.path.to_string())
            .collect();
        assert_eq!(
            found,
            [
                "streams/main.graph.json",
                "streams/nested/second.graph.json"
            ]
        );
    }
}
