// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! 80s CRT + film-grain post-effect via [`VulkanComputeKernel`].
//!
//! Linux counterpart to the macOS Metal kernel at
//! `examples/camera-python-display/src/shaders/crt_film_grain.metal`.

use std::sync::Arc;

use crate::core::rhi::{ComputeBindingSpec, ComputeKernelDescriptor, RhiPixelBuffer};
use crate::core::{Result, StreamError};

use super::{HostVulkanDevice, VulkanComputeKernel};

/// Push-constants layout matching `crt_film_grain.comp`'s
/// `layout(push_constant)` block.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CrtFilmGrainPushConstants {
    pub width: u32,
    pub height: u32,
    pub time: f32,
    pub crt_curve: f32,
    pub scanline_intensity: f32,
    pub chromatic_aberration: f32,
    pub grain_intensity: f32,
    pub grain_speed: f32,
    pub vignette_intensity: f32,
    pub brightness: f32,
}

const CRT_FILM_GRAIN_BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::storage_buffer(0), // input
    ComputeBindingSpec::storage_buffer(1), // output
];

const WORKGROUP_SIZE: u32 = 16;

/// Inputs for one CRT/film-grain dispatch. Both buffers are BGRA8 packed
/// little-endian and must share the same dimensions — the kernel writes
/// the output 1:1 with the input grid.
pub struct CrtFilmGrainInputs<'a> {
    pub input: &'a RhiPixelBuffer,
    pub output: &'a RhiPixelBuffer,
    pub time_seconds: f32,
    pub crt_curve: f32,
    pub scanline_intensity: f32,
    pub chromatic_aberration: f32,
    pub grain_intensity: f32,
    pub grain_speed: f32,
    pub vignette_intensity: f32,
    pub brightness: f32,
}

/// CRT + film-grain post-effect compute kernel.
pub struct VulkanCrtFilmGrain {
    kernel: VulkanComputeKernel,
}

impl VulkanCrtFilmGrain {
    pub fn new(vulkan_device: &Arc<HostVulkanDevice>) -> Result<Self> {
        let spv = include_bytes!(concat!(env!("OUT_DIR"), "/crt_film_grain.spv"));
        let kernel = VulkanComputeKernel::new(
            vulkan_device,
            &ComputeKernelDescriptor {
                label: "crt_film_grain",
                spv,
                bindings: CRT_FILM_GRAIN_BINDINGS,
                push_constant_size: std::mem::size_of::<CrtFilmGrainPushConstants>() as u32,
            },
        )?;
        Ok(Self { kernel })
    }

    /// Apply the CRT/film-grain effect from `inputs.input` into
    /// `inputs.output`. The output's dimensions drive the dispatch grid;
    /// input must match the output 1:1 (the shader assumes a shared grid
    /// for its UV sampling and per-pixel composite).
    pub fn dispatch(&self, inputs: CrtFilmGrainInputs<'_>) -> Result<()> {
        let width = inputs.output.width;
        let height = inputs.output.height;

        if inputs.input.width != width || inputs.input.height != height {
            return Err(StreamError::GpuError(format!(
                "CrtFilmGrain: input is {}×{}, expected {w}×{h} (must match output)",
                inputs.input.width,
                inputs.input.height,
                w = width,
                h = height,
            )));
        }

        let push = CrtFilmGrainPushConstants {
            width,
            height,
            time: inputs.time_seconds,
            crt_curve: inputs.crt_curve,
            scanline_intensity: inputs.scanline_intensity,
            chromatic_aberration: inputs.chromatic_aberration,
            grain_intensity: inputs.grain_intensity,
            grain_speed: inputs.grain_speed,
            vignette_intensity: inputs.vignette_intensity,
            brightness: inputs.brightness,
        };

        self.kernel.set_storage_buffer(0, inputs.input)?;
        self.kernel.set_storage_buffer(1, inputs.output)?;
        self.kernel.set_push_constants_value(&push)?;

        let dispatch_x = width.div_ceil(WORKGROUP_SIZE);
        let dispatch_y = height.div_ceil(WORKGROUP_SIZE);
        self.kernel.dispatch(dispatch_x, dispatch_y, 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::rhi::{PixelFormat, RhiPixelBufferRef};
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

    fn make_buf(device: &Arc<HostVulkanDevice>, w: u32, h: u32) -> RhiPixelBuffer {
        let vk = HostVulkanPixelBuffer::new(device, w, h, 4, PixelFormat::Bgra32)
            .expect("pixel buffer");
        RhiPixelBuffer::new(RhiPixelBufferRef {
            inner: Arc::new(vk),
        })
    }

    /// Default config matches the Metal `CrtFilmGrainConfig` in
    /// `examples/camera-python-display/src/crt_film_grain.rs`.
    fn default_inputs<'a>(input: &'a RhiPixelBuffer, output: &'a RhiPixelBuffer) -> CrtFilmGrainInputs<'a> {
        CrtFilmGrainInputs {
            input,
            output,
            time_seconds: 0.0,
            crt_curve: 0.7,
            scanline_intensity: 0.6,
            chromatic_aberration: 0.004,
            grain_intensity: 0.18,
            grain_speed: 1.0,
            vignette_intensity: 0.5,
            brightness: 2.2,
        }
    }

    fn read_pixel(buf: &RhiPixelBuffer, x: u32, y: u32) -> (u8, u8, u8, u8) {
        unsafe {
            let ptr = buf.buffer_ref().inner.mapped_ptr() as *const u32;
            let p = *ptr.add((y * buf.width + x) as usize);
            (
                (p & 0xFF) as u8,
                ((p >> 8) & 0xFF) as u8,
                ((p >> 16) & 0xFF) as u8,
                ((p >> 24) & 0xFF) as u8,
            )
        }
    }

    fn fill_solid(buf: &RhiPixelBuffer, b: u8, g: u8, r: u8, a: u8) {
        let pixel = (b as u32) | ((g as u32) << 8) | ((r as u32) << 16) | ((a as u32) << 24);
        let count = (buf.width * buf.height) as usize;
        unsafe {
            let ptr = buf.buffer_ref().inner.mapped_ptr() as *mut u32;
            for i in 0..count {
                *ptr.add(i) = pixel;
            }
        }
    }

    #[test]
    fn new_compiles_kernel() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let result = VulkanCrtFilmGrain::new(&device);
        assert!(result.is_ok(), "kernel creation must succeed: {:?}", result.err());
    }

    #[test]
    fn rejects_size_mismatch() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let kernel = VulkanCrtFilmGrain::new(&device).expect("kernel");
        let input = make_buf(&device, 32, 32);
        let output = make_buf(&device, 64, 32);
        let err = kernel
            .dispatch(default_inputs(&input, &output))
            .expect_err("size mismatch must error");
        assert!(matches!(err, StreamError::GpuError(_)));
    }

    /// Runs the kernel against a uniform mid-grey input. The center of the
    /// output (well inside the barrel-distorted screen rect) must be
    /// non-black after CRT processing, and the far corners must be black
    /// (outside the curved bounds → explicitly zeroed).
    #[test]
    fn solid_input_produces_curved_bounds() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let kernel = VulkanCrtFilmGrain::new(&device).expect("kernel");

        let w: u32 = 256;
        let h: u32 = 192;
        let input = make_buf(&device, w, h);
        let output = make_buf(&device, w, h);
        // Mid-grey opaque BGRA = (128, 128, 128, 255).
        fill_solid(&input, 128, 128, 128, 255);

        let mut inputs = default_inputs(&input, &output);
        // Disable grain so we can make deterministic assertions about the
        // center pixel; barrel curve stays in to test the bounds carve-out.
        inputs.grain_intensity = 0.0;
        kernel.dispatch(inputs).expect("dispatch");

        // Center should pass through CRT processing → non-zero output. The
        // S-curve / scanline / brightness chain can drop a channel below
        // mid-grey but the result should be non-black.
        let (cb, cg, cr, _ca) = read_pixel(&output, w / 2, h / 2);
        assert!(
            cb as u32 + cg as u32 + cr as u32 > 0,
            "center pixel must not be fully black after CRT pass: BGR=({cb},{cg},{cr})"
        );

        // Far corner should be outside the barrel-curved screen → black.
        let (b0, g0, r0, _a0) = read_pixel(&output, 0, 0);
        assert_eq!(
            (b0, g0, r0),
            (0, 0, 0),
            "top-left corner must be zeroed by outside-bounds carve-out"
        );
    }

    /// Visual smoke: feeds a checkerboard + bright square into the kernel,
    /// dispatches, and writes a PNG of the result for human review (PR
    /// embedding via `attach-images`). Mirrors the `visual_smoke_emits_png`
    /// shape used by `vulkan_blending_compositor`.
    #[test]
    fn visual_smoke_emits_png() {
        let device = match try_vulkan_device() { Some(d) => d, None => return };
        let kernel = VulkanCrtFilmGrain::new(&device).expect("kernel");

        let w: u32 = 480;
        let h: u32 = 320;
        let input = make_buf(&device, w, h);
        let output = make_buf(&device, w, h);

        // Compose a synthetic input: an 8×8 checkerboard with a bright
        // magenta square in the upper-left and a green diagonal so the
        // output's CRT processing is visually evaluable.
        unsafe {
            let ptr = input.buffer_ref().inner.mapped_ptr() as *mut u32;
            for y in 0..h {
                for x in 0..w {
                    let cell = ((x / 32) + (y / 32)) % 2 == 0;
                    let mut b = if cell { 200u32 } else { 60u32 };
                    let mut g = if cell { 200u32 } else { 60u32 };
                    let mut r = if cell { 200u32 } else { 60u32 };
                    // Bright magenta block for color separation visibility.
                    if x < 96 && y < 96 {
                        b = 255;
                        g = 0;
                        r = 255;
                    }
                    // Green diagonal stripe.
                    let on_diag = (x as i32 - y as i32).abs() < 8;
                    if on_diag {
                        b = 0;
                        g = 240;
                        r = 0;
                    }
                    let pixel = b | (g << 8) | (r << 16) | (255u32 << 24);
                    *ptr.add((y * w + x) as usize) = pixel;
                }
            }
        }

        let mut inputs = default_inputs(&input, &output);
        // Pick a non-zero animation phase so scanlines + grain are visible
        // in the rendered PNG; deterministic enough for inspection.
        inputs.time_seconds = 0.4;
        kernel.dispatch(inputs).expect("dispatch must succeed");

        let bgra_size = (w * h * 4) as usize;
        let mut bgra_bytes = vec![0u8; bgra_size];
        unsafe {
            std::ptr::copy_nonoverlapping(
                output.buffer_ref().inner.mapped_ptr(),
                bgra_bytes.as_mut_ptr(),
                bgra_size,
            );
        }
        let mut rgba = vec![0u8; bgra_size];
        for chunk in 0..(bgra_size / 4) {
            let i = chunk * 4;
            rgba[i] = bgra_bytes[i + 2];
            rgba[i + 1] = bgra_bytes[i + 1];
            rgba[i + 2] = bgra_bytes[i];
            rgba[i + 3] = bgra_bytes[i + 3];
        }

        let out_path = std::env::var("STREAMLIB_CRT_FILM_GRAIN_PNG_OUT")
            .unwrap_or_else(|_| "target/crt_film_grain_smoke.png".to_string());
        let _ = std::fs::create_dir_all(
            std::path::Path::new(&out_path)
                .parent()
                .unwrap_or_else(|| std::path::Path::new(".")),
        );
        let file = std::fs::File::create(&out_path)
            .unwrap_or_else(|e| panic!("create {out_path}: {e}"));
        let bw = std::io::BufWriter::new(file);
        let mut encoder = png::Encoder::new(bw, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header");
        writer.write_image_data(&rgba).expect("PNG data");
        eprintln!("crt_film_grain visual smoke wrote {out_path}");
    }
}
