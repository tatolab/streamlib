// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The control plane's `exchange` verb: a published surface id in, that
//! frame's pixels out as PNG bytes.
//!
//! Encoding lives here rather than beside the copy because it must run
//! *after* the claim has been released, so an encoder's cost can never
//! extend the window a producer is kept out of its own slot. Why the verb
//! has this shape at all: `docs/decisions/control-plane-pixel-exchange.md`.

use crate::core::error::{Error, Result};

/// What one exchange hands back: the frame as a PNG, plus the extent it
/// was encoded at and the extent the surface itself carries.
///
/// The two extents differ exactly when a downscale cap applied, which is
/// what lets a caller state the true resolution alongside a reduced image.
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

/// Reports the image's shape and the size of its bytes, never the bytes:
/// a derived `Debug` would spill a whole frame into any panic message or
/// `tracing` field that touched one.
impl std::fmt::Debug for ExchangedPublishedSurfaceFramePngImage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExchangedPublishedSurfaceFramePngImage")
            .field(
                "png_image_bytes",
                &format_args!("<{} bytes>", self.png_image_bytes.len()),
            )
            .field("encoded_image_pixel_width", &self.encoded_image_pixel_width)
            .field(
                "encoded_image_pixel_height",
                &self.encoded_image_pixel_height,
            )
            .field(
                "source_surface_pixel_width",
                &self.source_surface_pixel_width,
            )
            .field(
                "source_surface_pixel_height",
                &self.source_surface_pixel_height,
            )
            .finish()
    }
}

/// Encode tightly-packed RGBA8 pixels as PNG bytes.
///
/// Lossless and un-inflated: the exact pixels the GPU copy produced are
/// what a caller writes to disk or measures PSNR against.
#[cfg(any(target_os = "linux", test))]
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
    let mut encoder =
        png::Encoder::new(&mut png_image_bytes, image_pixel_width, image_pixel_height);
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
#[cfg(any(target_os = "linux", test))]
const PNG_RGBA8_BYTES_PER_PIXEL: u64 = 4;

#[cfg(any(target_os = "linux", test))]
fn png_encode_failure(failure: png::EncodingError) -> Error {
    Error::Runtime(format!(
        "PNG encode of the exchanged frame failed: {failure}"
    ))
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

    // ------------------------------------------------------------------
    // The operation as a caller reaches it: through `RuntimeOperations`,
    // on a real `Runner`. Without these the whole production path — the
    // `Runner` impl, the blocking hop, and the composed
    // claim/copy/release/encode — is unlocked: replacing its body with an
    // error leaves every other test in this branch green.
    // ------------------------------------------------------------------

    use crate::core::runtime::{Runner, RuntimeOperations};

    /// Runs the operation's future to completion on a runtime of the
    /// test's own, so the blocking hop the `Runner` impl makes has a
    /// blocking pool to land on.
    fn awaiting_the_exchange(
        runner: &Runner,
        published_surface_id: &str,
        downscale_long_edge_pixel_cap: Option<u32>,
    ) -> Result<ExchangedPublishedSurfaceFramePngImage> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a test tokio runtime")
            .block_on(
                runner.exchange_published_surface_id_for_png_image_bytes_async(
                    published_surface_id.to_string(),
                    downscale_long_edge_pixel_cap,
                ),
            )
    }

    /// A node that has not started owns no pool and no device, so the
    /// operation says that rather than failing somewhere inside a resolve.
    /// Needs no GPU.
    #[test]
    #[serial_test::serial]
    fn exchanging_against_a_runtime_that_never_started_names_the_missing_context() {
        let runner = Runner::new().expect("a runner boots without a graph");
        let Err(refusal) = awaiting_the_exchange(&runner, "any-surface", None) else {
            panic!("a runtime with no GPU context has no frame to hand back");
        };
        let reported = refusal.to_string();
        assert!(reported.contains("start the runtime"), "{reported}");
    }

    /// The whole path a `curl` drives, minus the HTTP hop: pool frame in,
    /// decodable PNG of exactly those pixels out.
    /// GPU-gated: skips when no device is present.
    #[test]
    #[serial_test::serial]
    fn a_published_pool_frame_exchanges_through_the_runtime_operation_for_its_own_pixels() {
        const FRAME_PIXEL_WIDTH: u32 = 64;
        const FRAME_PIXEL_HEIGHT: u32 = 32;
        const PUBLISHED_RGBA8_PIXEL: [u8; 4] = [0x0D, 0x7A, 0xC4, 0xFF];

        let runner = Runner::new().expect("a runner boots without a graph");
        if runner.start().is_err() {
            println!("Skipping - the runtime could not start (no GPU device available)");
            return;
        }

        let gpu_context = runner
            .runtime_context
            .lock()
            .as_ref()
            .map(|runtime_context| runtime_context.gpu.clone())
            .expect("a started runtime carries its GPU context");

        let (published_frame_id, pooled_backing) = gpu_context
            .acquire_pixel_buffer(
                FRAME_PIXEL_WIDTH,
                FRAME_PIXEL_HEIGHT,
                crate::core::rhi::PixelFormat::Rgba32,
            )
            .expect("acquire the frame's pooled backing");
        let base_address = pooled_backing.plane_base_address(0);
        assert!(
            !base_address.is_null(),
            "the pooled allocation must be mapped"
        );
        let backing = unsafe {
            std::slice::from_raw_parts_mut(base_address, pooled_backing.plane_size(0) as usize)
        };
        for (index, byte) in backing.iter_mut().enumerate() {
            *byte = PUBLISHED_RGBA8_PIXEL[index % PUBLISHED_RGBA8_PIXEL.len()];
        }

        let exchanged = awaiting_the_exchange(&runner, &published_frame_id.to_string(), None)
            .expect("the published frame exchanges for an image");
        runner.stop().expect("the runtime stops");

        assert_eq!(
            (
                exchanged.encoded_image_pixel_width,
                exchanged.encoded_image_pixel_height
            ),
            (FRAME_PIXEL_WIDTH, FRAME_PIXEL_HEIGHT)
        );
        assert_eq!(
            (
                exchanged.source_surface_pixel_width,
                exchanged.source_surface_pixel_height
            ),
            (FRAME_PIXEL_WIDTH, FRAME_PIXEL_HEIGHT)
        );

        // Decoded, not merely well-formed: a PNG header alone would pass
        // over a frame of zeroes, which is exactly the failure a live run
        // with clean logs hides.
        let decoder = png::Decoder::new(std::io::Cursor::new(&exchanged.png_image_bytes));
        let mut reader = decoder.read_info().expect("the answer is a valid PNG");
        assert_eq!(reader.info().width, FRAME_PIXEL_WIDTH);
        assert_eq!(reader.info().height, FRAME_PIXEL_HEIGHT);
        let mut decoded = vec![0u8; reader.output_buffer_size()];
        let frame = reader.next_frame(&mut decoded).expect("one frame decodes");
        decoded.truncate(frame.buffer_size());
        for (pixel_index, pixel) in decoded.chunks_exact(4).enumerate() {
            assert_eq!(
                pixel, PUBLISHED_RGBA8_PIXEL,
                "pixel {pixel_index} of the exchanged image is not what the pool published"
            );
        }
    }
}
