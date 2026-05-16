// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Image→image tone-curve kernel backed by `VulkanComputeKernel`.
//!
//! The shader (`src/shaders/tone_curve.comp`) handles BT.2390 EETF
//! (HDR→SDR) and BT.2446-1 method A2 inverse (SDR→HDR) per channel,
//! plus pure transfer-conversion (no tone curve, peak rescale only).
//! All variation rides per-frame push constants — one kernel instance
//! per consumer covers every `(input_transfer, output_transfer, curve,
//! peak_in, peak_out)` combination.

use std::sync::Arc;

use parking_lot::Mutex;

use streamlib::sdk::engine::host_rhi::{
    HostVulkanDevice, RhiCommandRecorder, VulkanAccess, VulkanComputeKernel, VulkanStage,
};
use streamlib::sdk::error::Result;
use streamlib::sdk::rhi::{ComputeBindingSpec, ComputeKernelDescriptor, Texture, VulkanLayout};

use crate::tone_mapper::{ToneMapperPushConstants, TONE_MAPPER_PUSH_CONSTANT_SIZE};

/// Workgroup tile size.
pub const TONE_MAPPER_WORKGROUP_SIZE: u32 = 16;

const IMAGE_TO_IMAGE_BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::storage_image(0), // input  (readonly  in shader)
    ComputeBindingSpec::storage_image(1), // output (writeonly in shader)
];

/// Vulkan implementation of [`crate::RhiToneMapper`].
pub struct VulkanToneMapper {
    vulkan_device: Arc<HostVulkanDevice>,
    pub(crate) kernel: Mutex<Option<Arc<VulkanComputeKernel>>>,
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
    /// recorder-driven dispatch.
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
    /// ensuring `src` and `dst` are already in `VulkanLayout::GENERAL`.
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
    /// the `→ GENERAL` pre-barriers + dispatch + `→ SHADER_READ_ONLY_OPTIMAL`
    /// post-barriers in one engine-owned command buffer; submits and
    /// waits before returning. Both `src` and `dst` are left in
    /// [`VulkanLayout::SHADER_READ_ONLY_OPTIMAL`] on success.
    pub fn apply_with_layouts(
        &self,
        src: &Texture,
        src_current_layout: VulkanLayout,
        dst: &Texture,
        dst_current_layout: VulkanLayout,
        push: &ToneMapperPushConstants,
    ) -> Result<()> {
        let kernel = self.prepare(src, dst, push)?;
        let dispatch_x = push.width.div_ceil(TONE_MAPPER_WORKGROUP_SIZE);
        let dispatch_y = push.height.div_ceil(TONE_MAPPER_WORKGROUP_SIZE);

        let mut recorder = RhiCommandRecorder::new(&self.vulkan_device, "tone_curve_apply")?;
        recorder.begin()?;
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
        recorder.record_image_barrier(
            src,
            VulkanLayout::GENERAL,
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            VulkanStage::COMPUTE_SHADER,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::SHADER_READ,
            VulkanAccess::SHADER_SAMPLED_READ,
        )?;
        recorder.record_image_barrier(
            dst,
            VulkanLayout::GENERAL,
            VulkanLayout::SHADER_READ_ONLY_OPTIMAL,
            VulkanStage::COMPUTE_SHADER,
            VulkanStage::ALL_COMMANDS,
            VulkanAccess::SHADER_WRITE,
            VulkanAccess::SHADER_SAMPLED_READ,
        )?;
        recorder.submit_and_wait()?;
        Ok(())
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
    use crate::tone::bt2390_eetf_per_channel;
    use crate::tone_mapper::ToneCurveId;
    use crate::transfer::{linear_to_pq, linear_to_srgb, pq_to_linear, srgb_to_linear, TransferId};
    use streamlib::sdk::engine::host_rhi::{
        HostTextureExt, HostVulkanBuffer, HostVulkanTexture, ImageCopyRegion,
    };
    use streamlib::sdk::rhi::{
        StorageBuffer, TextureDescriptor, TextureFormat, TextureReadbackDescriptor,
        TextureSourceLayout, TextureUsages,
    };
    use streamlib::sdk::engine::host_rhi::VulkanTextureReadback;

    fn try_vulkan_device() -> Option<Arc<HostVulkanDevice>> {
        HostVulkanDevice::new().ok()
    }

    #[test]
    fn new_is_cheap_and_lazy() {
        let Some(device) = try_vulkan_device() else { return };
        let mapper = VulkanToneMapper::new(&device);
        assert!(mapper.kernel.lock().is_none());
    }

    /// Bake `pattern_bgra(x, y)` into a fresh `STORAGE_BINDING |
    /// COPY_SRC | COPY_DST` BGRA8 texture, leaving it in
    /// `VulkanLayout::GENERAL`. Uses only the public SDK surface —
    /// no direct `vulkanalia` calls — so this helper builds inside
    /// the packaged carve-out without the boundary check.
    fn make_general_texture(
        device: &Arc<HostVulkanDevice>,
        width: u32,
        height: u32,
        pattern_bgra: impl Fn(u32, u32) -> [u8; 4],
    ) -> Texture {
        let bpp: u32 = 4;
        let host_staging = HostVulkanBuffer::new_storage_buffer_host_visible(
            device,
            (width as u64) * (height as u64) * (bpp as u64),
        )
        .expect("staging");
        unsafe {
            let mut p = host_staging.mapped_ptr();
            for y in 0..height {
                for x in 0..width {
                    let px = pattern_bgra(x, y);
                    std::ptr::copy_nonoverlapping(px.as_ptr(), p, 4);
                    p = p.add(4);
                }
            }
        }
        let staging = StorageBuffer::from_host_vulkan_buffer(Arc::new(host_staging));
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
        let texture = <Texture as HostTextureExt>::from_vulkan(host_tex);

        let mut recorder =
            RhiCommandRecorder::new(device, "tone-mapper-test-fill").expect("recorder");
        recorder.begin().expect("begin");
        recorder
            .record_image_barrier(
                &texture,
                VulkanLayout::UNDEFINED,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                VulkanStage::ALL_COMMANDS,
                VulkanStage::COPY,
                VulkanAccess::NONE,
                VulkanAccess::TRANSFER_WRITE,
            )
            .expect("barrier to dst");
        recorder
            .record_copy_buffer_to_image(
                &staging,
                &texture,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                ImageCopyRegion::tightly_packed(width, height),
            )
            .expect("copy");
        recorder
            .record_image_barrier(
                &texture,
                VulkanLayout::TRANSFER_DST_OPTIMAL,
                VulkanLayout::GENERAL,
                VulkanStage::COPY,
                VulkanStage::ALL_COMMANDS,
                VulkanAccess::TRANSFER_WRITE,
                VulkanAccess::MEMORY_READ,
            )
            .expect("barrier to general");
        recorder.submit_and_wait().expect("submit");
        texture
    }

    fn make_dst_texture(
        device: &Arc<HostVulkanDevice>,
        width: u32,
        height: u32,
    ) -> Texture {
        let desc = TextureDescriptor {
            width,
            height,
            format: TextureFormat::Bgra8Unorm,
            usage: TextureUsages::COPY_SRC
                | TextureUsages::COPY_DST
                | TextureUsages::STORAGE_BINDING,
            label: Some("tone-mapper-test-output"),
        };
        let host_tex = HostVulkanTexture::new(device, &desc).expect("texture");
        <Texture as HostTextureExt>::from_vulkan(host_tex)
    }

    fn pq_pixel(linear_norm_0_to_1: f32) -> [u8; 4] {
        let abs_lin = (linear_norm_0_to_1 * 1000.0) / 10_000.0;
        let pq = linear_to_pq(abs_lin).clamp(0.0, 1.0);
        let byte = (pq * 255.0).round() as u8;
        [byte, byte, byte, 0xFF]
    }

    /// GPU dispatch parity test for the BT.2390 EETF kernel path.
    /// Mentally revert the GLSL Hermite spline → GPU output diverges
    /// from the CPU reference everywhere above the knee.
    #[test]
    #[ignore = "hardware integration — requires a working Vulkan device + queue"]
    fn bt2390_pq_to_srgb_matches_cpu_reference() {
        let Some(device) = try_vulkan_device() else { return };
        let width = 16u32;
        let height = 16u32;
        let pin_nits = 1000.0_f32;
        let pout_nits = 100.0_f32;

        let input_linear_for = |x: u32, y: u32| -> f32 {
            let idx = (y * width + x) as f32;
            idx / (width * height - 1) as f32
        };

        let src = make_general_texture(&device, width, height, |x, y| {
            pq_pixel(input_linear_for(x, y))
        });
        let dst = make_dst_texture(&device, width, height);

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

        mapper
            .apply_with_layouts(
                &src,
                VulkanLayout::GENERAL,
                &dst,
                VulkanLayout::UNDEFINED,
                &push,
            )
            .expect("apply_with_layouts");

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
            .submit(&dst, TextureSourceLayout::ShaderReadOnly)
            .expect("submit");
        let bytes = readback.wait_and_read(ticket, u64::MAX).expect("read");

        for y in 0..height {
            for x in 0..width {
                let input_lin_norm = input_linear_for(x, y);
                let pq_in = (input_lin_norm * pin_nits / 10_000.0).clamp(0.0, 1.0);
                let pq_encoded = linear_to_pq(pq_in);
                let lin_in = pq_to_linear(pq_encoded) * 10_000.0 / pin_nits;
                let lin_tone = bt2390_eetf_per_channel(lin_in, pin_nits, pout_nits);
                let srgb_out = linear_to_srgb(lin_tone.clamp(0.0, 1.0));
                let expected_byte = (srgb_out * 255.0).round() as i32;

                let off = ((y * width + x) * 4) as usize;
                for ch in 0..3 {
                    let actual = bytes[off + ch] as i32;
                    let delta = (actual - expected_byte).abs();
                    assert!(
                        delta <= 2,
                        "pixel ({x},{y}) ch {ch}: GPU={actual} expected={expected_byte} (delta {delta})"
                    );
                }
                assert_eq!(bytes[off + 3], 0xFF, "alpha pass-through broken at ({x},{y})");
            }
        }
    }

    #[test]
    fn pq_pixel_at_peak_matches_pq_encoding() {
        let bytes = pq_pixel(1.0);
        let expected = (linear_to_pq(0.1) * 255.0).round() as u8;
        assert_eq!(bytes[0], expected);
        assert_eq!(bytes[1], expected);
        assert_eq!(bytes[2], expected);
        assert_eq!(bytes[3], 0xFF);
    }

    #[test]
    fn srgb_round_trip_at_unit_holds_to_byte_255() {
        let lin = pq_to_linear(linear_to_pq(0.1)) * 10.0;
        let srgb = linear_to_srgb(lin.clamp(0.0, 1.0));
        let byte = (srgb * 255.0).round() as i32;
        assert_eq!(byte, 255, "round-trip should land at byte 255");
        let _ = srgb_to_linear(0.5);
    }
}
