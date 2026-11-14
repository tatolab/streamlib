// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Still images: the picture size from the header, and the picture decoded and scaled for the
//! preview.

use std::io::Cursor;
use std::path::Path;

use image::{ImageFormat, ImageReader};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    AssetImageFactsOnTheWire, AssetMediaFactsOnTheWire,
};

use crate::{
    ASSET_PREVIEW_PICTURE_LONGEST_EDGE_PIXELS, AssetMediaFactsReadFromTheFile,
    AssetMediaFactsUnreadable, AssetPreviewPictureMadeFromTheFile,
};

pub(crate) fn read_an_image_file(
    image_file_path: &Path,
) -> Result<AssetMediaFactsReadFromTheFile, AssetMediaFactsUnreadable> {
    let opened = |why_prefix: &str| {
        ImageReader::open(image_file_path)
            .and_then(ImageReader::with_guessed_format)
            .map_err(|why| AssetMediaFactsUnreadable::because(format!("{why_prefix}: {why}")))
    };
    let (pixel_width, pixel_height) = opened("the image could not be opened")?
        .into_dimensions()
        .map_err(|why| {
            AssetMediaFactsUnreadable::because(format!("the image's header did not read: {why}"))
        })?;
    let preview_picture = match opened("the image could not be opened")?.decode() {
        Ok(decoded) => preview_picture_of(&decoded),
        Err(why) => {
            tracing::debug!(path = %image_file_path.display(), %why, "an image's size read and its picture did not decode; it has no preview");
            None
        }
    };
    Ok(AssetMediaFactsReadFromTheFile {
        media_facts: AssetMediaFactsOnTheWire {
            image: Some(AssetImageFactsOnTheWire {
                pixel_width,
                pixel_height,
            }),
            ..AssetMediaFactsOnTheWire::default()
        },
        preview_picture,
    })
}

/// `picture` scaled to fit [`ASSET_PREVIEW_PICTURE_LONGEST_EDGE_PIXELS`], never enlarged, as PNG.
pub(crate) fn preview_picture_of(
    picture: &image::DynamicImage,
) -> Option<AssetPreviewPictureMadeFromTheFile> {
    let fits = picture.width() <= ASSET_PREVIEW_PICTURE_LONGEST_EDGE_PIXELS
        && picture.height() <= ASSET_PREVIEW_PICTURE_LONGEST_EDGE_PIXELS;
    let scaled = if fits {
        picture.to_rgba8()
    } else {
        picture
            .thumbnail(
                ASSET_PREVIEW_PICTURE_LONGEST_EDGE_PIXELS,
                ASSET_PREVIEW_PICTURE_LONGEST_EDGE_PIXELS,
            )
            .to_rgba8()
    };
    let mut png_bytes = Cursor::new(Vec::new());
    if let Err(why) = scaled.write_to(&mut png_bytes, ImageFormat::Png) {
        tracing::debug!(%why, "a preview picture did not encode as PNG");
        return None;
    }
    Some(AssetPreviewPictureMadeFromTheFile {
        pixel_width: scaled.width(),
        pixel_height: scaled.height(),
        png_bytes: png_bytes.into_inner(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AssetMediaFactsReadersOfThisRuntime, read_the_media_facts_of_an_asset_file};

    fn a_png_of(directory: &Path, file_name: &str, width: u32, height: u32) -> std::path::PathBuf {
        let path = directory.join(file_name);
        image::RgbImage::from_pixel(width, height, image::Rgb([200, 40, 10]))
            .save_with_format(&path, ImageFormat::Png)
            .unwrap();
        path
    }

    #[test]
    fn an_image_reads_its_size_and_a_preview_scaled_to_fit_its_longest_edge() {
        let directory = tempfile::tempdir().unwrap();
        let read = read_the_media_facts_of_an_asset_file(
            &a_png_of(directory.path(), "wide.png", 2400, 1200),
            &AssetMediaFactsReadersOfThisRuntime::default(),
        )
        .unwrap();
        assert_eq!(
            read.media_facts.image,
            Some(AssetImageFactsOnTheWire {
                pixel_width: 2400,
                pixel_height: 1200
            })
        );
        let preview = read.preview_picture.unwrap();
        assert_eq!((preview.pixel_width, preview.pixel_height), (512, 256));
        let decoded = image::load_from_memory_with_format(&preview.png_bytes, ImageFormat::Png)
            .unwrap()
            .to_rgb8();
        assert_eq!(decoded.get_pixel(10, 10), &image::Rgb([200, 40, 10]));
    }

    #[test]
    fn a_small_image_keeps_its_size_in_the_preview() {
        let directory = tempfile::tempdir().unwrap();
        let read = read_the_media_facts_of_an_asset_file(
            &a_png_of(directory.path(), "icon.png", 64, 32),
            &AssetMediaFactsReadersOfThisRuntime::default(),
        )
        .unwrap();
        let preview = read.preview_picture.unwrap();
        assert_eq!((preview.pixel_width, preview.pixel_height), (64, 32));
    }

    #[test]
    fn a_file_that_is_no_image_is_unreadable() {
        let directory = tempfile::tempdir().unwrap();
        let not_an_image = directory.path().join("cover.png");
        std::fs::write(&not_an_image, b"plain text, not a PNG").unwrap();
        assert!(
            read_the_media_facts_of_an_asset_file(
                &not_an_image,
                &AssetMediaFactsReadersOfThisRuntime::default()
            )
            .is_err()
        );
    }
}
