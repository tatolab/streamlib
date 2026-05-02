// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! NV12 → BGRA / RGBA pixel-format conversion via [`VulkanComputeKernel`].
//!
//! Thin wrapper that owns a compute kernel pre-configured for the
//! `nv12_to_bgra` shader and exposes a `convert(source, dest)` method.

use std::sync::Arc;

use crate::core::rhi::{
    ComputeBindingSpec, ComputeKernelDescriptor, PixelFormat, RhiPixelBuffer,
};
use crate::core::{Result, StreamError};

use super::{VulkanComputeKernel, HostVulkanDevice};

/// Push-constants struct matching the `nv12_to_bgra` compute shader's
/// `layout(push_constant)` block (width, height, flags). Flags encode:
///   bit 0 — destination is BGRA (else RGBA)
///   bit 1 — full-range YUV input (else video range)
const NV12_TO_BGRA_PUSH_CONSTANT_SIZE: u32 = 12;

const NV12_TO_BGRA_BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::storage_buffer(0), // NV12 input
    ComputeBindingSpec::storage_buffer(1), // BGRA output
];

/// Workgroup size: shader processes 1 pixel per thread, 16×16 workgroups.
const NV12_TO_BGRA_WORKGROUP_SIZE: u32 = 16;

/// NV12 → RGBA/BGRA format converter.
pub struct VulkanFormatConverter {
    kernel: VulkanComputeKernel,
    source_bytes_per_pixel: u32,
    dest_bytes_per_pixel: u32,
}

impl VulkanFormatConverter {
    pub fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        source_bytes_per_pixel: u32,
        dest_bytes_per_pixel: u32,
    ) -> Result<Self> {
        let spv = include_bytes!(concat!(env!("OUT_DIR"), "/nv12_to_bgra.spv"));
        let kernel = VulkanComputeKernel::new(
            vulkan_device,
            &ComputeKernelDescriptor {
                label: "nv12_to_bgra",
                spv,
                bindings: NV12_TO_BGRA_BINDINGS,
                push_constant_size: NV12_TO_BGRA_PUSH_CONSTANT_SIZE,
            },
        )?;
        Ok(Self {
            kernel,
            source_bytes_per_pixel,
            dest_bytes_per_pixel,
        })
    }

    /// Convert NV12 → RGBA/BGRA. Source and destination must have the same
    /// dimensions; destination format determines the BGRA-vs-RGBA flag.
    pub fn convert(&self, source: &RhiPixelBuffer, dest: &RhiPixelBuffer) -> Result<()> {
        let width = source.width;
        let height = source.height;
        if width != dest.width || height != dest.height {
            return Err(StreamError::GpuError(
                "Source and destination buffers must have the same dimensions".into(),
            ));
        }

        let src_format = source.buffer_ref().inner.format();
        let dst_format = dest.buffer_ref().inner.format();
        let flags = match (src_format, dst_format) {
            (
                PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange,
                PixelFormat::Rgba32 | PixelFormat::Bgra32,
            ) => {
                let is_bgra = matches!(dst_format, PixelFormat::Bgra32);
                let full_range = matches!(src_format, PixelFormat::Nv12FullRange);
                (is_bgra as u32) | ((full_range as u32) << 1)
            }
            _ => {
                return Err(StreamError::NotSupported(format!(
                    "Unsupported format conversion: {:?} → {:?}",
                    src_format, dst_format
                )));
            }
        };

        self.kernel.set_storage_buffer(0, source)?;
        self.kernel.set_storage_buffer(1, dest)?;
        let push_data: [u32; 3] = [width, height, flags];
        self.kernel.set_push_constants_value(&push_data)?;

        let dispatch_x = width.div_ceil(NV12_TO_BGRA_WORKGROUP_SIZE);
        let dispatch_y = height.div_ceil(NV12_TO_BGRA_WORKGROUP_SIZE);
        self.kernel.dispatch(dispatch_x, dispatch_y, 1)
    }

    pub fn source_bytes_per_pixel(&self) -> u32 {
        self.source_bytes_per_pixel
    }

    pub fn dest_bytes_per_pixel(&self) -> u32 {
        self.dest_bytes_per_pixel
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rhi::RhiPixelBufferRef;
    use crate::core::StreamError;
    use crate::vulkan::rhi::HostVulkanPixelBuffer;

    fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
        match HostVulkanDevice::new() {
            Ok(d) => Some(d),
            Err(_) => {
                println!("Skipping - no Vulkan device available");
                None
            }
        }
    }

    fn make_pixel_buffer(
        device: &Arc<HostVulkanDevice>,
        width: u32,
        height: u32,
        bytes_per_pixel: u32,
        format: PixelFormat,
    ) -> RhiPixelBuffer {
        let vk_buf = HostVulkanPixelBuffer::new(device, width, height, bytes_per_pixel, format)
            .expect("Failed to create pixel buffer");
        let ref_ = RhiPixelBufferRef {
            inner: Arc::new(vk_buf),
        };
        RhiPixelBuffer::new(ref_)
    }

    /// CPU reference for the GLSL `nv12_to_bgra.comp` shader, full-range path.
    /// Returns a `Vec<u32>` of length `width * height` packed B|G<<8|R<<16|0xFF<<24
    /// (matching the shader's output for `is_bgra=true, full_range=true`).
    fn cpu_reference_nv12_full_to_bgra(
        nv12: &[u8],
        width: u32,
        height: u32,
    ) -> Vec<u32> {
        let mut out = vec![0u32; (width * height) as usize];
        for y in 0..height {
            for x in 0..width {
                let y_val = nv12[(y * width + x) as usize] as f32;
                let uv_offset = width * height + (y >> 1) * width + (x & !1);
                let u_val = nv12[uv_offset as usize] as f32;
                let v_val = nv12[uv_offset as usize + 1] as f32;
                let cb = u_val - 128.0;
                let cr = v_val - 128.0;
                let r = (y_val + 1.402 * cr).clamp(0.0, 255.0) as u32;
                let g = (y_val - 0.344136 * cb - 0.714136 * cr).clamp(0.0, 255.0) as u32;
                let b = (y_val + 1.772 * cb).clamp(0.0, 255.0) as u32;
                out[(y * width + x) as usize] = b | (g << 8) | (r << 16) | (255 << 24);
            }
        }
        out
    }

    #[test]
    fn nv12_full_range_to_bgra_matches_cpu_reference() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let width = 64u32;
        let height = 32u32;

        // Vary Y across the frame, set U and V to a non-neutral value so
        // chroma actually contributes to the output (catches binding swaps,
        // wrong plane offsets, etc.).
        let nv12_size = (width * height + width * height / 2) as usize;
        let mut nv12_input: Vec<u8> = vec![0u8; nv12_size];
        for y in 0..height {
            for x in 0..width {
                nv12_input[(y * width + x) as usize] = ((x + y) % 256) as u8;
            }
        }
        // UV plane: U=180, V=70 over the entire frame
        let uv_offset = (width * height) as usize;
        let mut idx = uv_offset;
        while idx + 1 < nv12_size {
            nv12_input[idx] = 180;
            nv12_input[idx + 1] = 70;
            idx += 2;
        }

        // Expected output computed entirely on the CPU.
        let expected = cpu_reference_nv12_full_to_bgra(&nv12_input, width, height);

        let nv12_buf = make_pixel_buffer(&device, width, height, 2, PixelFormat::Nv12FullRange);
        let bgra_buf = make_pixel_buffer(&device, width, height, 4, PixelFormat::Bgra32);

        unsafe {
            std::ptr::copy_nonoverlapping(
                nv12_input.as_ptr(),
                nv12_buf.buffer_ref().inner.mapped_ptr(),
                nv12_size,
            );
        }

        let converter = VulkanFormatConverter::new(&device, 2, 4)
            .expect("VulkanFormatConverter::new must succeed");
        converter
            .convert(&nv12_buf, &bgra_buf)
            .expect("convert must succeed");

        let mut actual = vec![0u32; (width * height) as usize];
        unsafe {
            std::ptr::copy_nonoverlapping(
                bgra_buf.buffer_ref().inner.mapped_ptr() as *const u32,
                actual.as_mut_ptr(),
                actual.len(),
            );
        }

        // GLSL float math and Rust f32 produce bit-identical results for these
        // inputs on every GPU we've tested, but allow ±1 per channel just in
        // case a future vendor diverges in rounding mode.
        assert_eq!(actual.len(), expected.len());
        for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
            let diff = |shift: u32| {
                ((*a >> shift) & 0xFF) as i32 - ((*e >> shift) & 0xFF) as i32
            };
            let dr = diff(16).abs();
            let dg = diff(8).abs();
            let db = diff(0).abs();
            assert!(
                dr <= 1 && dg <= 1 && db <= 1,
                "pixel {i} mismatch: got {a:08x}, expected {e:08x}"
            );
        }
    }

    #[test]
    fn convert_rejects_size_mismatch() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let nv12 = make_pixel_buffer(&device, 64, 32, 2, PixelFormat::Nv12FullRange);
        let bgra = make_pixel_buffer(&device, 32, 32, 4, PixelFormat::Bgra32);
        let converter =
            VulkanFormatConverter::new(&device, 2, 4).expect("converter creation");
        let err = converter.convert(&nv12, &bgra).err().expect("expected error");
        assert!(matches!(err, StreamError::GpuError(_)));
    }

    #[test]
    fn convert_rejects_unsupported_formats() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let src = make_pixel_buffer(&device, 64, 32, 4, PixelFormat::Bgra32);
        let dst = make_pixel_buffer(&device, 64, 32, 4, PixelFormat::Bgra32);
        let converter =
            VulkanFormatConverter::new(&device, 4, 4).expect("converter creation");
        let err = converter.convert(&src, &dst).err().expect("expected error");
        assert!(matches!(err, StreamError::NotSupported(_)));
    }

    #[test]
    fn test_new_creates_compute_pipeline_successfully() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        // Validates that the migrated converter still constructs cleanly through
        // the new compute-kernel abstraction (SPIR-V reflection + descriptor
        // layout from declared bindings + pipeline + descriptor pool).
        let result = VulkanFormatConverter::new(&device, 2, 4);
        assert!(
            result.is_ok(),
            "VulkanFormatConverter::new must succeed: {:?}",
            result.err()
        );
    }

    // -- PNG fixture regression lock ----------------------------------------------
    //
    // The fixture is a snapshot of the shader's NV12-full-range → BGRA output for
    // a deterministic input. Any future change to the shader (or to the dispatch
    // path) that alters output bytes will fail this test, forcing a deliberate
    // regeneration via `cargo test ... -- --ignored regenerate_nv12_to_bgra_fixture`.

    const FIXTURE_WIDTH: u32 = 64;
    const FIXTURE_HEIGHT: u32 = 32;
    const FIXTURE_INPUT_PATH: &str =
        "tests/fixtures/nv12_to_bgra/input_nv12_full_range_64x32.raw";
    const FIXTURE_EXPECTED_PATH: &str =
        "tests/fixtures/nv12_to_bgra/expected_bgra_64x32.png";

    /// Deterministic NV12 input: Y varies across the frame, U and V vary by
    /// position so chroma actually contributes (catches plane-offset bugs and
    /// binding swaps).
    fn build_fixture_nv12_input() -> Vec<u8> {
        let w = FIXTURE_WIDTH;
        let h = FIXTURE_HEIGHT;
        let mut buf = vec![0u8; (w * h + w * h / 2) as usize];
        for y in 0..h {
            for x in 0..w {
                buf[(y * w + x) as usize] = ((x * 4 + y * 8) % 256) as u8;
            }
        }
        let uv_offset = (w * h) as usize;
        let half_h = h / 2;
        for y in 0..half_h {
            for x in (0..w).step_by(2) {
                let i = uv_offset + (y * w + x) as usize;
                buf[i] = ((x * 8) % 256) as u8;
                buf[i + 1] = ((y * 16 + 64) % 256) as u8;
            }
        }
        buf
    }

    fn run_shader_against_fixture_input(
        device: &Arc<HostVulkanDevice>,
        nv12_bytes: &[u8],
    ) -> Vec<u8> {
        let nv12_buf = make_pixel_buffer(
            device,
            FIXTURE_WIDTH,
            FIXTURE_HEIGHT,
            2,
            PixelFormat::Nv12FullRange,
        );
        let bgra_buf = make_pixel_buffer(
            device,
            FIXTURE_WIDTH,
            FIXTURE_HEIGHT,
            4,
            PixelFormat::Bgra32,
        );
        unsafe {
            std::ptr::copy_nonoverlapping(
                nv12_bytes.as_ptr(),
                nv12_buf.buffer_ref().inner.mapped_ptr(),
                nv12_bytes.len(),
            );
        }
        let converter = VulkanFormatConverter::new(device, 2, 4)
            .expect("VulkanFormatConverter::new must succeed");
        converter
            .convert(&nv12_buf, &bgra_buf)
            .expect("convert must succeed");
        let bgra_size = (FIXTURE_WIDTH * FIXTURE_HEIGHT * 4) as usize;
        let mut bgra_out = vec![0u8; bgra_size];
        unsafe {
            std::ptr::copy_nonoverlapping(
                bgra_buf.buffer_ref().inner.mapped_ptr(),
                bgra_out.as_mut_ptr(),
                bgra_size,
            );
        }
        bgra_out
    }

    /// PNG decode helper — returns RGBA8 bytes in row-major order.
    fn read_png_rgba8(path: &str) -> (u32, u32, Vec<u8>) {
        let file = std::fs::File::open(path)
            .unwrap_or_else(|e| panic!("failed to open fixture {path}: {e}"));
        let decoder = png::Decoder::new(file);
        let mut reader = decoder.read_info().expect("PNG decode_info");
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf).expect("PNG next_frame");
        let bytes = buf[..info.buffer_size()].to_vec();
        // The fixture is written as RGBA8; assert the encoded format matches
        // what we expect on read so a future PNG library version doesn't
        // silently change the channel order.
        assert_eq!(info.color_type, png::ColorType::Rgba);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);
        (info.width, info.height, bytes)
    }

    /// Convert BGRA8 row-major bytes to RGBA8 row-major bytes (used both when
    /// writing the fixture and when comparing on read — the PNG file stores
    /// RGBA which is what every PNG viewer expects).
    fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
        let mut rgba = vec![0u8; bgra.len()];
        for chunk in 0..bgra.len() / 4 {
            let i = chunk * 4;
            rgba[i] = bgra[i + 2]; // R = BGRA[2]
            rgba[i + 1] = bgra[i + 1]; // G = BGRA[1]
            rgba[i + 2] = bgra[i]; // B = BGRA[0]
            rgba[i + 3] = bgra[i + 3]; // A
        }
        rgba
    }

    #[test]
    fn nv12_to_bgra_matches_committed_png_fixture() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let nv12_input = build_fixture_nv12_input();
        let expected_input = std::fs::read(FIXTURE_INPUT_PATH)
            .unwrap_or_else(|e| panic!("failed to read fixture input {FIXTURE_INPUT_PATH}: {e}"));
        assert_eq!(
            nv12_input, expected_input,
            "fixture input bytes drifted from the deterministic generator — regenerate the fixture"
        );

        let actual_bgra = run_shader_against_fixture_input(&device, &nv12_input);
        let actual_rgba = bgra_to_rgba(&actual_bgra);

        let (png_w, png_h, expected_rgba) = read_png_rgba8(FIXTURE_EXPECTED_PATH);
        assert_eq!(png_w, FIXTURE_WIDTH);
        assert_eq!(png_h, FIXTURE_HEIGHT);

        // Bit-exact comparison: any drift in shader math, descriptor binding,
        // push-constant layout, or plane offsets fails this test. Regenerate
        // deliberately via `--ignored regenerate_nv12_to_bgra_fixture`.
        assert_eq!(
            actual_rgba.len(),
            expected_rgba.len(),
            "shader output length differs from PNG fixture"
        );
        for i in (0..actual_rgba.len()).step_by(4) {
            let a = &actual_rgba[i..i + 4];
            let e = &expected_rgba[i..i + 4];
            assert_eq!(
                a, e,
                "pixel {} (RGBA) drift: got {:?}, expected {:?}",
                i / 4,
                a,
                e
            );
        }
    }

    /// Regenerate the NV12 input + expected BGRA PNG fixture. Run with:
    ///
    /// ```text
    /// cargo test -p streamlib --lib vulkan::rhi::vulkan_format_converter \
    ///     -- --ignored regenerate_nv12_to_bgra_fixture --nocapture
    /// ```
    ///
    /// The fixture path is relative to `libs/streamlib/`. After running, commit
    /// the regenerated `tests/fixtures/nv12_to_bgra/*` files.
    #[test]
    #[ignore = "regenerate fixture — run explicitly with --ignored"]
    fn regenerate_nv12_to_bgra_fixture() {
        let device = try_vulkan_device().expect("Vulkan device required to regenerate fixture");
        let nv12_input = build_fixture_nv12_input();
        std::fs::create_dir_all("tests/fixtures/nv12_to_bgra")
            .expect("create fixture dir");
        std::fs::write(FIXTURE_INPUT_PATH, &nv12_input).expect("write input fixture");

        let actual_bgra = run_shader_against_fixture_input(&device, &nv12_input);
        let rgba = bgra_to_rgba(&actual_bgra);

        let png_file = std::fs::File::create(FIXTURE_EXPECTED_PATH)
            .expect("create expected PNG fixture");
        let bw = std::io::BufWriter::new(png_file);
        let mut encoder = png::Encoder::new(bw, FIXTURE_WIDTH, FIXTURE_HEIGHT);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header");
        writer.write_image_data(&rgba).expect("PNG image data");
        eprintln!(
            "Regenerated fixtures:\n  {FIXTURE_INPUT_PATH}\n  {FIXTURE_EXPECTED_PATH}"
        );
    }
}
