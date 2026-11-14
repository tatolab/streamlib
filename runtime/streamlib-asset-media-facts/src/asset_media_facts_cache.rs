// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The facts and preview picture read from an asset's bytes, kept in its project's
//! `.tatolab/asset-media/<content_sha256>/<extension>/` so the bytes are read once by the reader
//! their extension chooses, and rebuilt whenever they are missing.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    AssetMediaFactsOnTheWire, AssetPreviewOnTheWire,
};

use crate::{
    ASSET_PREVIEW_PICTURE_MEDIA_TYPE, AssetMediaFactsReadFromTheFile,
    AssetMediaFactsReadersOfThisRuntime, AssetMediaFactsUnreadable,
    read_the_media_facts_of_an_asset_file, the_lowercase_extension_of,
};

/// The folder of a project, relative to its directory, the facts read from each asset's bytes
/// are kept in, one folder per SHA-256 and one inside it per file extension.
pub const ASSET_MEDIA_FACTS_CACHE_DIRECTORY: &str = ".tatolab/asset-media";

const FACTS_FILE_NAME: &str = "facts.json";
const PREVIEW_PICTURE_FILE_NAME: &str = "preview.png";

/// The extension folder of a file with no extension; no real extension starts with `-`.
const NO_EXTENSION_FOLDER_NAME: &str = "-none";

/// The extension folder of a file whose extension is not a short run of ASCII letters and
/// digits, which no reader takes.
const UNUSUAL_EXTENSION_FOLDER_NAME: &str = "-unusual";

/// The readers' format: a kept read of another format is read again, so a reader that learns a
/// new family or fact reaches every asset.
const ASSET_MEDIA_FACTS_READER_FORMAT_VERSION: u32 = 2;

/// What the runtime keeps of one read of an asset's bytes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "media_facts_state", rename_all = "snake_case")]
pub enum AssetMediaFactsCachedInTheProject {
    /// The bytes read; each fact they did not state is `null`.
    Read {
        /// What was read.
        media_facts: Box<AssetMediaFactsOnTheWire>,
        /// The preview picture kept beside the facts, `None` for a family with none.
        preview: Option<AssetPreviewOnTheWire>,
    },
    /// No reader could read the bytes.
    Unreadable {
        /// Why, in the reader's own words.
        media_facts_unreadable_because: String,
    },
}

#[derive(Serialize, Deserialize)]
struct AssetMediaFactsFileInTheCache {
    reader_format_version: u32,
    #[serde(flatten)]
    cached: AssetMediaFactsCachedInTheProject,
}

/// The kept facts of one project's assets.
#[derive(Clone, Debug)]
pub struct AssetMediaFactsCacheOfAProject {
    cache_directory: PathBuf,
}

/// Whether `content_sha256` is a SHA-256 in lowercase hex, the only name a cache folder takes.
fn is_a_lowercase_hex_sha256(content_sha256: &str) -> bool {
    content_sha256.len() == 64
        && content_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl AssetMediaFactsCacheOfAProject {
    /// The cache of the project at `project_directory`.
    pub fn of_the_project_at(project_directory: &Path) -> Self {
        Self {
            cache_directory: project_directory.join(ASSET_MEDIA_FACTS_CACHE_DIRECTORY),
        }
    }

    fn folder_of(&self, content_sha256: &str, asset_file_path: &Path) -> Option<PathBuf> {
        let extension = the_lowercase_extension_of(asset_file_path);
        let extension_folder_name = if extension.is_empty() {
            NO_EXTENSION_FOLDER_NAME
        } else if extension.len() <= 16
            && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        {
            extension.as_str()
        } else {
            UNUSUAL_EXTENSION_FOLDER_NAME
        };
        is_a_lowercase_hex_sha256(content_sha256).then(|| {
            self.cache_directory
                .join(content_sha256)
                .join(extension_folder_name)
        })
    }

    /// What is kept for the bytes whose SHA-256 is `content_sha256`, read by the reader the
    /// extension of `asset_file_path` chooses; `None` when nothing of the current reader format
    /// is.
    pub fn cached(
        &self,
        content_sha256: &str,
        asset_file_path: &Path,
    ) -> Option<AssetMediaFactsCachedInTheProject> {
        let facts_file = self
            .folder_of(content_sha256, asset_file_path)?
            .join(FACTS_FILE_NAME);
        let facts_json = std::fs::read(facts_file).ok()?;
        let kept: AssetMediaFactsFileInTheCache = serde_json::from_slice(&facts_json).ok()?;
        (kept.reader_format_version == ASSET_MEDIA_FACTS_READER_FORMAT_VERSION)
            .then_some(kept.cached)
    }

    /// The preview picture kept for the bytes whose SHA-256 is `content_sha256`, made by the
    /// reader the extension of `asset_file_path` chooses, PNG-encoded.
    pub fn preview_png_bytes(
        &self,
        content_sha256: &str,
        asset_file_path: &Path,
    ) -> Option<Vec<u8>> {
        std::fs::read(
            self.folder_of(content_sha256, asset_file_path)?
                .join(PREVIEW_PICTURE_FILE_NAME),
        )
        .ok()
    }

    /// Read the asset file at `asset_file_path`, whose bytes' SHA-256 is `content_sha256`, and
    /// keep what was read; a keep that fails is logged and the read is still answered.
    pub fn read_and_keep(
        &self,
        content_sha256: &str,
        asset_file_path: &Path,
        readers: &AssetMediaFactsReadersOfThisRuntime,
    ) -> AssetMediaFactsCachedInTheProject {
        self.read_by_and_keep(content_sha256, asset_file_path, |asset_file_path| {
            read_the_media_facts_of_an_asset_file(asset_file_path, readers)
        })
    }

    /// Read the asset file at `asset_file_path` by `reader` and keep what was read; a reader that
    /// panics on the file leaves it kept as unreadable, so it is never read again.
    fn read_by_and_keep(
        &self,
        content_sha256: &str,
        asset_file_path: &Path,
        reader: impl FnOnce(&Path) -> Result<AssetMediaFactsReadFromTheFile, AssetMediaFactsUnreadable>,
    ) -> AssetMediaFactsCachedInTheProject {
        let read_or_unreadable =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader(asset_file_path)))
                .unwrap_or_else(|_| {
                    tracing::warn!(path = %asset_file_path.display(), "a media facts reader panicked on an asset file");
                    Err(AssetMediaFactsUnreadable::because(
                        "the runtime's reader failed on this file",
                    ))
                });
        let (cached, preview_png_bytes) = match read_or_unreadable {
            Ok(read) => {
                let preview = read
                    .preview_picture
                    .as_ref()
                    .map(|picture| AssetPreviewOnTheWire {
                        pixel_width: picture.pixel_width,
                        pixel_height: picture.pixel_height,
                        media_type: ASSET_PREVIEW_PICTURE_MEDIA_TYPE.to_owned(),
                    });
                (
                    AssetMediaFactsCachedInTheProject::Read {
                        media_facts: Box::new(read.media_facts),
                        preview,
                    },
                    read.preview_picture.map(|picture| picture.png_bytes),
                )
            }
            Err(unreadable) => (
                AssetMediaFactsCachedInTheProject::Unreadable {
                    media_facts_unreadable_because: unreadable.0,
                },
                None,
            ),
        };
        if let Err(why) = self.keep(
            content_sha256,
            asset_file_path,
            &cached,
            preview_png_bytes.as_deref(),
        ) {
            tracing::warn!(content_sha256, %why, "an asset's media facts were read and not kept; they are read again next time");
        }
        cached
    }

    fn keep(
        &self,
        content_sha256: &str,
        asset_file_path: &Path,
        cached: &AssetMediaFactsCachedInTheProject,
        preview_png_bytes: Option<&[u8]>,
    ) -> std::io::Result<()> {
        let Some(folder) = self.folder_of(content_sha256, asset_file_path) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("`{content_sha256}` is no SHA-256 in lowercase hex"),
            ));
        };
        std::fs::create_dir_all(&folder)?;
        if let Some(preview_png_bytes) = preview_png_bytes {
            write_by_rename(&folder.join(PREVIEW_PICTURE_FILE_NAME), preview_png_bytes)?;
        }
        let facts_json = serde_json::to_vec_pretty(&AssetMediaFactsFileInTheCache {
            reader_format_version: ASSET_MEDIA_FACTS_READER_FORMAT_VERSION,
            cached: cached.clone(),
        })
        .map_err(std::io::Error::other)?;
        write_by_rename(&folder.join(FACTS_FILE_NAME), &facts_json)
    }
}

/// Write `bytes` beside `path` and rename them onto it, so a reader never sees half a file.
fn write_by_rename(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut being_written = path.as_os_str().to_owned();
    being_written.push(".being-written");
    let being_written = PathBuf::from(being_written);
    std::fs::write(&being_written, bytes)?;
    std::fs::rename(&being_written, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA256_OF_SOMETHING: &str =
        "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

    #[test]
    fn a_read_is_kept_under_its_sha256_and_answered_again_without_the_file() {
        let project = tempfile::tempdir().unwrap();
        let cover = project.path().join("cover.png");
        image::RgbImage::from_pixel(1000, 500, image::Rgb([0, 0, 255]))
            .save(&cover)
            .unwrap();
        let cache = AssetMediaFactsCacheOfAProject::of_the_project_at(project.path());
        assert_eq!(cache.cached(SHA256_OF_SOMETHING, &cover), None);
        let read = cache.read_and_keep(
            SHA256_OF_SOMETHING,
            &cover,
            &AssetMediaFactsReadersOfThisRuntime::default(),
        );
        std::fs::remove_file(&cover).unwrap();
        assert_eq!(
            cache.cached(SHA256_OF_SOMETHING, &cover),
            Some(read.clone())
        );
        let AssetMediaFactsCachedInTheProject::Read { preview, .. } = read else {
            panic!("the image read: {read:?}");
        };
        assert_eq!(
            preview,
            Some(AssetPreviewOnTheWire {
                pixel_width: 512,
                pixel_height: 256,
                media_type: "image/png".to_owned()
            })
        );
        let png_bytes = cache
            .preview_png_bytes(SHA256_OF_SOMETHING, &cover)
            .unwrap();
        assert!(png_bytes.starts_with(b"\x89PNG"));
        assert!(
            project
                .path()
                .join(ASSET_MEDIA_FACTS_CACHE_DIRECTORY)
                .join(SHA256_OF_SOMETHING)
                .join("png")
                .join(FACTS_FILE_NAME)
                .is_file()
        );
    }

    #[test]
    fn an_unreadable_file_is_kept_as_unreadable_with_why() {
        let project = tempfile::tempdir().unwrap();
        let model = project.path().join("weights.onnx");
        std::fs::write(&model, b"weights").unwrap();
        let cache = AssetMediaFactsCacheOfAProject::of_the_project_at(project.path());
        let read = cache.read_and_keep(
            SHA256_OF_SOMETHING,
            &model,
            &AssetMediaFactsReadersOfThisRuntime::default(),
        );
        assert_eq!(
            read,
            AssetMediaFactsCachedInTheProject::Unreadable {
                media_facts_unreadable_because: "the runtime has no reader for `.onnx` files"
                    .to_owned()
            }
        );
        assert_eq!(cache.cached(SHA256_OF_SOMETHING, &model), Some(read));
        assert_eq!(cache.preview_png_bytes(SHA256_OF_SOMETHING, &model), None);
    }

    #[test]
    fn a_name_that_is_no_sha256_never_reaches_the_disk() {
        let project = tempfile::tempdir().unwrap();
        let cache = AssetMediaFactsCacheOfAProject::of_the_project_at(project.path());
        let lut = project.path().join("film.cube");
        std::fs::write(&lut, "LUT_3D_SIZE 2\n").unwrap();
        cache.read_and_keep(
            "../../escape",
            &lut,
            &AssetMediaFactsReadersOfThisRuntime::default(),
        );
        assert_eq!(cache.cached("../../escape", &lut), None);
        assert!(
            !project
                .path()
                .join(ASSET_MEDIA_FACTS_CACHE_DIRECTORY)
                .exists()
        );
    }

    #[test]
    fn a_kept_read_of_another_reader_format_is_read_again() {
        let project = tempfile::tempdir().unwrap();
        let cache = AssetMediaFactsCacheOfAProject::of_the_project_at(project.path());
        let lut = project.path().join("film.cube");
        let folder = cache.folder_of(SHA256_OF_SOMETHING, &lut).unwrap();
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join(FACTS_FILE_NAME),
            r#"{"reader_format_version":0,"media_facts_state":"unreadable","media_facts_unreadable_because":"old"}"#,
        )
        .unwrap();
        assert_eq!(cache.cached(SHA256_OF_SOMETHING, &lut), None);
    }

    #[test]
    fn the_same_bytes_under_another_extension_are_read_again_by_its_reader() {
        let project = tempfile::tempdir().unwrap();
        let cache = AssetMediaFactsCacheOfAProject::of_the_project_at(project.path());
        let logo_as_data = project.path().join("logo.bin");
        image::RgbImage::from_pixel(4, 4, image::Rgb([255, 0, 0]))
            .save_with_format(&logo_as_data, image::ImageFormat::Png)
            .unwrap();
        let read_as_data = cache.read_and_keep(
            SHA256_OF_SOMETHING,
            &logo_as_data,
            &AssetMediaFactsReadersOfThisRuntime::default(),
        );
        assert!(matches!(
            read_as_data,
            AssetMediaFactsCachedInTheProject::Unreadable { .. }
        ));
        let logo_as_image = project.path().join("logo.png");
        std::fs::rename(&logo_as_data, &logo_as_image).unwrap();
        assert_eq!(cache.cached(SHA256_OF_SOMETHING, &logo_as_image), None);
        assert!(matches!(
            cache.read_and_keep(
                SHA256_OF_SOMETHING,
                &logo_as_image,
                &AssetMediaFactsReadersOfThisRuntime::default()
            ),
            AssetMediaFactsCachedInTheProject::Read { .. }
        ));
        assert_eq!(
            cache.cached(SHA256_OF_SOMETHING, &logo_as_data),
            Some(read_as_data)
        );
    }

    #[test]
    fn a_reader_that_panics_leaves_the_file_kept_as_unreadable() {
        let project = tempfile::tempdir().unwrap();
        let cache = AssetMediaFactsCacheOfAProject::of_the_project_at(project.path());
        let clip = project.path().join("clip.mp4");
        std::fs::write(&clip, b"not a movie").unwrap();
        let unreadable = AssetMediaFactsCachedInTheProject::Unreadable {
            media_facts_unreadable_because: "the runtime's reader failed on this file".to_owned(),
        };
        assert_eq!(
            cache.read_by_and_keep(SHA256_OF_SOMETHING, &clip, |_| panic!("a reader defect")),
            unreadable
        );
        assert_eq!(cache.cached(SHA256_OF_SOMETHING, &clip), Some(unreadable));
    }
}
