// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Image→image tone-curve kernel backed by [`VulkanComputeKernel`].
//!
//! Sibling to [`super::vulkan_color_converter::VulkanColorConverter`].
//! The shader (`vulkan/rhi/shaders/tone_curve.comp`) handles BT.2390
//! EETF (HDR→SDR) and BT.2446-1 method A2 inverse (SDR→HDR) per channel,
//! plus pure transfer-conversion (no tone curve, peak rescale only).
//! All variation rides per-frame push constants — one cached kernel
//! instance covers every `(input_transfer, output_transfer, curve,
//! peak_in, peak_out)` combination.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::core::rhi::{
    ComputeBindingSpec, ComputeKernelDescriptor, TONE_MAPPER_PUSH_CONSTANT_SIZE, Texture,
    ToneMapperFinalTextureLayouts, ToneMapperPushConstants, VulkanLayout,
};
use crate::core::{Error, Result};
use crate::host_rhi::HostTextureExt;

use super::{
    HostVulkanDevice, RhiCommandRecorder, VulkanAccess, VulkanComputeKernel, VulkanStage,
    VulkanTextureLike,
};

/// Workgroup tile size. Matches the converter shader so consumers
/// computing dispatch dims (`⌈(width, height) / 16⌉`) can use a single
/// constant for both kernels.
pub const TONE_MAPPER_WORKGROUP_SIZE: u32 = 16;

const IMAGE_TO_IMAGE_BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::storage_image(0), // input  (readonly  in shader)
    ComputeBindingSpec::storage_image(1), // output (writeonly in shader)
];

/// Vulkan implementation of [`crate::core::rhi::RhiToneMapper`].
pub struct VulkanToneMapper {
    vulkan_device: Arc<HostVulkanDevice>,
    kernel: Mutex<Option<Arc<VulkanComputeKernel>>>,
}

impl VulkanToneMapper {
    /// Build a tone-mapper bound to a device. The kernel is allocated
    /// lazily on first dispatch.
    pub fn new(vulkan_device: &Arc<HostVulkanDevice>) -> Self {
        Self {
            vulkan_device: Arc::clone(vulkan_device),
            kernel: Mutex::new(None),
        }
    }

    /// Bind `(src, dst)` + push-constants and return the kernel for
    /// recorder-driven dispatch. Used by consumers that drive dispatch
    /// through [`crate::vulkan::rhi::RhiCommandRecorder::record_dispatch`]
    /// so the compute step nests inside their own recorded command
    /// buffer with barriers.
    pub fn prepare(
        &self,
        src: &Texture,
        dst: &Texture,
        push: &ToneMapperPushConstants,
    ) -> Result<Arc<VulkanComputeKernel>> {
        let kernel = self.get_or_build_kernel()?;
        kernel.set_storage_image(0, src)?;
        kernel.set_storage_image(1, dst)?;
        kernel.set_push_constants_value(push)?;
        Ok(kernel)
    }

    /// Apply tone curve to `src` into `dst` end-to-end. Builds (if
    /// needed), binds, and dispatches via the kernel's own command
    /// buffer + fence + queue submit. Caller is responsible for
    /// ensuring `src` and `dst` are already in `VulkanLayout::GENERAL`
    /// (the storage-image binding requirement).
    ///
    /// For consumers that need layout transitions handled, prefer
    /// [`Self::apply_with_layouts`].
    pub fn apply(
        &self,
        src: &Texture,
        dst: &Texture,
        push: &ToneMapperPushConstants,
    ) -> Result<()> {
        let kernel = self.prepare(src, dst, push)?;
        let dispatch_x = push.width.div_ceil(TONE_MAPPER_WORKGROUP_SIZE);
        let dispatch_y = push.height.div_ceil(TONE_MAPPER_WORKGROUP_SIZE);
        kernel.dispatch(dispatch_x, dispatch_y, 1)
    }

    /// Apply tone curve with caller-declared current layouts, recording
    /// the `→ GENERAL` pre-barriers + dispatch + post-barriers in one
    /// engine-owned command buffer; submits and waits before returning.
    /// Each texture's terminal layout is chosen from its create-time
    /// usage — `SHADER_READ_ONLY_OPTIMAL` when the image is
    /// sampled-capable, `GENERAL` otherwise (SROO on an image without
    /// `SAMPLED` / `INPUT_ATTACHMENT` usage violates
    /// VUID-VkImageMemoryBarrier2-oldLayout-01211) — and returned as
    /// [`ToneMapperFinalTextureLayouts`].
    ///
    /// Used by [`crate::core::context::GpuContext`] consumers (e.g.,
    /// the `BlendingCompositor` per-acquire conversion) that don't
    /// already own a surrounding [`RhiCommandRecorder`] but do need
    /// honest layout management around the dispatch.
    ///
    /// Caller contract:
    /// - `src` and `dst` must reference distinct VkImages — in-place
    ///   tone-map is not supported; passing the same handle returns
    ///   `Err(Error::Configuration)`.
    /// - The declared current layouts must be layouts the images'
    ///   usage permits — `SHADER_READ_ONLY_OPTIMAL` declared for an
    ///   image without `SAMPLED` / `INPUT_ATTACHMENT` usage returns
    ///   `Err(Error::Configuration)`.
    /// - The helper does not touch
    ///   [`crate::core::context::TextureRegistration`]; the caller
    ///   updates any associated registration with the returned
    ///   terminal layouts after the call.
    ///
    /// For consumers that already drive their own recorder, prefer
    /// [`Self::prepare`] + their recorder's `record_image_barrier` +
    /// `record_dispatch` to avoid the extra submit/wait round-trip.
    pub fn apply_with_layouts(
        &self,
        src: &Texture,
        src_current_layout: VulkanLayout,
        dst: &Texture,
        dst_current_layout: VulkanLayout,
        push: &ToneMapperPushConstants,
    ) -> Result<ToneMapperFinalTextureLayouts> {
        // `vulkan_inner()` routes through `Texture::host_inner()`,
        // which panics in cdylib mode by design — the host owns the
        // RHI and cdylib code must reach `HostVulkanTexture` through
        // the FullAccess vtable. `host_vulkan_texture_arc()` is the
        // documented cdylib-safe accessor: in host mode it
        // `Arc::clone`s the inner directly; in cdylib mode it
        // dispatches through the v10 FullAccess vtable slot so the
        // host returns its own `Arc<HostVulkanTexture>` and both the
        // `VkImage` compare and the usage-driven layout choice run
        // against the real host-side handles.
        let src_host_texture = src.host_vulkan_texture_arc()?;
        let dst_host_texture = dst.host_vulkan_texture_arc()?;

        // Reject in-place tone-map. The kernel binds src + dst as
        // distinct storage images and the barrier sequence (src →
        // GENERAL, dst → GENERAL, dispatch, src post, dst post)
        // emits conflicting layout claims on the same VkImage when
        // the caller passes the same handle for both, which trips
        // VUID-VkImageMemoryBarrier2-oldLayout-01197 twice per submit.
        // Catch the misuse here rather than as validation noise 60
        // frames downstream.
        if let (Some(src_image), Some(dst_image)) =
            (src_host_texture.image(), dst_host_texture.image())
        {
            if src_image == dst_image {
                return Err(Error::Configuration(
                    "VulkanToneMapper::apply_with_layouts: src and dst must be distinct VkImages (in-place tone-map is not supported)".into(),
                ));
            }
        }

        let src_final_layout = src_host_texture.terminal_layout_for_shader_read_access();
        let dst_final_layout = dst_host_texture.terminal_layout_for_shader_read_access();

        // The oldLayout side of VUID-VkImageMemoryBarrier2-oldLayout-01211:
        // a declared current layout of SHADER_READ_ONLY_OPTIMAL on an
        // image whose usage cannot legally hold that layout is the same
        // violation the terminal-layout choice above prevents — the
        // image cannot actually be in the layout the caller claims.
        // Refuse it loudly instead of feeding it into the pre-barrier.
        for (texture_role, declared_current_layout, usage_legal_terminal_layout) in [
            ("src", src_current_layout, src_final_layout),
            ("dst", dst_current_layout, dst_final_layout),
        ] {
            if declared_current_layout == VulkanLayout::SHADER_READ_ONLY_OPTIMAL
                && usage_legal_terminal_layout != VulkanLayout::SHADER_READ_ONLY_OPTIMAL
            {
                return Err(Error::Configuration(format!(
                    "VulkanToneMapper::apply_with_layouts: {texture_role} declares current layout SHADER_READ_ONLY_OPTIMAL but its image was created without SAMPLED or INPUT_ATTACHMENT usage, which that layout requires (VUID-VkImageMemoryBarrier2-oldLayout-01211)"
                )));
            }
        }

        let kernel = self.prepare(src, dst, push)?;
        let dispatch_x = push.width.div_ceil(TONE_MAPPER_WORKGROUP_SIZE);
        let dispatch_y = push.height.div_ceil(TONE_MAPPER_WORKGROUP_SIZE);

        let mut recorder = RhiCommandRecorder::new(&self.vulkan_device, "tone_curve_apply")?;
        recorder.begin()?;
        // Pre-barriers: src + dst → GENERAL for storage-image read/write.
        recorder.record_image_barrier(
            src,
            src_current_layout,
            VulkanLayout::GENERAL,
            VulkanStage::ALL_COMMANDS,
            VulkanStage::COMPUTE_SHADER,
            VulkanAccess::MEMORY_READ | VulkanAccess::MEMORY_WRITE,
            VulkanAccess::SHADER_READ,
        )?;
        recorder.record_image_barrier(
            dst,
            dst_current_layout,
            VulkanLayout::GENERAL,
            VulkanStage::ALL_COMMANDS,
            VulkanStage::COMPUTE_SHADER,
            VulkanAccess::MEMORY_READ | VulkanAccess::MEMORY_WRITE,
            VulkanAccess::SHADER_WRITE,
        )?;
        recorder.record_dispatch(&kernel, dispatch_x, dispatch_y, 1)?;
        // Post-barriers: leave each image in its usage-legal terminal
        // layout ("ready for the next consumer"). A GENERAL→GENERAL
        // barrier is not a no-op — it still orders the dispatch's
        // shader access against downstream consumers and makes the
        // writes available. The src barrier keeps its registration
        // claim from drifting if another consumer reads the same
        // surface_id afterward.
        recorder.record_image_barrier(
            src,
            VulkanLayout::GENERAL,
            src_final_layout,
            VulkanStage::COMPUTE_SHADER,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::SHADER_READ,
            VulkanAccess::MEMORY_READ | VulkanAccess::MEMORY_WRITE,
        )?;
        recorder.record_image_barrier(
            dst,
            VulkanLayout::GENERAL,
            dst_final_layout,
            VulkanStage::COMPUTE_SHADER,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::SHADER_WRITE,
            VulkanAccess::MEMORY_READ | VulkanAccess::MEMORY_WRITE,
        )?;
        recorder.submit_and_wait()?;
        Ok(ToneMapperFinalTextureLayouts {
            src_final_layout,
            dst_final_layout,
        })
    }

    fn get_or_build_kernel(&self) -> Result<Arc<VulkanComputeKernel>> {
        let mut guard = self.kernel.lock();
        if let Some(k) = guard.as_ref() {
            return Ok(Arc::clone(k));
        }
        let kernel = Arc::new(self.build_kernel()?);
        *guard = Some(Arc::clone(&kernel));
        Ok(kernel)
    }

    fn build_kernel(&self) -> Result<VulkanComputeKernel> {
        let spv: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tone_curve.spv"));
        VulkanComputeKernel::new(
            &self.vulkan_device,
            &ComputeKernelDescriptor {
                entry_point: "main",
                label: "tone_curve_image_to_image",
                spv,
                bindings: IMAGE_TO_IMAGE_BINDINGS,
                push_constant_size: TONE_MAPPER_PUSH_CONSTANT_SIZE,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::color::{
        TransferId, bt2390_eetf_per_channel, linear_to_pq, linear_to_srgb, pq_to_linear,
        srgb_to_linear,
    };
    use crate::core::rhi::{
        TextureDescriptor, TextureFormat, TextureReadbackDescriptor, TextureSourceLayout,
        TextureUsages, ToneCurveId, ToneMapperPushConstants,
    };
    use crate::vulkan::rhi::{HostVulkanBuffer, HostVulkanTexture, VulkanTextureReadback};
    use vulkanalia::prelude::v1_4::*;
    use vulkanalia::vk;

    fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
        HostVulkanDevice::new().ok()
    }

    /// Construction is cheap (lazy kernel build) and must succeed on
    /// any host that brings up `HostVulkanDevice`. Without a Vulkan
    /// runtime this skips silently per the existing test patterns
    /// elsewhere in `vulkan/rhi/`.
    #[test]
    fn new_is_cheap_and_lazy() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        let mapper = VulkanToneMapper::new(&device);
        // Kernel must be unbuilt at this point.
        assert!(mapper.kernel.lock().is_none());
    }

    /// Bake `pattern_bgra(x, y)` into a fresh `STORAGE_BINDING |
    /// COPY_SRC | COPY_DST` BGRA8 texture, leaving it in
    /// `VK_IMAGE_LAYOUT_GENERAL` (ready for the tone-mapper kernel's
    /// storage-image read binding). Mirrors `make_filled_texture` in
    /// `vulkan_texture_readback::tests` — duplicated here because that
    /// helper is private to that test module and the alternative
    /// (lifting it to a `pub(crate) fn`) would put test scaffolding in
    /// the production module tree.
    fn make_general_texture(
        device: &Arc<HostVulkanDevice>,
        width: u32,
        height: u32,
        pattern_bgra: impl Fn(u32, u32) -> [u8; 4],
    ) -> Texture {
        let bpp: u32 = 4;
        let staging =
            HostVulkanBuffer::new(device, (width as u64) * (height as u64) * (bpp as u64))
                .expect("staging");
        unsafe {
            let mut p = staging.mapped_ptr();
            for y in 0..height {
                for x in 0..width {
                    let px = pattern_bgra(x, y);
                    std::ptr::copy_nonoverlapping(px.as_ptr(), p, 4);
                    p = p.add(4);
                }
            }
        }
        let desc = TextureDescriptor {
            width,
            height,
            format: TextureFormat::Bgra8Unorm,
            usage: TextureUsages::COPY_SRC
                | TextureUsages::COPY_DST
                | TextureUsages::STORAGE_BINDING,
            label: Some("tone-mapper-test-input"),
        };
        let host_tex = HostVulkanTexture::new(device, &desc).expect("texture");
        let texture = <Texture as crate::host_rhi::HostTextureExt>::from_vulkan(host_tex);

        let dev = device.device();
        let queue = device.queue();
        let qf = device.queue_family_index();
        let pool = unsafe {
            dev.create_command_pool(
                &vk::CommandPoolCreateInfo::builder()
                    .queue_family_index(qf)
                    .flags(vk::CommandPoolCreateFlags::TRANSIENT)
                    .build(),
                None,
            )
        }
        .expect("pool");
        let cmd = unsafe {
            dev.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::builder()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1)
                    .build(),
            )
        }
        .expect("cmd")[0];
        unsafe {
            dev.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::builder()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)
                    .build(),
            )
            .expect("begin");
            let to_dst = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .src_access_mask(vk::AccessFlags2::empty())
                .dst_stage_mask(vk::PipelineStageFlags2::COPY)
                .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .old_layout(vk::ImageLayout::UNDEFINED)
                .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .src_queue_family_index(qf)
                .dst_queue_family_index(qf)
                .image({
                    use crate::host_rhi::HostTextureExt;
                    texture.vulkan_inner().image().expect("vk image")
                })
                .subresource_range(
                    vk::ImageSubresourceRange::builder()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1)
                        .build(),
                )
                .build();
            let bs = [to_dst];
            let dep = vk::DependencyInfo::builder()
                .image_memory_barriers(&bs)
                .build();
            dev.cmd_pipeline_barrier2(cmd, &dep);

            let copy = vk::BufferImageCopy::builder()
                .buffer_offset(0)
                .buffer_row_length(0)
                .buffer_image_height(0)
                .image_subresource(
                    vk::ImageSubresourceLayers::builder()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .layer_count(1)
                        .build(),
                )
                .image_offset(vk::Offset3D { x: 0, y: 0, z: 0 })
                .image_extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                })
                .build();
            let regions = [copy];
            dev.cmd_copy_buffer_to_image(
                cmd,
                staging.buffer(),
                {
                    use crate::host_rhi::HostTextureExt;
                    texture.vulkan_inner().image().expect("vk image")
                },
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &regions,
            );
            let to_general = vk::ImageMemoryBarrier2::builder()
                .src_stage_mask(vk::PipelineStageFlags2::COPY)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .dst_access_mask(vk::AccessFlags2::MEMORY_READ)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::GENERAL)
                .src_queue_family_index(qf)
                .dst_queue_family_index(qf)
                .image({
                    use crate::host_rhi::HostTextureExt;
                    texture.vulkan_inner().image().expect("vk image")
                })
                .subresource_range(
                    vk::ImageSubresourceRange::builder()
                        .aspect_mask(vk::ImageAspectFlags::COLOR)
                        .level_count(1)
                        .layer_count(1)
                        .build(),
                )
                .build();
            let bs2 = [to_general];
            let dep2 = vk::DependencyInfo::builder()
                .image_memory_barriers(&bs2)
                .build();
            dev.cmd_pipeline_barrier2(cmd, &dep2);
            dev.end_command_buffer(cmd).expect("end");
            let cmd_infos = [vk::CommandBufferSubmitInfo::builder()
                .command_buffer(cmd)
                .build()];
            let submits = [vk::SubmitInfo2::builder()
                .command_buffer_infos(&cmd_infos)
                .build()];
            device
                .submit_to_queue(queue, &submits, vk::Fence::null())
                .expect("submit fill");
            dev.queue_wait_idle(queue).expect("wait idle");
            dev.destroy_command_pool(pool, None);
        }
        texture
    }

    /// Allocate an empty BGRA8 texture with the given usage and debug
    /// label. Layout is `UNDEFINED` on return — `apply_with_layouts`
    /// transitions it.
    fn make_empty_bgra8_texture_with_usage(
        device: &Arc<HostVulkanDevice>,
        width: u32,
        height: u32,
        usage: TextureUsages,
        label: &'static str,
    ) -> Texture {
        let desc = TextureDescriptor {
            width,
            height,
            format: TextureFormat::Bgra8Unorm,
            usage,
            label: Some(label),
        };
        let host_tex = HostVulkanTexture::new(device, &desc).expect("texture");
        <Texture as crate::host_rhi::HostTextureExt>::from_vulkan(host_tex)
    }

    /// Canonical PQ → sRGB BT.2390 1000 nit → 100 nit push constants
    /// shared by the dispatch-shaped tests.
    fn pq_to_srgb_bt2390_push_constants(width: u32, height: u32) -> ToneMapperPushConstants {
        ToneMapperPushConstants::new(
            width,
            height,
            TransferId::Pq,
            TransferId::Srgb,
            ToneCurveId::Bt2390,
            1000.0,
            100.0,
        )
    }

    /// Convert a CPU-reference HDR PQ pixel into the BGRA8 byte triple
    /// baked into the test input texture.
    ///
    /// The shader's storage images carry no format qualifier (SPIR-V
    /// format `Unknown`), so `imageLoad` honors the bound view's
    /// `B8G8R8A8_UNORM` component order and the shader's `vec4` is
    /// `(R, G, B, A)` with no reinterpretation. The fixture is
    /// grayscale (one byte across all three channels), so the expected
    /// bytes are channel-order-independent regardless.
    fn pq_pixel(linear_norm_0_to_1: f32) -> [u8; 4] {
        let abs_lin = (linear_norm_0_to_1 * 1000.0) / 10_000.0;
        let pq = linear_to_pq(abs_lin).clamp(0.0, 1.0);
        let byte = (pq * 255.0).round() as u8;
        // BGRA8 byte order matches the test fixture; the shader sees
        // the same numeric value per channel.
        [byte, byte, byte, 0xFF]
    }

    /// GPU dispatch parity test for the BT.2390 EETF kernel path.
    ///
    /// Bakes a 16x16 PQ-encoded grayscale ramp into a BGRA8 storage
    /// texture, dispatches the tone-mapper at PQ → Srgb with BT.2390
    /// 1000 nit → 100 nit, reads back the output, and asserts every
    /// pixel matches the CPU reference within an 8-bit-quantization
    /// tolerance (±2 ULPs).
    ///
    /// Mentally revert the GLSL Hermite spline (drop the
    /// `(b3 - 2.0*b2 + b) * (1.0 - ks)` term in `tone_curve.comp`'s
    /// `bt2390_eetf`) and this test fails — the GPU output diverges
    /// from the CPU reference everywhere above the knee.
    #[test]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    fn bt2390_pq_to_srgb_matches_cpu_reference() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        let width = 16u32;
        let height = 16u32;
        let pin_nits = 1000.0_f32;
        let pout_nits = 100.0_f32;

        // Per-pixel input linear value (normalized to peak_in_nits).
        let input_linear_for = |x: u32, y: u32| -> f32 {
            let idx = (y * width + x) as f32;
            idx / (width * height - 1) as f32
        };

        let src = make_general_texture(&device, width, height, |x, y| {
            pq_pixel(input_linear_for(x, y))
        });
        let dst = make_empty_bgra8_texture_with_usage(
            &device,
            width,
            height,
            TextureUsages::COPY_SRC | TextureUsages::COPY_DST | TextureUsages::STORAGE_BINDING,
            "tone-mapper-test-output",
        );

        let mapper = VulkanToneMapper::new(&device);
        let push = ToneMapperPushConstants::new(
            width,
            height,
            TransferId::Pq,
            TransferId::Srgb,
            ToneCurveId::Bt2390,
            pin_nits,
            pout_nits,
        );

        let final_layouts = mapper
            .apply_with_layouts(
                &src,
                VulkanLayout::GENERAL,
                &dst,
                VulkanLayout::UNDEFINED,
                &push,
            )
            .expect("apply_with_layouts");

        // Neither fixture carries SAMPLED usage, so SHADER_READ_ONLY_OPTIMAL
        // is illegal for both (VUID-VkImageMemoryBarrier2-oldLayout-01211)
        // and the mapper must leave them in GENERAL.
        assert_eq!(final_layouts.src_final_layout, VulkanLayout::GENERAL);
        assert_eq!(final_layouts.dst_final_layout, VulkanLayout::GENERAL);

        let readback = VulkanTextureReadback::new(
            &device,
            &TextureReadbackDescriptor {
                label: "tone-mapper-parity",
                format: TextureFormat::Bgra8Unorm,
                width,
                height,
            },
        )
        .expect("readback");
        let ticket = readback
            .submit(&dst, TextureSourceLayout::General)
            .expect("submit");
        let bytes = readback.wait_and_read(ticket, u64::MAX).expect("read");

        // CPU reference: per channel, EOTF(input) → BT.2390(linear) → OETF(output).
        // The kernel applies the same chain per channel.
        for y in 0..height {
            for x in 0..width {
                let input_lin_norm = input_linear_for(x, y);
                // Round-trip through PQ encode/decode to match the GPU
                // shader's same encode-then-decode chain (since the
                // input texture stores PQ-encoded bytes).
                let pq_in = (input_lin_norm * pin_nits / 10_000.0).clamp(0.0, 1.0);
                let pq_encoded = linear_to_pq(pq_in);
                // GPU shader path: EOTF(pq_encoded) → tone-curve → OETF(srgb)
                let lin_in = pq_to_linear(pq_encoded) * 10_000.0 / pin_nits;
                let lin_tone = bt2390_eetf_per_channel(lin_in, pin_nits, pout_nits);
                let srgb_out = linear_to_srgb(lin_tone.clamp(0.0, 1.0));
                let expected_byte = (srgb_out * 255.0).round() as i32;

                let off = ((y * width + x) * 4) as usize;
                // BGRA storage: bytes [B, G, R, A].
                for ch in 0..3 {
                    let actual = bytes[off + ch] as i32;
                    let delta = (actual - expected_byte).abs();
                    assert!(
                        delta <= 2,
                        "pixel ({x},{y}) ch {ch}: GPU={actual} expected={expected_byte} (delta {delta})"
                    );
                }
                // Alpha should pass through as 0xFF.
                assert_eq!(
                    bytes[off + 3],
                    0xFF,
                    "alpha pass-through broken at ({x},{y})"
                );
            }
        }
    }

    /// A sampled-capable pair (`TEXTURE_BINDING` in the usage) keeps
    /// the pre-fix terminal state: both textures land in
    /// `SHADER_READ_ONLY_OPTIMAL` — the production path (compositor
    /// intermediates created with `SAMPLED` usage) does not regress to
    /// GENERAL. Mentally revert the usage-driven layout choice in
    /// `apply_with_layouts` to unconditional GENERAL and this fails.
    #[test]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    fn apply_with_layouts_leaves_sampled_capable_textures_shader_read_only() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        let sampled_capable_usage = TextureUsages::COPY_SRC
            | TextureUsages::COPY_DST
            | TextureUsages::STORAGE_BINDING
            | TextureUsages::TEXTURE_BINDING;
        let src = make_empty_bgra8_texture_with_usage(
            &device,
            8,
            8,
            sampled_capable_usage,
            "tone-mapper-test-sampled-src",
        );
        let dst = make_empty_bgra8_texture_with_usage(
            &device,
            8,
            8,
            sampled_capable_usage,
            "tone-mapper-test-sampled-dst",
        );
        let push = pq_to_srgb_bt2390_push_constants(8, 8);
        let mapper = VulkanToneMapper::new(&device);
        let final_layouts = mapper
            .apply_with_layouts(
                &src,
                VulkanLayout::UNDEFINED,
                &dst,
                VulkanLayout::UNDEFINED,
                &push,
            )
            .expect("apply_with_layouts");
        assert_eq!(
            final_layouts.src_final_layout,
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL
        );
        assert_eq!(
            final_layouts.dst_final_layout,
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL
        );
    }

    /// A declared *current* layout of `SHADER_READ_ONLY_OPTIMAL` on a
    /// storage-only image is the oldLayout side of
    /// VUID-VkImageMemoryBarrier2-oldLayout-01211 — the image cannot
    /// actually be in that layout. The mapper refuses it loudly
    /// instead of recording the illegal pre-barrier.
    #[test]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    fn apply_with_layouts_refuses_a_declared_layout_the_usage_cannot_hold() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        let storage_only_usage =
            TextureUsages::COPY_SRC | TextureUsages::COPY_DST | TextureUsages::STORAGE_BINDING;
        let src =
            make_empty_bgra8_texture_with_usage(&device, 8, 8, storage_only_usage, "refusal-src");
        let dst =
            make_empty_bgra8_texture_with_usage(&device, 8, 8, storage_only_usage, "refusal-dst");
        let push = pq_to_srgb_bt2390_push_constants(8, 8);
        let mapper = VulkanToneMapper::new(&device);
        let result = mapper.apply_with_layouts(
            &src,
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            &dst,
            VulkanLayout::UNDEFINED,
            &push,
        );
        match result {
            Err(crate::core::Error::Configuration(msg)) => {
                assert!(
                    msg.contains("src") && msg.contains("SHADER_READ_ONLY_OPTIMAL"),
                    "expected a refusal naming src and the illegal layout, got: {msg}"
                );
            }
            other => panic!("expected Err(Configuration), got {other:?}"),
        }
    }

    /// The tone-curve SPIR-V declares the two `WithoutFormat`
    /// capabilities its format-less storage images require — the
    /// shader-side half of the device-feature pairing enabled in
    /// `HostVulkanDevice::new` (`shaderStorageImageReadWithoutFormat` /
    /// `shaderStorageImageWriteWithoutFormat`). Runs without a GPU:
    /// it parses the compiled blob's `OpCapability` instructions.
    #[test]
    fn tone_curve_spirv_declares_the_without_format_capabilities() {
        const SPIRV_MAGIC: u32 = 0x0723_0203;
        const OP_CAPABILITY: u32 = 17;
        const CAPABILITY_STORAGE_IMAGE_READ_WITHOUT_FORMAT: u32 = 55;
        const CAPABILITY_STORAGE_IMAGE_WRITE_WITHOUT_FORMAT: u32 = 56;

        let spv: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tone_curve.spv"));
        assert_eq!(spv.len() % 4, 0, "SPIR-V blob must be word-aligned");
        let words: Vec<u32> = spv
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        assert_eq!(words[0], SPIRV_MAGIC, "not a SPIR-V module");

        let mut declared_capabilities = Vec::new();
        // Instructions start after the 5-word header; word 0 of each is
        // (word_count << 16) | opcode.
        let mut cursor = 5;
        while cursor < words.len() {
            let word_count = (words[cursor] >> 16) as usize;
            let opcode = words[cursor] & 0xFFFF;
            assert!(word_count > 0, "malformed SPIR-V instruction");
            if opcode == OP_CAPABILITY {
                declared_capabilities.push(words[cursor + 1]);
            }
            cursor += word_count;
        }

        assert!(
            declared_capabilities.contains(&CAPABILITY_STORAGE_IMAGE_READ_WITHOUT_FORMAT),
            "tone_curve.spv no longer declares StorageImageReadWithoutFormat — \
             if the shader regained a format qualifier, drop the matching \
             device-feature enable in HostVulkanDevice::new"
        );
        assert!(
            declared_capabilities.contains(&CAPABILITY_STORAGE_IMAGE_WRITE_WITHOUT_FORMAT),
            "tone_curve.spv no longer declares StorageImageWriteWithoutFormat — \
             if the shader regained a format qualifier, drop the matching \
             device-feature enable in HostVulkanDevice::new"
        );
    }

    /// Sanity check on `pq_pixel`: 1.0 linear → PQ encoded byte for the
    /// 1000-nit peak. Locks the fixture math against drift.
    #[test]
    fn pq_pixel_at_peak_matches_pq_encoding() {
        let bytes = pq_pixel(1.0);
        let expected = (linear_to_pq(0.1) * 255.0).round() as u8;
        assert_eq!(bytes[0], expected);
        assert_eq!(bytes[1], expected);
        assert_eq!(bytes[2], expected);
        assert_eq!(bytes[3], 0xFF);
    }

    /// Sanity check on the CPU reference chain at peak input — should
    /// reach near-peak sRGB (1.0 → byte 255). Locks the expected-value
    /// computation in `bt2390_pq_to_srgb_matches_cpu_reference` against
    /// drift in any of the helper functions.
    #[test]
    fn srgb_round_trip_at_unit_holds_to_byte_255() {
        let lin = pq_to_linear(linear_to_pq(0.1)) * 10.0; // ≈ 1.0
        let srgb = linear_to_srgb(lin.clamp(0.0, 1.0));
        let byte = (srgb * 255.0).round() as i32;
        assert_eq!(byte, 255, "round-trip should land at byte 255");
        // Suppress unused-import warnings if the GPU test is skipped.
        let _ = srgb_to_linear(0.5);
    }

    /// `apply_with_layouts` rejects `src == dst` with an `Err`. Catches
    /// the caller-side cache-reuse bug class (a layer's `texture`
    /// referring to the same VkImage as the per-port intermediate)
    /// directly, instead of surfacing it as VUID-01197 validation
    /// noise on every per-frame submit downstream. Mentally revert
    /// the new precondition check at the top of
    /// `VulkanToneMapper::apply_with_layouts` and this test fails.
    #[test]
    #[cfg_attr(
        not(feature = "hardware-tests"),
        ignore = "hardware integration — set --features streamlib/hardware-tests + run with --test-threads=1. See docs/testing-hardware.md"
    )]
    fn apply_with_layouts_rejects_same_image_for_src_and_dst() {
        let Some(device) = try_vulkan_device() else {
            return;
        };
        let texture = make_general_texture(&device, 8, 8, |_, _| [0; 4]);
        let push = pq_to_srgb_bt2390_push_constants(8, 8);
        let mapper = VulkanToneMapper::new(&device);
        let result = mapper.apply_with_layouts(
            &texture,
            VulkanLayout::GENERAL,
            &texture, // same VkImage handle as src
            VulkanLayout::GENERAL,
            &push,
        );
        match result {
            Err(crate::core::Error::Configuration(msg)) => {
                assert!(
                    msg.contains("distinct VkImages"),
                    "expected distinct-VkImages diagnostic, got: {msg}"
                );
            }
            other => panic!("expected Err(Configuration), got {other:?}"),
        }
    }
}
