// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::canonical_project_json::canonical_project_json_bytes;
use crate::project_file::PROJECT_FILE_NAME;
use crate::project_repository_refusal::ProjectRepositoryRefusal;

/// What ends a stream asset's graph document: `streams/main.graph.json`.
pub const GRAPH_DOCUMENT_FILE_SUFFIX: &str = ".graph.json";

/// Folders a project's scan for sidecars and graph documents never enters, so no asset sits in one.
pub(crate) const DIRECTORIES_A_PROJECT_SCAN_SKIPS: &[&str] = &["node_modules", "__pycache__"];

/// The folder at a project's root that holds each project it imports, one folder per import.
pub const PROJECT_IMPORTS_DIRECTORY: &str = "imports";

/// What follows an asset's file name to name its sidecar: `intro.mp4` → `intro.mp4.asset.json`.
pub const ASSET_SIDECAR_FILE_SUFFIX: &str = ".asset.json";

/// What an asset is, as the studio and the runtime treat it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAssetKind {
    /// A stream's graph document.
    Stream,
    /// Video, audio or an image.
    Media,
    /// USD, glTF and other 3D scenes.
    Scene,
    /// Everything else a node reads: an FMU, a LUT, model weights, a font.
    Data,
}

impl ProjectAssetKind {
    /// The kind an import takes when the caller names none, from the file's extension.
    pub fn inferred_from_file_name(file_name: &str) -> Self {
        let extension = file_name
            .rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .unwrap_or_default();
        match extension.as_str() {
            "mp4" | "mov" | "mkv" | "webm" | "avi" | "mxf" | "wav" | "mp3" | "flac" | "aac"
            | "ogg" | "opus" | "aiff" | "png" | "jpg" | "jpeg" | "exr" | "tif" | "tiff"
            | "webp" | "gif" | "bmp" | "hdr" | "dpx" => Self::Media,
            "usd" | "usda" | "usdc" | "usdz" | "gltf" | "glb" | "obj" | "fbx" | "abc" | "ply"
            | "stl" => Self::Scene,
            _ => Self::Data,
        }
    }

    /// The project folder an asset of this kind is imported into when the caller names no path.
    pub fn standard_folder_in_a_project(self) -> &'static str {
        match self {
            Self::Stream => "streams",
            Self::Media => "media",
            Self::Scene => "scenes",
            Self::Data => "data",
        }
    }
}

/// An asset's path inside its project: relative, `/`-separated, never into what the runtime keeps.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProjectRelativeAssetPath(String);

impl ProjectRelativeAssetPath {
    /// Check `path` names a file a project may track as an asset.
    pub fn parse(path: &str) -> Result<Self, ProjectRepositoryRefusal> {
        let refuse = |why: &'static str| ProjectRepositoryRefusal::AssetPathRefused {
            path: path.to_owned(),
            why,
        };
        if path.is_empty() {
            return Err(refuse("it is empty"));
        }
        if path.contains('\\') {
            return Err(refuse("its separator is `/`, never `\\`"));
        }
        if Path::new(path).is_absolute() || path.starts_with('/') {
            return Err(refuse(
                "it is absolute; an asset path is relative to its project",
            ));
        }
        if path.split('/').any(str::is_empty) {
            return Err(refuse("it holds an empty segment"));
        }
        if path
            .split('/')
            .any(|segment| DIRECTORIES_A_PROJECT_SCAN_SKIPS.contains(&segment))
        {
            return Err(refuse(
                "a segment is `node_modules` or `__pycache__`, which a project's scan never enters",
            ));
        }
        for component in Path::new(path).components() {
            match component {
                Component::Normal(segment) => {
                    let segment = segment.to_string_lossy();
                    if segment.starts_with('.') {
                        return Err(refuse(
                            "a segment starts with `.`, which names what the runtime, Git or a venv keeps",
                        ));
                    }
                }
                _ => return Err(refuse("it holds `.`, `..` or a root")),
            }
        }
        if path.ends_with('/') {
            return Err(refuse("it names a directory, not a file"));
        }
        if path.split('/').next() == Some(PROJECT_IMPORTS_DIRECTORY) {
            return Err(refuse(
                "it is inside `imports/`, which holds the projects this one imports; open an \
                 import as its own project to change its assets",
            ));
        }
        if path.ends_with(ASSET_SIDECAR_FILE_SUFFIX) {
            return Err(refuse("it names an asset sidecar"));
        }

        if path == PROJECT_FILE_NAME {
            return Err(refuse("it names the project file"));
        }
        Ok(Self(path.to_owned()))
    }

    /// The path as the project spells it.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The last segment: the file's own name.
    pub fn file_name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// The directory part, empty at the project's root.
    pub fn directory(&self) -> &str {
        self.0
            .rsplit_once('/')
            .map(|(directory, _)| directory)
            .unwrap_or("")
    }

    /// The path of this asset's sidecar inside the project.
    pub fn sidecar_path(&self) -> String {
        format!("{}{ASSET_SIDECAR_FILE_SUFFIX}", self.0)
    }

    /// The asset's file on disk under `project_directory`.
    pub fn on_disk_under(&self, project_directory: &Path) -> PathBuf {
        project_directory.join(&self.0)
    }
}

impl TryFrom<String> for ProjectRelativeAssetPath {
    type Error = ProjectRepositoryRefusal;
    fn try_from(path: String) -> Result<Self, Self::Error> {
        Self::parse(&path)
    }
}

impl From<ProjectRelativeAssetPath> for String {
    fn from(path: ProjectRelativeAssetPath) -> Self {
        path.0
    }
}

impl fmt::Display for ProjectRelativeAssetPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// What an asset's sidecar `<file>.asset.json` holds; its path is the asset's path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectAssetSidecar {
    /// The asset's UUID, generated at its import and never changed.
    pub asset: Uuid,
    /// What the asset is.
    pub kind: ProjectAssetKind,
    /// The SHA-256 of the asset's bytes when they were imported, lowercase hex.
    pub content_sha256: String,
    /// The length of the asset's bytes.
    pub content_byte_len: u64,
    /// Settings the import or a later `set_asset_settings` gave the asset.
    #[serde(default)]
    pub import_settings: Map<String, Value>,
}

/// One asset of a project: its sidecar and where it sits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectAsset {
    /// The asset's UUID.
    pub asset: Uuid,
    /// What the asset is.
    pub kind: ProjectAssetKind,
    /// Where the asset's file sits in the project.
    pub path: ProjectRelativeAssetPath,
    /// The SHA-256 of its bytes.
    pub content_sha256: String,
    /// The length of its bytes.
    pub content_byte_len: u64,
    /// Its import settings.
    pub import_settings: Map<String, Value>,
}

impl ProjectAsset {
    /// The asset its sidecar at `path` describes.
    pub fn from_sidecar(path: ProjectRelativeAssetPath, sidecar: ProjectAssetSidecar) -> Self {
        Self {
            asset: sidecar.asset,
            kind: sidecar.kind,
            path,
            content_sha256: sidecar.content_sha256,
            content_byte_len: sidecar.content_byte_len,
            import_settings: sidecar.import_settings,
        }
    }

    /// The sidecar that records this asset.
    pub fn sidecar(&self) -> ProjectAssetSidecar {
        ProjectAssetSidecar {
            asset: self.asset,
            kind: self.kind,
            content_sha256: self.content_sha256.clone(),
            content_byte_len: self.content_byte_len,
            import_settings: self.import_settings.clone(),
        }
    }

    /// The sidecar's canonical bytes.
    pub fn sidecar_bytes(&self) -> Result<Vec<u8>, ProjectRepositoryRefusal> {
        Ok(canonical_project_json_bytes(&self.sidecar())?)
    }
}

/// Every file under `project_directory` whose name ends with `suffix`, relative and sorted, past
/// hidden folders and the ones a scan skips.
pub(crate) fn project_files_ending_with(
    project_directory: &Path,
    suffix: &str,
) -> Result<Vec<String>, ProjectRepositoryRefusal> {
    let mut found = Vec::new();
    let mut directories_to_read = vec![(project_directory.to_path_buf(), String::new())];
    while let Some((directory, relative_directory)) = directories_to_read.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            let relative = if relative_directory.is_empty() {
                name.clone()
            } else {
                format!("{relative_directory}/{name}")
            };
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                let is_the_imports_folder =
                    relative_directory.is_empty() && name == PROJECT_IMPORTS_DIRECTORY;
                if !is_the_imports_folder
                    && !DIRECTORIES_A_PROJECT_SCAN_SKIPS.contains(&name.as_str())
                {
                    directories_to_read.push((entry.path(), relative));
                }
            } else if file_type.is_file() && name.ends_with(suffix) {
                found.push(relative);
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Every file of the project that its history records, project-relative and sorted: each
/// regular file outside a hidden folder, a folder a project scan skips and a Python virtual
/// environment (a folder holding `pyvenv.cfg`), leaving out each file with a sidecar beside it,
/// which history records by its sidecar alone, the project's own sidecars, which history takes
/// from the assets it records, and symbolic links, whose target is no file of the project. An
/// import's sidecars are recorded as its files.
#[cfg(feature = "history")]
pub(crate) fn project_files_the_history_records(
    project_directory: &Path,
) -> Result<Vec<String>, ProjectRepositoryRefusal> {
    let mut found = Vec::new();
    let mut directories_to_read = vec![(project_directory.to_path_buf(), String::new())];
    while let Some((directory, relative_directory)) = directories_to_read.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if relative_directory.is_empty() {
                name.clone()
            } else {
                format!("{relative_directory}/{name}")
            };
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                let is_recorded_folder = !name.starts_with('.')
                    && !DIRECTORIES_A_PROJECT_SCAN_SKIPS.contains(&name.as_str())
                    && !entry.path().join("pyvenv.cfg").exists();
                if is_recorded_folder {
                    directories_to_read.push((entry.path(), relative));
                }
            } else if file_type.is_file() {
                let is_the_projects_own_sidecar = name.ends_with(ASSET_SIDECAR_FILE_SUFFIX)
                    && !relative.starts_with(&format!("{PROJECT_IMPORTS_DIRECTORY}/"));
                let has_a_sidecar_beside_it = directory
                    .join(format!("{name}{ASSET_SIDECAR_FILE_SUFFIX}"))
                    .is_file();
                if !is_the_projects_own_sidecar && !has_a_sidecar_beside_it {
                    found.push(relative);
                }
            }
        }
    }
    found.sort();
    Ok(found)
}

/// Every asset the project at `project_directory` holds, by UUID: each sidecar's, and each
/// stream's its graph document names.
pub fn every_asset_uuid_the_project_holds(
    project_directory: &Path,
    project: Uuid,
) -> Result<BTreeSet<Uuid>, ProjectRepositoryRefusal> {
    let mut asset_uuids: BTreeSet<Uuid> = read_every_asset_sidecar(project_directory)?
        .into_keys()
        .collect();
    for stream_document in
        crate::project_stream_document::project_stream_documents(project_directory)?
    {
        asset_uuids.insert(stream_document.stream_asset(project));
    }
    Ok(asset_uuids)
}

/// The file of the asset `asset`: the one file at its path, which a reader opens and never
/// writes. Refused when the project holds no such asset, or when no file is at its path.
pub fn the_file_of_an_asset(
    project_directory: &Path,
    asset: Uuid,
) -> Result<PathBuf, ProjectRepositoryRefusal> {
    let held_asset = read_every_asset_sidecar(project_directory)?
        .remove(&asset)
        .ok_or(ProjectRepositoryRefusal::NoSuchAsset(asset))?;
    let file = held_asset.path.on_disk_under(project_directory);
    if !file.is_file() {
        return Err(ProjectRepositoryRefusal::AssetFileMissing { asset, file });
    }
    Ok(file)
}

pub(crate) fn read_every_asset_sidecar(
    project_directory: &Path,
) -> Result<BTreeMap<Uuid, ProjectAsset>, ProjectRepositoryRefusal> {
    let mut assets_by_uuid: BTreeMap<Uuid, ProjectAsset> = BTreeMap::new();
    for sidecar_path in project_files_ending_with(project_directory, ASSET_SIDECAR_FILE_SUFFIX)? {
        let on_disk = project_directory.join(&sidecar_path);
        let unreadable = |why: String| ProjectRepositoryRefusal::AssetSidecarUnreadable {
            path: on_disk.clone(),
            why,
        };
        let asset_path = ProjectRelativeAssetPath::parse(
            sidecar_path
                .strip_suffix(ASSET_SIDECAR_FILE_SUFFIX)
                .unwrap_or(&sidecar_path),
        )
        .map_err(|refusal| unreadable(refusal.to_string()))?;
        let bytes = std::fs::read(&on_disk)?;
        let sidecar: ProjectAssetSidecar =
            serde_json::from_slice(&bytes).map_err(|error| unreadable(error.to_string()))?;
        if let Some(earlier) = assets_by_uuid.get(&sidecar.asset) {
            return Err(ProjectRepositoryRefusal::AssetNamedTwice {
                asset: sidecar.asset,
                first: earlier.path.sidecar_path(),
                second: sidecar_path,
            });
        }
        assets_by_uuid.insert(
            sidecar.asset,
            ProjectAsset::from_sidecar(asset_path, sidecar),
        );
    }
    Ok(assets_by_uuid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_path_into_a_folder_is_an_asset_path() {
        let path = ProjectRelativeAssetPath::parse("media/intro.mp4").unwrap();
        assert_eq!(path.file_name(), "intro.mp4");
        assert_eq!(path.directory(), "media");
        assert_eq!(path.sidecar_path(), "media/intro.mp4.asset.json");
    }

    #[test]
    fn paths_into_what_the_runtime_keeps_or_out_of_the_project_are_refused() {
        for refused in [
            "",
            "/etc/passwd",
            "../outside.mp4",
            "media/../../outside.mp4",
            ".tatolab/uploads/abc",
            ".git/config",
            ".venv/bin/python",
            "media/intro.mp4.asset.json",
            "tatolab-project.json",
            "media\\intro.mp4",
            "media/",
            "./intro.mp4",
            "media//intro.mp4",
            "node_modules/pkg/logo.png",
            "media/__pycache__/x.png",
        ] {
            assert!(
                ProjectRelativeAssetPath::parse(refused).is_err(),
                "`{refused}` should be refused"
            );
        }
    }

    #[test]
    fn kinds_are_inferred_from_the_extension() {
        assert_eq!(
            ProjectAssetKind::inferred_from_file_name("a.MP4"),
            ProjectAssetKind::Media
        );
        assert_eq!(
            ProjectAssetKind::inferred_from_file_name("arm.usd"),
            ProjectAssetKind::Scene
        );
        assert_eq!(
            ProjectAssetKind::inferred_from_file_name("arm.fmu"),
            ProjectAssetKind::Data
        );
        assert_eq!(
            ProjectAssetKind::inferred_from_file_name("noextension"),
            ProjectAssetKind::Data
        );
    }
}
