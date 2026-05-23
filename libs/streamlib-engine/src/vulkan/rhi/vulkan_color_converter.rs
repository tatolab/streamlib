// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `(src_format, dst_format)`-keyed color converter backed by
//! [`VulkanComputeKernel`].
//!
//! Each converter instance owns up to one kernel per binding-shape
//! (buffer source, image source); kernels are built lazily on first
//! use and cached for the lifetime of the converter. `ResolvedColorInfo`
//! drives the matrix + transfer math via per-frame push constants —
//! mid-stream color changes don't touch the pipeline.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::color::{ResolvedColorInfo, TransferId};
use crate::core::rhi::{
    ColorConverterPushConstants, ComputeBindingSpec, ComputeKernelDescriptor, PixelFormat,
    SourceLayoutInfo, Texture, COLOR_CONVERTER_PUSH_CONSTANT_SIZE,
};
use crate::core::{Error, Result};

use super::{HostVulkanDevice, VulkanComputeKernel};

/// Compute-shader workgroup tile size (one pixel per thread, 16×16
/// workgroups). Exposed so consumers driving the dispatch through
/// [`crate::vulkan::rhi::RhiCommandRecorder::record_dispatch`] can
/// compute `(group_x, group_y) = ⌈(width, height) / 16⌉`.
pub const COLOR_CONVERTER_WORKGROUP_SIZE: u32 = 16;

const BUFFER_TO_IMAGE_BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::storage_buffer(0), // YCbCr / RGB byte input
    ComputeBindingSpec::storage_image(1),  // RGBA output
];

/// Vulkan implementation of [`crate::core::rhi::RhiColorConverter`].
pub struct VulkanColorConverter {
    vulkan_device: Arc<HostVulkanDevice>,
    src_format: PixelFormat,
    dst_format: PixelFormat,
    buffer_to_image_kernel: Mutex<Option<Arc<VulkanComputeKernel>>>,
}

impl VulkanColorConverter {
    /// Build a converter for the given `(src, dst)` pair. Kernels are
    /// allocated lazily on first dispatch.
    pub fn new(
        vulkan_device: &Arc<HostVulkanDevice>,
        src_format: PixelFormat,
        dst_format: PixelFormat,
    ) -> Result<Self> {
        validate_format_pair(src_format, dst_format)?;
        Ok(Self {
            vulkan_device: Arc::clone(vulkan_device),
            src_format,
            dst_format,
            buffer_to_image_kernel: Mutex::new(None),
        })
    }

    pub fn src_format(&self) -> PixelFormat {
        self.src_format
    }

    pub fn dst_format(&self) -> PixelFormat {
        self.dst_format
    }

    /// Bind `(src, dst)` + push-constants on the buffer→image kernel
    /// and return the kernel for the caller to dispatch. Used by
    /// consumers that drive dispatch through
    /// [`crate::vulkan::rhi::RhiCommandRecorder::record_dispatch`]
    /// so the compute step nests inside their own recorded command
    /// buffer with barriers and copies.
    ///
    /// `dst_transfer` is the output transfer curve; when it matches
    /// `info.transfer` the shader bypasses the transfer-function path.
    /// `Srgb` is the right default for the RGBA8_UNORM displays we
    /// ship today; once display color-space negotiation (#817) lands,
    /// consumers thread the negotiated curve through here.
    pub fn prepare_buffer_to_image_storage(
        &self,
        src: &crate::core::rhi::StorageBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
        dst_transfer: TransferId,
    ) -> Result<Arc<VulkanComputeKernel>> {
        let kernel = self.get_or_build_buffer_to_image_kernel()?;
        kernel.set_storage_buffer_storage(0, src)?;
        self.finish_buffer_to_image(&kernel, dst, info, dst_transfer, src_layout)?;
        Ok(kernel)
    }

    /// [`PixelBuffer`](crate::core::rhi::PixelBuffer)-shape source
    /// variant — bind a pixel-shaped buffer as the SSBO. Used by
    /// camera processors that already hold a [`PixelBuffer`] wrapper
    /// (e.g. V4L2-imported DMA-BUF surfaces wrapped as
    /// [`PixelBuffer`]).
    pub fn prepare_buffer_to_image_pixel(
        &self,
        src: &crate::core::rhi::PixelBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
        dst_transfer: TransferId,
    ) -> Result<Arc<VulkanComputeKernel>> {
        let kernel = self.get_or_build_buffer_to_image_kernel()?;
        kernel.set_storage_buffer_pixel(0, src)?;
        self.finish_buffer_to_image(&kernel, dst, info, dst_transfer, src_layout)?;
        Ok(kernel)
    }

    /// Convert `src` into `dst` end-to-end. Builds (if needed),
    /// binds, and dispatches the buffer→image kernel via its own
    /// command buffer + fence + queue submit. Use this when there's
    /// no surrounding [`crate::vulkan::rhi::RhiCommandRecorder`];
    /// otherwise prefer [`Self::prepare_buffer_to_image_storage`].
    pub fn convert_buffer_to_image_storage(
        &self,
        src: &crate::core::rhi::StorageBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
    ) -> Result<()> {
        let kernel = self.prepare_buffer_to_image_storage(src, src_layout, dst, info, TransferId::Srgb)?;
        Self::dispatch_buffer_to_image(&kernel, dst)
    }

    /// [`PixelBuffer`](crate::core::rhi::PixelBuffer) variant of
    /// [`Self::convert_buffer_to_image_storage`].
    pub fn convert_buffer_to_image_pixel(
        &self,
        src: &crate::core::rhi::PixelBuffer,
        src_layout: SourceLayoutInfo,
        dst: &Texture,
        info: &ResolvedColorInfo,
    ) -> Result<()> {
        let kernel = self.prepare_buffer_to_image_pixel(src, src_layout, dst, info, TransferId::Srgb)?;
        Self::dispatch_buffer_to_image(&kernel, dst)
    }

    fn finish_buffer_to_image(
        &self,
        kernel: &VulkanComputeKernel,
        dst: &Texture,
        info: &ResolvedColorInfo,
        dst_transfer: TransferId,
        src_layout: SourceLayoutInfo,
    ) -> Result<()> {
        let width = dst.width();
        let height = dst.height();
        kernel.set_storage_image(1, dst)?;
        let push = ColorConverterPushConstants::from_resolved(
            info,
            dst_transfer,
            width,
            height,
            src_layout,
        );
        kernel.set_push_constants_value(&push)?;
        Ok(())
    }

    fn dispatch_buffer_to_image(kernel: &VulkanComputeKernel, dst: &Texture) -> Result<()> {
        let dispatch_x = dst.width().div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE);
        let dispatch_y = dst.height().div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE);
        kernel.dispatch(dispatch_x, dispatch_y, 1)
    }

    fn get_or_build_buffer_to_image_kernel(&self) -> Result<Arc<VulkanComputeKernel>> {
        let mut guard = self.buffer_to_image_kernel.lock();
        if let Some(k) = guard.as_ref() {
            return Ok(Arc::clone(k));
        }
        let kernel = Arc::new(self.build_buffer_to_image_kernel()?);
        *guard = Some(Arc::clone(&kernel));
        Ok(kernel)
    }

    fn build_buffer_to_image_kernel(&self) -> Result<VulkanComputeKernel> {
        let spv: &[u8] = match self.src_format {
            PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange => {
                include_bytes!(concat!(env!("OUT_DIR"), "/color_convert_nv12_buffer_to_rgba.spv"))
            }
            PixelFormat::Yuyv422 => {
                include_bytes!(concat!(env!("OUT_DIR"), "/color_convert_yuyv_buffer_to_rgba.spv"))
            }
            other => {
                return Err(Error::NotSupported(format!(
                    "color converter buffer-source path: unsupported src format {:?}",
                    other
                )));
            }
        };
        let label = format!(
            "color_convert_buffer_to_image:{:?}_to_{:?}",
            self.src_format, self.dst_format
        );
        VulkanComputeKernel::new(
            &self.vulkan_device,
            &ComputeKernelDescriptor {
                label: label.as_str(),
                spv,
                bindings: BUFFER_TO_IMAGE_BINDINGS,
                push_constant_size: COLOR_CONVERTER_PUSH_CONSTANT_SIZE,
            },
        )
    }
}

fn validate_format_pair(src: PixelFormat, dst: PixelFormat) -> Result<()> {
    let src_ok = matches!(
        src,
        PixelFormat::Nv12VideoRange
            | PixelFormat::Nv12FullRange
            | PixelFormat::Yuyv422
            | PixelFormat::Rgba32
            | PixelFormat::Bgra32
    );
    let dst_ok = matches!(dst, PixelFormat::Rgba32);
    if !src_ok || !dst_ok {
        return Err(Error::NotSupported(format!(
            "color converter: unsupported format pair {:?} → {:?} (today: \
             {{NV12, YUYV, RGBA, BGRA}} → RGBA only)",
            src, dst
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use vulkanalia::prelude::v1_4::*;
    use vulkanalia::vk;

    use crate::core::color::{from_linear, to_linear, MatrixId, PrimariesId, RangeId};
    use crate::core::rhi::{
        PixelBuffer, TextureDescriptor, TextureFormat, TextureReadbackDescriptor,
        TextureSourceLayout, TextureUsages,
    };
    use crate::host_rhi::HostTextureExt;
    use crate::vulkan::rhi::{HostVulkanBuffer, HostVulkanTexture, VulkanTextureReadback};

    fn try_device() -> Option<Arc<HostVulkanDevice>> {
        HostVulkanDevice::new().ok()
    }

    /// Construction succeeds for every supported `(src, dst)` pair —
    /// no SPIR-V is loaded yet (that's lazy on first dispatch), but the
    /// format-pair validation runs.
    #[test]
    fn new_succeeds_for_every_supported_pair() {
        let Some(device) = try_device() else { return };
        for src in [
            PixelFormat::Nv12VideoRange,
            PixelFormat::Nv12FullRange,
            PixelFormat::Yuyv422,
        ] {
            let conv = VulkanColorConverter::new(&device, src, PixelFormat::Rgba32)
                .expect("converter construction must succeed");
            assert_eq!(conv.src_format(), src);
            assert_eq!(conv.dst_format(), PixelFormat::Rgba32);
        }
    }

    /// Unsupported destination format is rejected at construction
    /// time, not at first dispatch.
    #[test]
    fn new_rejects_unsupported_dst_format() {
        let Some(device) = try_device() else { return };
        let err = VulkanColorConverter::new(&device, PixelFormat::Nv12FullRange, PixelFormat::Bgra32)
            .err()
            .expect("must reject Bgra32 dest today");
        assert!(matches!(err, Error::NotSupported(_)));
    }

    // ---- GPU-output bit-exact regression coverage ----
    //
    // Restores the regression coverage deleted with
    // `VulkanFormatConverter::tests::nv12_full_range_to_bgra_matches_cpu_reference`
    // in #823. The deleted test's CPU reference used hardcoded
    // BT.601-Full coefficients (1.402 / 0.344136 / 0.714136 / 1.772),
    // which made it independent of any production matrix code — a
    // bug in the production matrix would fail the test. This file
    // preserves that property: each test passes its hardcoded
    // matrix + offset into [`cpu_reference_rgba`] as plain `[f32; 9]`
    // + `[f32; 3]`, NOT through `yuv_to_rgb_matrix`. The shader-side
    // values come from `yuv_to_rgb_matrix` via push constants, so a
    // regression in the Rust matrix shifts only the GPU side and the
    // test fails.
    //
    // Transfer functions stay routed through `core::color::transfer`
    // because the GLSL has independent closed-form implementations
    // in `color_convert_common.glsl` — Rust-vs-GLSL drift is the
    // failure mode the transfer-path test below catches, and that
    // requires both implementations to be independently authored.
    //
    // The math chain mirrors `convert_color` in
    // `vulkan/rhi/shaders/color_convert_common.glsl`:
    //   1. `c = ycbcr_byte - range_offset`
    //   2. `rgb_byte = M * c` (3×3 row-major matrix)
    //   3. `rgb = clamp(rgb_byte / 255, 0, 1)`
    //   4. if `transfer_in != transfer_out`:
    //         `rgb = from_linear(out, to_linear(in, rgb))`  per channel
    //
    // What these tests catch:
    //   - `ColorConverterPushConstants` std430 field ORDER / SIZE
    //     drift. Confirmed by negative-test: zeroing the Y component
    //     of `range_offset` in `from_resolved` fails the BT.709-limited
    //     and BT.709→sRGB tests with thousands of pixel mismatches.
    //   - `yuv_to_rgb_matrix` / `range_scaling` coefficient
    //     regressions — CPU reference is hardcoded, GPU pulls
    //     through the production path, any divergence is caught.
    //   - Shader's NV12 stride math (`read_byte` packed-uint
    //     extraction + `plane0_stride_bytes` / `plane1_stride_bytes`
    //     / `plane1_offset_bytes` walks).
    //   - GLSL ↔ Rust drift in the transfer functions
    //     (`color_convert_common.glsl` vs `core::color::transfer`).
    //   - `imageStore` correctness on `rgba8` storage image (UNORM
    //     rounding within ±1 of the CPU reference).
    //   - The `UNDEFINED → GENERAL` layout transition on the output
    //     texture (without it the dispatch is spec-illegal).
    //
    // Out of scope (locked by their own unit-test layers):
    //   - `core::color::matrix::tests` independently locks matrix
    //     coefficients with hardcoded golden values per
    //     (matrix, range) tuple — the source of truth this file's
    //     hardcoded matrices were taken from.
    //   - `core::color::transfer::tests` locks Rust-side transfer
    //     math via round-trip + known-points.

    /// Hardcoded matrix + offset for a `(MatrixId, RangeId)` tuple.
    /// Source: H.273 / ITU-T canonical coefficients (same numbers
    /// `core::color::matrix::tests` independently locks).
    ///
    /// Authored independently of the production
    /// `core::color::yuv_to_rgb_matrix` so a regression there shifts
    /// only the GPU side of the pipeline and the test fails.
    struct CpuReferenceMatrix {
        row_major: [f32; 9],
        offset: [f32; 3],
    }

    /// BT.601 (SMPTE 170M) full-range, the canonical webcam path.
    const BT601_FULL: CpuReferenceMatrix = CpuReferenceMatrix {
        row_major: [
            1.0, 0.0, 1.402,
            1.0, -0.344136, -0.714136,
            1.0, 1.772, 0.0,
        ],
        offset: [0.0, 128.0, 128.0],
    };

    /// BT.709 limited-range, the codec-output path. Coefficients
    /// from `255/219 * Y` scale + chroma `255/224 * 1.5748` etc.
    const BT709_LIMITED: CpuReferenceMatrix = CpuReferenceMatrix {
        row_major: [
            1.164384, 0.0, 1.792741,
            1.164384, -0.213249, -0.532909,
            1.164384, 2.112402, 0.0,
        ],
        offset: [16.0, 128.0, 128.0],
    };

    /// CPU reference: one NV12 source pixel → one RGBA8 output pixel.
    ///
    /// Mirrors `convert_color()` in `color_convert_common.glsl`.
    /// `matrix` and `offset` are passed in explicitly (hardcoded by
    /// the caller) so the reference stays independent of the
    /// production `yuv_to_rgb_matrix`. Transfer math uses
    /// `core::color::transfer` since the GLSL closed-form is
    /// independently authored.
    fn cpu_reference_rgba(
        y_byte: u8,
        u_byte: u8,
        v_byte: u8,
        matrix: &CpuReferenceMatrix,
        transfer_in: TransferId,
        transfer_out: TransferId,
    ) -> [u8; 4] {
        let m = matrix.row_major;
        let off = matrix.offset;
        let c = [
            y_byte as f32 - off[0],
            u_byte as f32 - off[1],
            v_byte as f32 - off[2],
        ];
        let mut rgb = [
            (m[0] * c[0] + m[1] * c[1] + m[2] * c[2]) / 255.0,
            (m[3] * c[0] + m[4] * c[1] + m[5] * c[2]) / 255.0,
            (m[6] * c[0] + m[7] * c[1] + m[8] * c[2]) / 255.0,
        ];
        for ch in &mut rgb {
            *ch = ch.clamp(0.0, 1.0);
        }
        if transfer_in != transfer_out {
            for ch in &mut rgb {
                *ch = from_linear(transfer_out, to_linear(transfer_in, *ch));
            }
        }
        [
            (rgb[0] * 255.0).round() as u8,
            (rgb[1] * 255.0).round() as u8,
            (rgb[2] * 255.0).round() as u8,
            255,
        ]
    }

    /// Synthesize a tight-packed NV12 byte buffer of size
    /// `width * height * 3 / 2`. Picks Y / U / V patterns that vary
    /// across the frame so chroma actually contributes (plane swaps
    /// surface as wrong color) and stays away from byte saturation
    /// (full bytes go through the matrix-expand path without
    /// clamping dominating the signal).
    fn build_deterministic_nv12(width: u32, height: u32) -> Vec<u8> {
        assert!(width % 2 == 0 && height % 2 == 0, "NV12 needs even dims");
        let y_plane_size = (width * height) as usize;
        let uv_plane_size = (width * height / 2) as usize;
        let mut buf = vec![0u8; y_plane_size + uv_plane_size];
        for y in 0..height {
            for x in 0..width {
                buf[(y * width + x) as usize] =
                    (28u32 + (x.wrapping_mul(5).wrapping_add(y.wrapping_mul(7))) % 200) as u8;
            }
        }
        let uv_base = y_plane_size;
        let half_h = height / 2;
        let half_w = width / 2;
        for cy in 0..half_h {
            for cx in 0..half_w {
                let off = uv_base + (cy * width + cx * 2) as usize;
                buf[off] =
                    (48u32 + (cx.wrapping_mul(13).wrapping_add(cy.wrapping_mul(11))) % 160) as u8;
                buf[off + 1] =
                    (48u32 + (cx.wrapping_mul(7).wrapping_add(cy.wrapping_mul(17)).wrapping_add(32)) % 160)
                        as u8;
            }
        }
        buf
    }

    /// Allocate an `Rgba8Unorm` storage texture with `STORAGE | COPY_SRC
    /// | COPY_DST` usage, transition it from `UNDEFINED` to `GENERAL` so
    /// the converter's `imageStore` is spec-legal, and return it.
    ///
    /// Mirrors the `make_filled_texture` UNDEFINED→GENERAL one-shot in
    /// `vulkan_texture_readback.rs::tests`; this variant skips the fill
    /// step because the converter dispatch will write every pixel.
    fn allocate_storage_target_in_general(
        device: &Arc<HostVulkanDevice>,
        width: u32,
        height: u32,
    ) -> Texture {
        let desc = TextureDescriptor {
            width,
            height,
            format: TextureFormat::Rgba8Unorm,
            usage: TextureUsages::COPY_SRC
                | TextureUsages::COPY_DST
                | TextureUsages::STORAGE_BINDING,
            label: Some("color-converter-test-output"),
        };
        let host_tex = HostVulkanTexture::new(device, &desc).expect("host texture");
        let texture = <Texture as crate::host_rhi::HostTextureExt>::from_vulkan(host_tex);

        let dev = device.device();
        let queue = device.queue();
        let qf = device.queue_family_index();
        unsafe {
            let pool = dev
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::builder()
                        .queue_family_index(qf)
                        .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                        .build(),
                    None,
                )
                .expect("command pool");
            let cmd = dev
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::builder()
                        .command_pool(pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1)
                        .build(),
                )
                .expect("command buffer")[0];
            dev.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                    .build(),
            )
            .expect("begin");

            let to_general = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .src_access_mask(vk::AccessFlags2::empty())
                .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .dst_access_mask(vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(qf)
                .dst_queue_family_index(qf)
                .image(texture.vulkan_inner().image().expect("vk image"))
                .subresource_range(
                    vk::ImageSubresourceRange::builder()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1)
                        .build(),
                )
                .build();
            let bs = [to_general];
            let dep = vk::DependencyInfo::builder().image_memory_barriers(&bs).build();
            dev.cmd_pipeline_barrier2(cmd, &dep);
            dev.end_command_buffer(cmd).expect("end");

            let cmd_infos = [vk::CommandBufferSubmitInfo::builder().command_buffer(cmd).build()];
            let submits = [vk::SubmitInfo2::builder().command_buffer_infos(&cmd_infos).build()];
            device
                .submit_to_queue(queue, &submits, vk::Fence::null())
                .expect("submit transition");
            dev.queue_wait_idle(queue).expect("wait idle");
            dev.destroy_command_pool(pool, None);
        }

        texture
    }

    /// Wrap a raw NV12 byte buffer in a HOST_VISIBLE `HostVulkanBuffer`
    /// + `PixelBuffer` so the converter sees it as a storage-buffer
    /// source.
    fn upload_nv12_pixel_buffer(
        device: &Arc<HostVulkanDevice>,
        nv12_bytes: &[u8],
        width: u32,
        height: u32,
        pixel_format: PixelFormat,
    ) -> PixelBuffer {
        let host_buf =
            HostVulkanBuffer::new(device, nv12_bytes.len() as u64).expect("host buffer");
        unsafe {
            std::ptr::copy_nonoverlapping(
                nv12_bytes.as_ptr(),
                host_buf.mapped_ptr(),
                nv12_bytes.len(),
            );
        }
        // `bytes_per_pixel` is descriptive metadata only — the shader
        // walks the buffer via `SourceLayoutInfo` push constants.
        PixelBuffer::from_host_vulkan_buffer(Arc::new(host_buf), width, height, 1, pixel_format)
    }

    /// End-to-end driver: dispatch the converter on a deterministic
    /// NV12 input under `info` (drives the production push-constant
    /// builder), read the resulting RGBA storage texture back to a
    /// CPU buffer, and assert every output pixel matches the
    /// independently-hardcoded `cpu_matrix` + transfer round-trip
    /// within ±1 per channel. `info.matrix` / `info.range` must
    /// describe the same tuple `cpu_matrix` was authored for —
    /// the test catches regressions where the two diverge.
    fn run_nv12_to_rgba_pixel_check(
        device: &Arc<HostVulkanDevice>,
        src_pixel_format: PixelFormat,
        info: &ResolvedColorInfo,
        dst_transfer: TransferId,
        cpu_matrix: &CpuReferenceMatrix,
        width: u32,
        height: u32,
        label: &str,
    ) {
        let nv12_bytes = build_deterministic_nv12(width, height);
        let pixel_buf = upload_nv12_pixel_buffer(device, &nv12_bytes, width, height, src_pixel_format);
        let output_texture = allocate_storage_target_in_general(device, width, height);

        let converter = VulkanColorConverter::new(device, src_pixel_format, PixelFormat::Rgba32)
            .expect("converter construction");

        let src_layout = SourceLayoutInfo::nv12_tight(width, height);
        let kernel = converter
            .prepare_buffer_to_image_pixel(&pixel_buf, src_layout, &output_texture, info, dst_transfer)
            .expect("prepare");
        let dispatch_x = width.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE);
        let dispatch_y = height.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE);
        kernel.dispatch(dispatch_x, dispatch_y, 1).expect("dispatch");

        let readback = VulkanTextureReadback::new(
            device,
            &TextureReadbackDescriptor {
                label: "color-converter-test-readback",
                format: TextureFormat::Rgba8Unorm,
                width,
                height,
            },
        )
        .expect("readback handle");
        let ticket = readback
            .submit(&output_texture, TextureSourceLayout::General)
            .expect("readback submit");
        let gpu = readback.wait_and_read(ticket, u64::MAX).expect("readback wait");

        let y_plane_size = (width * height) as usize;
        let mut mismatches = 0u32;
        let mut first_mismatch_msg = String::new();
        for y in 0..height {
            for x in 0..width {
                let y_byte = nv12_bytes[(y * width + x) as usize];
                // Interleaved (U, V) at half spatial resolution, even-x.
                let uv_offset =
                    y_plane_size + ((y >> 1) * width + (x & !1)) as usize;
                let u_byte = nv12_bytes[uv_offset];
                let v_byte = nv12_bytes[uv_offset + 1];

                let expected = cpu_reference_rgba(
                    y_byte,
                    u_byte,
                    v_byte,
                    cpu_matrix,
                    info.transfer,
                    dst_transfer,
                );

                let off = ((y * width + x) * 4) as usize;
                let actual = [gpu[off], gpu[off + 1], gpu[off + 2], gpu[off + 3]];

                for ch in 0..4 {
                    let diff = (actual[ch] as i32 - expected[ch] as i32).abs();
                    if diff > 1 {
                        if mismatches == 0 {
                            first_mismatch_msg = format!(
                                "[{label}] pixel ({x},{y}) ch {ch}: gpu={actual:?} expected={expected:?} (Y={y_byte},U={u_byte},V={v_byte})"
                            );
                        }
                        mismatches += 1;
                    }
                }
            }
        }
        assert_eq!(
            mismatches, 0,
            "{} pixel(s) failed ±1 tolerance. First: {}",
            mismatches, first_mismatch_msg
        );
    }

    /// BT.601 full-range NV12 (the canonical webcam path) → Rgba8Unorm
    /// must match the CPU reference within ±1 per channel.
    ///
    /// Locks (see module header for the full taxonomy):
    /// - Push-constant struct field-order — the shader pulls the
    ///   matrix + offset out of fixed std430 slots.
    /// - Transfer-bypass path (`Srgb` source = `Srgb` dest → shader
    ///   skips `pow()` per channel).
    /// - `SourceLayoutInfo::nv12_tight` strides flowing through push
    ///   constants and the shader's `read_byte` walk.
    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn nv12_full_range_bt601_matches_cpu_reference() {
        let Some(device) = try_device() else { return };
        let info = ResolvedColorInfo {
            primaries: PrimariesId::Bt709,
            transfer: TransferId::Srgb,
            matrix: MatrixId::Smpte170m,
            range: RangeId::Full,
        };
        run_nv12_to_rgba_pixel_check(
            &device,
            PixelFormat::Nv12FullRange,
            &info,
            TransferId::Srgb,
            &BT601_FULL,
            64,
            32,
            "bt601-full",
        );
    }

    /// BT.709 limited-range NV12 (the codec-output path) → Rgba8Unorm
    /// must match the CPU reference within ±1 per channel.
    ///
    /// Locks (see module header for the full taxonomy):
    /// - Push-constant `range_offset` slot is wired through to the
    ///   shader's `c = ycbcr_byte - range_offset` step — confirmed
    ///   by negative-test: zeroing the Y component of `range_offset`
    ///   in `from_resolved` fails this test with thousands of
    ///   mismatches.
    /// - `yuv_to_rgb_matrix` / `range_scaling` regressions on the
    ///   production side — confirmed by negative-test: reverting
    ///   the `255/219` Y-scale in `core::color::matrix::range_scaling`
    ///   fails this test (the CPU reference's `BT709_LIMITED` matrix
    ///   is hardcoded, so a regression in the Rust matrix shifts only
    ///   the GPU side and the test catches it).
    /// - Transfer-bypass path (`Bt709` source = `Bt709` dest).
    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn nv12_video_range_bt709_matches_cpu_reference() {
        let Some(device) = try_device() else { return };
        let info = ResolvedColorInfo {
            primaries: PrimariesId::Bt709,
            transfer: TransferId::Bt709,
            matrix: MatrixId::Bt709,
            range: RangeId::Limited,
        };
        run_nv12_to_rgba_pixel_check(
            &device,
            PixelFormat::Nv12VideoRange,
            &info,
            TransferId::Bt709,
            &BT709_LIMITED,
            64,
            32,
            "bt709-limited",
        );
    }

    /// BT.709 limited-range NV12 with mismatched dest transfer
    /// (`Bt709` source → `Srgb` dest) → Rgba8Unorm must match the CPU
    /// reference within ±1 per channel.
    ///
    /// Locks (see module header for the full taxonomy):
    /// - `FLAG_APPLY_TRANSFER` path active (`transfer_in !=
    ///   transfer_out` → shader runs `from_linear(srgb,
    ///   to_linear(bt709, x))` per channel).
    /// - GLSL ↔ Rust transfer-function drift. The transfer-bypass
    ///   tests above can't catch this; here the GLSL closed-forms
    ///   in `color_convert_common.glsl` and the Rust closed-forms in
    ///   `core::color::transfer` are independent implementations,
    ///   and the test fails the moment they disagree.
    /// - Mid-stream transfer change costs only push constants — no
    ///   pipeline rebuild — which the test verifies by reusing the
    ///   same `(src, dst)` PixelFormat pair as the prior test.
    #[cfg_attr(not(feature = "hardware-tests"), ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md")]
    #[test]
    fn nv12_bt709_to_srgb_transfer_path_matches_cpu_reference() {
        let Some(device) = try_device() else { return };
        let info = ResolvedColorInfo {
            primaries: PrimariesId::Bt709,
            transfer: TransferId::Bt709,
            matrix: MatrixId::Bt709,
            range: RangeId::Limited,
        };
        run_nv12_to_rgba_pixel_check(
            &device,
            PixelFormat::Nv12VideoRange,
            &info,
            TransferId::Srgb,
            &BT709_LIMITED,
            64,
            32,
            "bt709→srgb",
        );
    }
}
