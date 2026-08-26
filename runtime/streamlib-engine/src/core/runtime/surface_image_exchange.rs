// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The control plane's `exchange` verb: a published surface id in, that
//! frame's pixels out as PNG bytes.
//!
//! Composed from doors that already ship — the surface's own resolve, the
//! pool's own claim, the RHI's own conversion and readback — so the caller
//! needs no Vulkan device, no surface socket, and no link into the graph.
//! Encoding happens here, *after* the claim has been released by the copy
//! below it: an encoder's cost can never extend the window a producer is
//! kept out of its own slot.
//!
//! The engine still inspects no bag content. Composition is entirely the
//! consumer's: it taps a channel, decodes the bag itself, reads whatever
//! field it knows carries a surface id, and calls this with the id.

use crate::core::error::{Error, Result};

/// What one exchange hands back: the frame as a PNG, plus the extent it
/// was encoded at and the extent the surface itself carries.
///
/// The two extents differ exactly when a downscale cap applied, which is
/// what lets a caller state the true resolution alongside a reduced image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExchangedPublishedSurfaceFramePngImage {
    /// The frame encoded as a lossless RGBA8 PNG.
    pub png_image_bytes: Vec<u8>,
    /// Width of the encoded image.
    pub encoded_image_pixel_width: u32,
    /// Height of the encoded image.
    pub encoded_image_pixel_height: u32,
    /// Width the surface's own backing carries.
    pub source_surface_pixel_width: u32,
    /// Height the surface's own backing carries.
    pub source_surface_pixel_height: u32,
}

/// Encode tightly-packed RGBA8 pixels as PNG bytes.
///
/// Lossless and un-inflated: the exact pixels the GPU copy produced are
/// what a caller writes to disk or measures PSNR against.
pub(crate) fn encode_rgba8_pixels_as_png_image_bytes(
    rgba8_pixel_bytes: &[u8],
    image_pixel_width: u32,
    image_pixel_height: u32,
) -> Result<Vec<u8>> {
    let expected_byte_count =
        u64::from(image_pixel_width) * u64::from(image_pixel_height) * PNG_RGBA8_BYTES_PER_PIXEL;
    if rgba8_pixel_bytes.len() as u64 != expected_byte_count {
        return Err(Error::Runtime(format!(
            "PNG encode was handed {} bytes for a {image_pixel_width}x{image_pixel_height} RGBA8 \
             image that needs {expected_byte_count}",
            rgba8_pixel_bytes.len()
        )));
    }

    let mut png_image_bytes = Vec::new();
    let mut encoder = png::Encoder::new(
        &mut png_image_bytes,
        image_pixel_width,
        image_pixel_height,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(png_encode_failure)?;
    writer
        .write_image_data(rgba8_pixel_bytes)
        .map_err(png_encode_failure)?;
    writer.finish().map_err(png_encode_failure)?;
    Ok(png_image_bytes)
}

/// Bytes per pixel of the RGBA8 the encoder is handed.
const PNG_RGBA8_BYTES_PER_PIXEL: u64 = 4;

fn png_encode_failure(failure: png::EncodingError) -> Error {
    Error::Runtime(format!("PNG encode of the exchanged frame failed: {failure}"))
}

/// Claim `published_surface_id`'s frame, copy it to the host under the
/// claim, release, then encode.
///
/// The order is the contract, not an implementation detail: see the module
/// docs and `docs/decisions/control-plane-pixel-exchange.md`.
#[cfg(target_os = "linux")]
pub(crate) fn exchange_published_surface_id_for_png_image_bytes(
    gpu: &crate::core::context::GpuContext,
    published_surface_id: &str,
    downscale_long_edge_pixel_cap: Option<u32>,
) -> Result<ExchangedPublishedSurfaceFramePngImage> {
    let host_image = gpu.copy_published_surface_frame_to_host_rgba8_image(
        published_surface_id,
        downscale_long_edge_pixel_cap,
    )?;
    let png_image_bytes = encode_rgba8_pixels_as_png_image_bytes(
        &host_image.rgba8_pixel_bytes,
        host_image.image_pixel_width,
        host_image.image_pixel_height,
    )?;
    Ok(ExchangedPublishedSurfaceFramePngImage {
        png_image_bytes,
        encoded_image_pixel_width: host_image.image_pixel_width,
        encoded_image_pixel_height: host_image.image_pixel_height,
        source_surface_pixel_width: host_image.source_surface_pixel_width,
        source_surface_pixel_height: host_image.source_surface_pixel_height,
    })
}

/// Every conversion the exchange performs is a Vulkan RHI primitive —
/// the color converter, the display blit, the texture readback — so on a
/// backend that carries none of them the verb refuses rather than
/// half-answering.
#[cfg(not(target_os = "linux"))]
pub(crate) fn exchange_published_surface_id_for_png_image_bytes(
    _gpu: &crate::core::context::GpuContext,
    published_surface_id: &str,
    _downscale_long_edge_pixel_cap: Option<u32>,
) -> Result<ExchangedPublishedSurfaceFramePngImage> {
    Err(Error::NotSupported(format!(
        "exchanging surface '{published_surface_id}' for image bytes needs the Vulkan RHI's \
         color converter, blit and readback, which this platform's backend does not carry"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba8_gradient(width: u32, height: u32) -> Vec<u8> {
        (0..width * height)
            .flat_map(|pixel| {
                let x = (pixel % width) as u8;
                let y = (pixel / width) as u8;
                [x, y, x ^ y, 0xFF]
            })
            .collect()
    }

    /// The encode is lossless: what a caller decodes is byte-for-byte the
    /// pixels the GPU copy produced. PSNR against a reference is only
    /// meaningful if this holds.
    #[test]
    fn the_encoded_png_decodes_back_to_the_exact_rgba8_it_was_given() {
        let (width, height) = (23u32, 17u32);
        let pixels = rgba8_gradient(width, height);

        let encoded = encode_rgba8_pixels_as_png_image_bytes(&pixels, width, height)
            .expect("a well-formed RGBA8 image encodes");

        let decoder = png::Decoder::new(std::io::Cursor::new(&encoded));
        let mut reader = decoder.read_info().expect("the encoder emits a valid PNG");
        assert_eq!(reader.info().width, width);
        assert_eq!(reader.info().height, height);
        assert_eq!(reader.info().color_type, png::ColorType::Rgba);
        assert_eq!(reader.info().bit_depth, png::BitDepth::Eight);

        let mut decoded = vec![0u8; reader.output_buffer_size()];
        let frame = reader.next_frame(&mut decoded).expect("one frame decodes");
        decoded.truncate(frame.buffer_size());
        assert_eq!(decoded, pixels);
    }

    /// A byte count that disagrees with the extent is a copy that went
    /// wrong upstream; encoding it anyway would emit a plausible-looking
    /// PNG of shifted rows.
    #[test]
    fn a_byte_count_that_disagrees_with_the_extent_is_refused_naming_both() {
        let failure = encode_rgba8_pixels_as_png_image_bytes(&[0u8; 16], 4, 4)
            .expect_err("16 bytes cannot be a 4x4 RGBA8 image");
        let message = failure.to_string();
        assert!(message.contains("16"), "{message}");
        assert!(message.contains("64"), "{message}");
    }
}
