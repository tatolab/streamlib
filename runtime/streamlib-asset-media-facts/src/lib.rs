// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What a project asset's bytes hold — duration, picture size, frame rate, codec, sample rate,
//! a LUT's size — and its preview picture, read from its file by one reader per file family and
//! kept in the project's `.tatolab/asset-media/` under the bytes' SHA-256.

mod asset_media_facts_cache;
mod audio_file_media_facts;
mod cube_lut_file_media_facts;
mod image_file_media_facts;
mod iso_base_media_file_facts;
mod matroska_file_media_facts;

use std::path::Path;

pub use asset_media_facts_cache::{
    ASSET_MEDIA_FACTS_CACHE_DIRECTORY, AssetMediaFactsCacheOfAProject,
    AssetMediaFactsCachedInTheProject,
};
use streamlib_runtime_client_contract::local_api_wire_contract::AssetMediaFactsOnTheWire;

/// The longest edge, in pixels, of a preview picture the runtime makes.
pub const ASSET_PREVIEW_PICTURE_LONGEST_EDGE_PIXELS: u32 = 512;

/// The media type of every preview picture the runtime makes.
pub const ASSET_PREVIEW_PICTURE_MEDIA_TYPE: &str = "image/png";

/// A preview picture a reader made, PNG-encoded.
#[derive(Clone, Debug, PartialEq)]
pub struct AssetPreviewPictureMadeFromTheFile {
    /// Its width in pixels.
    pub pixel_width: u32,
    /// Its height in pixels.
    pub pixel_height: u32,
    /// The PNG file's bytes.
    pub png_bytes: Vec<u8>,
}

/// What a reader read from one asset file.
#[derive(Clone, Debug, PartialEq)]
pub struct AssetMediaFactsReadFromTheFile {
    /// The facts, each `None` the file did not state or the reader could not read.
    pub media_facts: AssetMediaFactsOnTheWire,
    /// The preview picture, for a file family that has one.
    pub preview_picture: Option<AssetPreviewPictureMadeFromTheFile>,
}

/// Why no reader could read an asset file, in the reader's own words.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AssetMediaFactsUnreadable(pub String);

impl AssetMediaFactsUnreadable {
    pub(crate) fn because(why: impl Into<String>) -> Self {
        Self(why.into())
    }
}

/// Read the media facts and preview picture of the asset file at `asset_file_path`, choosing
/// the reader by its file extension.
pub fn read_the_media_facts_of_an_asset_file(
    asset_file_path: &Path,
) -> Result<AssetMediaFactsReadFromTheFile, AssetMediaFactsUnreadable> {
    if !asset_file_path.is_file() {
        return Err(AssetMediaFactsUnreadable::because(
            "no file is at the asset's path",
        ));
    }
    let extension = the_lowercase_extension_of(asset_file_path);
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff" | "hdr" | "exr" => {
            image_file_media_facts::read_an_image_file(asset_file_path)
        }
        "mp4" | "m4v" | "mov" | "m4a" => {
            iso_base_media_file_facts::read_an_iso_base_media_file(asset_file_path)
        }
        "mkv" | "webm" | "mka" => matroska_file_media_facts::read_a_matroska_file(asset_file_path),
        "wav" | "mp3" | "flac" | "aac" | "ogg" | "oga" | "opus" | "aiff" | "aif" | "caf" => {
            audio_file_media_facts::read_an_audio_file(asset_file_path, &extension)
        }
        "cube" => cube_lut_file_media_facts::read_a_cube_lut_file(asset_file_path),
        "" => Err(AssetMediaFactsUnreadable::because(
            "the runtime has no reader for a file with no extension",
        )),
        other => Err(AssetMediaFactsUnreadable::because(format!(
            "the runtime has no reader for `.{other}` files"
        ))),
    }
}

/// The extension of `asset_file_path` in lowercase, the one that chooses its reader; empty for
/// none.
pub(crate) fn the_lowercase_extension_of(asset_file_path: &Path) -> String {
    asset_file_path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default()
}

/// Whole milliseconds in `ticks` of a clock running `ticks_per_second`.
pub(crate) fn milliseconds_of_ticks(ticks: u64, ticks_per_second: u64) -> Option<u64> {
    if ticks_per_second == 0 {
        return None;
    }
    u64::try_from(u128::from(ticks) * 1_000 / u128::from(ticks_per_second)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_the_runtime_has_no_reader_for_is_unreadable_in_words_naming_its_extension() {
        let directory = tempfile::tempdir().unwrap();
        let weights = directory.path().join("weights.safetensors");
        std::fs::write(&weights, b"not media").unwrap();
        assert_eq!(
            read_the_media_facts_of_an_asset_file(&weights),
            Err(AssetMediaFactsUnreadable::because(
                "the runtime has no reader for `.safetensors` files"
            ))
        );
        assert_eq!(
            read_the_media_facts_of_an_asset_file(&directory.path().join("gone.png")),
            Err(AssetMediaFactsUnreadable::because(
                "no file is at the asset's path"
            ))
        );
    }

    #[test]
    fn ticks_become_whole_milliseconds() {
        assert_eq!(milliseconds_of_ticks(90_000 * 42, 90_000), Some(42_000));
        assert_eq!(milliseconds_of_ticks(1, 3), Some(333));
        assert_eq!(milliseconds_of_ticks(5, 0), None);
    }
}
