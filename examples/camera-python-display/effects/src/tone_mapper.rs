// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Sandboxed image→image tone-curve kernel — engine-free example-local
//! copy of the engine's `RhiToneMapper` (Linux only).
//!
//! ## Why this lives in the example, not the engine
//!
//! `BlendingCompositor`'s per-acquire color normalization needs a
//! transfer + tone-curve conversion, which the engine ships as
//! `RhiToneMapper` behind the FullAccess facade. The engine-free plugin
//! SDK deliberately does NOT carry a `ToneMapper` PluginAbiObject — the
//! sound, minimal shape for a sandboxed consumer is to author the kernel
//! against the already-lifted cdylib-safe primitives
//! ([`GpuContextFullAccess::create_compute_kernel`] +
//! [`GpuContextFullAccess::create_command_recorder`] +
//! [`RhiCommandRecorder::record_dispatch`]) and copy the shader
//! example-local. The 32-byte [`ToneMapperPushConstants`] block is locked
//! byte-for-byte against `shaders/tone_curve.comp`'s `layout(push_constant)`
//! by the regression test at the bottom of this file.
//!
//! ## Engine surfaces this rides
//!
//! - [`GpuContextFullAccess::create_compute_kernel`] — the two-storage-image
//!   tone-curve compute pipeline, built once at setup.
//! - [`RhiCommandRecorder`] (`record_image_barrier` + `record_dispatch` +
//!   `submit_and_wait`) — the barrier dance around one dispatch, in one
//!   engine-owned command buffer.
//!
//! Neither surface names a raw `HostVulkanDevice` or `vulkanalia` type —
//! the kernel is cdylib-safe end-to-end.

#![cfg(target_os = "linux")]

use std::sync::Mutex;

use streamlib_plugin_sdk::sdk::color::TransferId;
use streamlib_plugin_sdk::sdk::context::GpuContextFullAccess;
use streamlib_plugin_sdk::sdk::error::{Error, Result};
use streamlib_plugin_sdk::sdk::rhi::{
    ComputeBindingSpec, ComputeKernelDescriptor, RhiCommandRecorder, Texture, VulkanAccess,
    VulkanComputeKernel, VulkanLayout, VulkanStage,
};

/// Workgroup tile size. Matches `local_size_x/y` in `tone_curve.comp` so
/// dispatch dims are `⌈(width, height) / 16⌉`.
pub const TONE_MAPPER_WORKGROUP_SIZE: u32 = 16;

/// Tone-curve selector for [`ToneMapperPushConstants::tonemap_curve`].
/// Numeric values must match the `TONE_CURVE_*` constants in
/// `shaders/tone_curve.comp`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToneCurveId {
    /// Identity — pure transfer-function conversion (EOTF then OETF),
    /// rescaled by the peak ratio.
    None = 0,
    /// ITU-R BT.2390 EETF — closed-form Hermite spline for HDR→SDR tone
    /// mapping (per channel, PQ space).
    Bt2390 = 1,
    /// ITU-R BT.2446-1 method A2 inverse — closed-form gamma-knee curve
    /// for SDR→HDR up-conversion (per channel, linear space).
    Bt2446a = 2,
}

/// Push-constants struct matching `tone_curve.comp`'s
/// `layout(push_constant, std430)` block. std430 packs `u32` and `f32`
/// tightly; the struct is 32 bytes total.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ToneMapperPushConstants {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Source transfer characteristic, encoded as [`TransferId`].
    pub input_transfer: u32,
    /// Destination transfer characteristic, encoded as [`TransferId`].
    pub output_transfer: u32,
    /// Tone-curve selector, encoded as [`ToneCurveId`].
    pub tonemap_curve: u32,
    /// Source reference peak luminance in nits (100 for SDR).
    pub peak_in_nits: f32,
    /// Destination reference peak luminance in nits (100 for SDR display).
    pub peak_out_nits: f32,
    /// Reserved bits for future tone-curve extensions. Must be zero today.
    pub flags: u32,
}

impl ToneMapperPushConstants {
    /// Build push-constants for the canonical configuration. When both
    /// peaks match and `curve = ToneCurveId::None`, the kernel reduces to
    /// a pure transfer-conversion path.
    pub fn new(
        width: u32,
        height: u32,
        input_transfer: TransferId,
        output_transfer: TransferId,
        curve: ToneCurveId,
        peak_in_nits: f32,
        peak_out_nits: f32,
    ) -> Self {
        Self {
            width,
            height,
            input_transfer: input_transfer as u32,
            output_transfer: output_transfer as u32,
            tonemap_curve: curve as u32,
            peak_in_nits,
            peak_out_nits,
            flags: 0,
        }
    }
}

/// Byte size of the push-constants block. Must match the
/// `layout(push_constant)` size in `shaders/tone_curve.comp`.
pub const TONE_MAPPER_PUSH_CONSTANT_SIZE: u32 =
    std::mem::size_of::<ToneMapperPushConstants>() as u32;

const IMAGE_TO_IMAGE_BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::storage_image(0), // input  (readonly  in shader)
    ComputeBindingSpec::storage_image(1), // output (writeonly in shader)
];

/// Compiled tone-curve compute SPIR-V (emitted by `build.rs` via `glslc`).
const TONE_CURVE_SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/tone_curve.comp.spv"));

/// Sandboxed image→image tone-curve primitive built on the engine-free
/// plugin SDK. The compute kernel + command recorder are built once from a
/// privileged [`GpuContextFullAccess`] at setup; per-frame dispatch runs
/// through the owned recorder (scope-free — no escalate on the hot path).
pub struct SandboxedToneMapper {
    label: &'static str,
    kernel: VulkanComputeKernel,
    /// Reused across dispatches for the pre/post barrier dance +
    /// `record_dispatch`. Single-owner (`&mut self` methods), so guarded
    /// by a Mutex for the shared struct.
    recorder: Mutex<RhiCommandRecorder>,
}

impl SandboxedToneMapper {
    /// Build the tone-curve kernel + recorder from a privileged
    /// [`GpuContextFullAccess`] (setup-time only).
    pub fn new(full: &GpuContextFullAccess) -> Result<Self> {
        let label = "tone_curve_image_to_image";
        let kernel = full.create_compute_kernel(&ComputeKernelDescriptor {
            label,
            spv: TONE_CURVE_SPV,
            bindings: IMAGE_TO_IMAGE_BINDINGS,
            push_constant_size: TONE_MAPPER_PUSH_CONSTANT_SIZE,
        })?;
        let recorder = full.create_command_recorder("tone_curve_recorder")?;
        Ok(Self {
            label,
            kernel,
            recorder: Mutex::new(recorder),
        })
    }

    /// Apply the tone curve from `src` into `dst` with caller-declared
    /// current layouts, recording the `→ GENERAL` pre-barriers + dispatch +
    /// `→ SHADER_READ_ONLY_OPTIMAL` post-barriers in one engine-owned
    /// command buffer; submits and waits before returning. Both `src` and
    /// `dst` are left in [`VulkanLayout::SHADER_READ_ONLY_OPTIMAL`] on
    /// success.
    ///
    /// Caller contract: `src` and `dst` must reference distinct textures —
    /// in-place tone-map is not supported (the kernel binds them as two
    /// storage images and the barrier sequence would emit conflicting
    /// layout claims on the same image). `BlendingCompositor::normalize_layer`
    /// enforces this by short-circuiting an already-normalized layer, so
    /// `src` (an upstream layer) and `dst` (a per-port pooled intermediate)
    /// are always distinct here.
    pub fn apply_with_layouts(
        &self,
        src: &Texture,
        src_current_layout: VulkanLayout,
        dst: &Texture,
        dst_current_layout: VulkanLayout,
        push: &ToneMapperPushConstants,
    ) -> Result<()> {
        // Bind src + dst as the two storage images + stage push-constants.
        self.kernel.set_storage_image(0, src)?;
        self.kernel.set_storage_image(1, dst)?;
        self.kernel.set_push_constants_value(push)?;

        let dispatch_x = push.width.div_ceil(TONE_MAPPER_WORKGROUP_SIZE);
        let dispatch_y = push.height.div_ceil(TONE_MAPPER_WORKGROUP_SIZE);

        let mut recorder = self.recorder.lock().map_err(|e| {
            Error::GpuError(format!("{}: recorder mutex poisoned: {e}", self.label))
        })?;
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
        recorder.record_dispatch(&self.kernel, dispatch_x, dispatch_y, 1)?;
        // Post-barriers: leave both in SHADER_READ_ONLY_OPTIMAL — the
        // canonical "ready for the next consumer to sample" state.
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
}

impl std::fmt::Debug for SandboxedToneMapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxedToneMapper").finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Push-constants size locks the cross-language contract with
    /// `shaders/tone_curve.comp`. If the struct changes, the shader's
    /// `layout(push_constant)` block must change in lock-step (and the
    /// SPIR-V regenerated). Mentally revert any field add/remove and this
    /// test catches the drift before it reaches a live dispatch.
    #[test]
    fn push_constants_size_is_32_bytes() {
        assert_eq!(
            std::mem::size_of::<ToneMapperPushConstants>(),
            32,
            "ToneMapperPushConstants layout drifted — update tone_curve.comp \
             push_constant block before regenerating SPIR-V"
        );
        assert_eq!(TONE_MAPPER_PUSH_CONSTANT_SIZE, 32);
    }

    /// Discriminator values must match the GLSL `TONE_CURVE_*` constants in
    /// `shaders/tone_curve.comp`. If they disagree, the shader silently
    /// picks a different curve or no-ops the dispatch.
    #[test]
    fn tone_curve_id_values_match_shader() {
        assert_eq!(ToneCurveId::None as u32, 0);
        assert_eq!(ToneCurveId::Bt2390 as u32, 1);
        assert_eq!(ToneCurveId::Bt2446a as u32, 2);
    }

    /// `TransferId` discriminants must match the `TRANSFER_*` constants in
    /// `shaders/color_convert_common.glsl`, which `tone_curve.comp` includes.
    /// `ToneMapperPushConstants.input_transfer` / `output_transfer` carry
    /// `TransferId as u32` and the shader dispatches on the raw numeric id, so
    /// a drift here silently selects the wrong transfer curve (no error, no
    /// panic). `TransferId` lives in a different crate (`streamlib-plugin-sdk`)
    /// from this example-local shader, so this lock pins the cross-crate
    /// Rust↔GLSL contract. Mentally revert any renumber and this catches it.
    #[test]
    fn transfer_id_values_match_shader() {
        // Mirrors: TRANSFER_LINEAR=0u, TRANSFER_SRGB=1u, TRANSFER_BT709=2u,
        //          TRANSFER_PQ=3u, TRANSFER_HLG=4u (color_convert_common.glsl).
        assert_eq!(TransferId::Linear as u32, 0);
        assert_eq!(TransferId::Srgb as u32, 1);
        assert_eq!(TransferId::Bt709 as u32, 2);
        assert_eq!(TransferId::Pq as u32, 3);
        assert_eq!(TransferId::Hlg as u32, 4);
    }

    /// The convenience constructor populates fields in the canonical
    /// HDR→SDR config and encodes the `TransferId` / `ToneCurveId` enums to
    /// their `#[repr(u32)]` discriminants.
    #[test]
    fn new_populates_hdr_to_sdr_canonical_values() {
        let pc = ToneMapperPushConstants::new(
            1920,
            1080,
            TransferId::Pq,
            TransferId::Srgb,
            ToneCurveId::Bt2390,
            1000.0,
            100.0,
        );
        assert_eq!(pc.width, 1920);
        assert_eq!(pc.height, 1080);
        assert_eq!(pc.input_transfer, TransferId::Pq as u32);
        assert_eq!(pc.output_transfer, TransferId::Srgb as u32);
        assert_eq!(pc.tonemap_curve, ToneCurveId::Bt2390 as u32);
        assert_eq!(pc.peak_in_nits, 1000.0);
        assert_eq!(pc.peak_out_nits, 100.0);
        assert_eq!(pc.flags, 0);
    }
}
