// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-owned tone-curve primitive — image→image compute kernel that
//! consumes [`TransferId`] + [`ToneCurveId`] + reference peak luminance
//! as push-constant state.
//!
//! Sibling to [`crate::core::rhi::RhiColorConverter`]: where the
//! converter handles buffer→image YCbCr→RGB matrix + transfer
//! conversion, the tone mapper handles image→image transfer + tone-curve
//! conversion. Together they cover the full color-management pipeline:
//! a YUV camera frame goes through the converter (NV12 → sRGB RGBA),
//! and an HDR PQ frame goes through the tone mapper (PQ → sRGB RGBA
//! with BT.2390 EETF).
//!
//! Consumers (the [`BlendingCompositor`], the display, encoders
//! targeting cross-color-space output) hold an `Arc<RhiToneMapper>`
//! as a struct field — same shape as `LinuxCameraProcessor` holds
//! `Arc<RhiColorConverter>` per `packages/camera/src/linux/camera.rs`.
//!
//! [`BlendingCompositor`]: ../../../examples/camera-python-display/src/blending_compositor.rs

use crate::core::color::TransferId;

#[cfg(target_os = "linux")]
use crate::core::rhi::Texture;
#[cfg(target_os = "linux")]
use crate::core::Result;

/// Tone-curve selector for [`ToneMapperPushConstants::tonemap_curve`].
/// Numeric values must match the `TONE_CURVE_*` constants in
/// `vulkan/rhi/shaders/tone_curve.comp`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToneCurveId {
    /// Identity — pure transfer-function conversion (EOTF then OETF),
    /// rescaled by the peak ratio. Use when the source and destination
    /// share peak luminance and only the encoding needs to change
    /// (e.g., sRGB → linear, BT.709 → sRGB).
    None = 0,
    /// ITU-R BT.2390 EETF — closed-form Hermite spline for HDR→SDR
    /// tone mapping. Operates per channel in PQ space; matches
    /// `--tone-mapping=auto` legacy default in mpv and FFmpeg's
    /// `tonemap=bt2390` filter.
    Bt2390 = 1,
    /// ITU-R BT.2446-1 method A2 inverse — closed-form gamma-knee
    /// curve for SDR→HDR up-conversion. Operates per channel in
    /// linear space.
    Bt2446a = 2,
}

/// Push-constants struct matching the tone-mapper shader's
/// `layout(push_constant, std430)` block.
///
/// std430 packs `u32` and `f32` tightly; the struct is 32 bytes total
/// (well under the spec minimum 128).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ToneMapperPushConstants {
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Source transfer characteristic (encoded in the input image),
    /// encoded as [`TransferId`].
    pub input_transfer: u32,
    /// Destination transfer characteristic (encoded in the output
    /// image), encoded as [`TransferId`].
    pub output_transfer: u32,
    /// Tone-curve selector, encoded as [`ToneCurveId`].
    pub tonemap_curve: u32,
    /// Source reference peak luminance in nits. For HDR10 PQ
    /// content this is the master display peak (commonly
    /// 1000 / 4000 / 10000); for SDR sources it is 100 nits.
    pub peak_in_nits: f32,
    /// Destination reference peak luminance in nits. For SDR display
    /// targets this is 100 nits; for HDR display targets it matches
    /// the negotiated display peak (typically 1000 nits for HDR10).
    pub peak_out_nits: f32,
    /// Reserved bits for future tone-curve extensions (gamut
    /// compression, hue-preservation flags, scene-adaptive metadata
    /// inputs). Must be zero today.
    pub flags: u32,
}

impl ToneMapperPushConstants {
    /// Build push-constants for the canonical configuration.
    ///
    /// The tone-curve discriminator picks the math; the peak-luminance
    /// pair sets the source/destination reference. When both peaks
    /// match and `curve = ToneCurveId::None`, the kernel reduces to a
    /// pure transfer-conversion path with no math beyond EOTF + OETF.
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

/// Byte size of the push-constants block sent to the tone-mapper kernel.
/// Must match the `layout(push_constant)` size in
/// `vulkan/rhi/shaders/tone_curve.comp`.
pub const TONE_MAPPER_PUSH_CONSTANT_SIZE: u32 =
    std::mem::size_of::<ToneMapperPushConstants>() as u32;

/// Engine-owned image→image tone-curve primitive.
///
/// Created via [`crate::core::context::GpuContext::tone_mapper`] and
/// cached for the lifetime of the [`crate::core::context::GpuContext`].
/// The kernel itself is stateless beyond push constants — one cached
/// instance handles every variation of `(input_transfer, output_transfer,
/// curve, peak_in, peak_out)` without invalidating the pipeline.
///
/// Thread-safe — internal compute-kernel submissions serialize through
/// the host queue mutex.
pub struct RhiToneMapper {
    #[cfg(target_os = "linux")]
    pub(crate) inner: crate::vulkan::rhi::VulkanToneMapper,

    #[cfg(not(target_os = "linux"))]
    _marker: std::marker::PhantomData<()>,
}

impl RhiToneMapper {
    /// Bind `(src, dst)` + push-constants on the kernel and return it
    /// for recorder-driven dispatch. Use when the caller already has an
    /// [`crate::vulkan::rhi::RhiCommandRecorder`] and wants the tone
    /// curve to nest inside its own barriers rather than spawning a
    /// separate queue submit.
    #[cfg(target_os = "linux")]
    pub fn prepare(
        &self,
        src: &Texture,
        dst: &Texture,
        push: &ToneMapperPushConstants,
    ) -> Result<std::sync::Arc<crate::vulkan::rhi::VulkanComputeKernel>> {
        self.inner.prepare(src, dst, push)
    }

    /// Apply the tone curve to `src` into `dst` end-to-end. Builds the
    /// kernel (if needed), binds, dispatches via its own command buffer
    /// + fence + queue submit, and waits before returning. Caller is
    /// responsible for ensuring `src` and `dst` are in
    /// [`crate::core::rhi::VulkanLayout::GENERAL`] (the storage-image
    /// binding requirement). For consumers that need layout transitions
    /// handled, prefer [`Self::apply_with_layouts`].
    #[cfg(target_os = "linux")]
    pub fn apply(
        &self,
        src: &Texture,
        dst: &Texture,
        push: &ToneMapperPushConstants,
    ) -> Result<()> {
        self.inner.apply(src, dst, push)
    }

    /// Apply the tone curve with caller-declared current layouts. The
    /// kernel records pre-barriers (`→ GENERAL`) + dispatch +
    /// post-barriers (`→ SHADER_READ_ONLY_OPTIMAL`) in one
    /// engine-owned command buffer; submits and waits before returning.
    /// Both `src` and `dst` are left in `SHADER_READ_ONLY_OPTIMAL` on
    /// success.
    #[cfg(target_os = "linux")]
    pub fn apply_with_layouts(
        &self,
        src: &Texture,
        src_current_layout: crate::core::rhi::VulkanLayout,
        dst: &Texture,
        dst_current_layout: crate::core::rhi::VulkanLayout,
        push: &ToneMapperPushConstants,
    ) -> Result<()> {
        self.inner
            .apply_with_layouts(src, src_current_layout, dst, dst_current_layout, push)
    }
}

impl std::fmt::Debug for RhiToneMapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RhiToneMapper").finish()
    }
}

// Compute-kernel submissions serialize through the host queue mutex.
unsafe impl Send for RhiToneMapper {}
unsafe impl Sync for RhiToneMapper {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Push-constants size locks the cross-language contract with the
    /// shader. If the struct changes, the shader's
    /// `layout(push_constant)` size must change in lock-step.
    #[test]
    fn push_constants_size_is_32_bytes() {
        assert_eq!(
            std::mem::size_of::<ToneMapperPushConstants>(),
            32,
            "ToneMapperPushConstants layout drifted — update the shader's \
             push_constant block before regenerating SPIR-V"
        );
    }

    /// Discriminator values must match the GLSL `TONE_CURVE_*` constants
    /// in `vulkan/rhi/shaders/tone_curve.comp`. If these disagree, the
    /// shader silently picks a different curve or no-ops the dispatch.
    #[test]
    fn tone_curve_id_values_match_shader() {
        assert_eq!(ToneCurveId::None as u32, 0);
        assert_eq!(ToneCurveId::Bt2390 as u32, 1);
        assert_eq!(ToneCurveId::Bt2446a as u32, 2);
    }

    /// Convenience constructor populates fields in the canonical
    /// (PQ, Srgb, Bt2390, 1000nit→100nit) HDR→SDR config.
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
