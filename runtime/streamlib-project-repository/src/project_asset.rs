// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::canonical_project_json::canonical_project_json_bytes;
use crate::project_file::PROJECT_FILE_NAME;
use crate::project_repository_refusal::ProjectRepositoryRefusal;

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

    /// The same file name under another directory of the project.
    pub fn moved_into_directory(&self, directory: &str) -> Result<Self, ProjectRepositoryRefusal> {
        let directory = directory.trim_end_matches('/');
        if directory.is_empty() {
            Self::parse(self.file_name())
        } else {
            Self::parse(&format!("{directory}/{}", self.file_name()))
        }
    }

    /// The same directory under another file name.
    pub fn renamed_to(&self, file_name: &str) -> Result<Self, ProjectRepositoryRefusal> {
        if file_name.contains('/') {
            return Err(ProjectRepositoryRefusal::AssetPathRefused {
                path: file_name.to_owned(),
                why: "a new file name holds no `/`; moving an asset is `move_asset`",
            });
        }
        if self.directory().is_empty() {
            Self::parse(file_name)
        } else {
            Self::parse(&format!("{}/{file_name}", self.directory()))
        }
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
    /// The SHA-256 of the asset's bytes, lowercase hex, naming them in the content store.
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
            ".tatolab/content/abc",
            ".git/config",
            ".venv/bin/python",
            "media/intro.mp4.asset.json",
            "tatolab-project.json",
            "media\\intro.mp4",
            "media/",
            "./intro.mp4",
        ] {
            assert!(
                ProjectRelativeAssetPath::parse(refused).is_err(),
                "`{refused}` should be refused"
            );
        }
    }

    #[test]
    fn a_rename_keeps_the_folder_and_a_move_keeps_the_file_name() {
        let path = ProjectRelativeAssetPath::parse("media/intro.mp4").unwrap();
        assert_eq!(
            path.renamed_to("opening.mp4").unwrap().as_str(),
            "media/opening.mp4"
        );
        assert_eq!(
            path.moved_into_directory("archive/2026").unwrap().as_str(),
            "archive/2026/intro.mp4"
        );
        assert_eq!(path.moved_into_directory("").unwrap().as_str(), "intro.mp4");
        assert!(path.renamed_to("elsewhere/opening.mp4").is_err());
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
