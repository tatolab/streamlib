// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Fused JPEG decode compute kernel: dequantize, 8x8 IDCT, 4:2:0 chroma
//! upsample, BT.601 full-range YCbCr -> RGB, write `rgba8` storage image.
//!
//! Built through the cdylib-safe RHI surface
//! ([`GpuContextFullAccess`]) per `docs/architecture/compute-kernel.md`:
//! the kernel is constructed via `create_compute_kernel` and its storage
//! buffers via `acquire_storage_buffer`, so this package never touches the
//! raw `HostVulkanDevice` and stays sound when built as a separately-
//! compiled `.slpkg` plugin (see
//! `docs/learnings/slpkg-raw-device-rhi-construction.md`). Bindings are
//! declared as data; SPIR-V reflection validates the layout at kernel
//! construction. No raw `vulkanalia` calls; no hand-rolled descriptor
//! sets, pipeline layouts, or command buffers.

use streamlib::sdk::color::{ResolvedColorInfo, TransferId};
use streamlib::sdk::context::GpuContextFullAccess;
use streamlib::sdk::engine::host_rhi::VulkanComputeKernel;
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::rhi::{
    ColorConverterPushConstants, ComputeBindingSpec, ComputeKernelDescriptor, SourceLayoutInfo,
    StorageBuffer, Texture,
};

use crate::header::DecodedJpeg;

/// Compute-shader workgroup tile size (one thread per output pixel, 16x16
/// workgroups). Mirrors the engine color-converter shape.
pub const JPEG_DECODE_WORKGROUP_SIZE: u32 = 16;

/// JPEG component positions in scan order: every JFIF-compliant encoder
/// writes Y first, Cb second, Cr third regardless of the numeric
/// `component_id` it assigns (the spec doesn't mandate the ids — JFIF
/// section A.1 says 1/2/3, but libjpeg, jpeg-encoder, mozjpeg, etc. use
/// either 0/1/2 or 1/2/3). Trusting scan order is the portable shape.
const Y_POSITION: usize = 0;
const CB_POSITION: usize = 1;
const CR_POSITION: usize = 2;

/// 4:2:0 horizontal/vertical sampling factor for Y; chroma is half-rate.
const Y_SAMPLING_420: u8 = 2;
const CHROMA_SAMPLING_420: u8 = 1;

const BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::storage_buffer(0), // coefficients (i32 sign-extended from i16)
    ComputeBindingSpec::storage_buffer(1), // quant tables  (u32 zero-extended from u16)
    ComputeBindingSpec::storage_image(2),  // rgba8 output
];

/// Push constants matching the GLSL `PushConstants` block.
///
/// `std430` layout: nine JPEG-geometry `uint`s, three `_pad` slots to
/// align the first `vec4` to a 16-byte boundary, then the same matrix
/// `+` range_offset `+` transfer/flags shape as
/// [`ColorConverterPushConstants`]. Total 128 bytes — at Vulkan's
/// spec-minimum push-constant range.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct JpegDecodePushConstants {
    width: u32,
    height: u32,
    y_blocks_h: u32,
    y_blocks_v: u32,
    chroma_blocks_h: u32,
    chroma_blocks_v: u32,
    cb_coef_offset: u32,
    cr_coef_offset: u32,
    chroma_qtable_offset: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    matrix_row0: [f32; 4],
    matrix_row1: [f32; 4],
    matrix_row2: [f32; 4],
    range_offset: [f32; 4],
    transfer_in: u32,
    transfer_out: u32,
    flags: u32,
    _pad3: u32,
}

const PUSH_CONSTANT_SIZE: u32 = std::mem::size_of::<JpegDecodePushConstants>() as u32;

/// Compile-time check that the push-constant block matches the shader's
/// 128-byte `layout(push_constant)` range.
const _: () = assert!(
    PUSH_CONSTANT_SIZE as usize == 128,
    "JpegDecodePushConstants must stay at 128 bytes — update the shader's \
     push_constant block before regenerating SPIR-V"
);

/// Fused JPEG decode kernel.
pub struct JpegDecodeKernel {
    kernel: VulkanComputeKernel,
}

impl JpegDecodeKernel {
    /// Build the kernel through the FullAccess `create_compute_kernel`
    /// primitive — the host builds the kernel on its own device and hands
    /// back a cdylib-safe `#[repr(C)]` handle. Loads SPIR-V, runs
    /// reflection, validates the declared bindings match the shader,
    /// allocates the Vulkan pipeline + descriptor set + command buffer +
    /// fence host-side. Never reaches the raw `HostVulkanDevice`, so it is
    /// sound from a separately-built `.slpkg` plugin.
    pub fn new(full_access: &GpuContextFullAccess) -> Result<Self> {
        let spv: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/jpeg_decode.spv"));
        let kernel = full_access.create_compute_kernel(&ComputeKernelDescriptor {
            label: "jpeg_decode",
            spv,
            bindings: BINDINGS,
            push_constant_size: PUSH_CONSTANT_SIZE,
        })?;
        Ok(Self { kernel })
    }

    /// Decode `decoded` into `output_texture`. The texture must be an
    /// `Rgba8Unorm` storage image with `STORAGE_BINDING` usage, sized
    /// `decoded.frame.width x decoded.frame.height`, and already
    /// transitioned to `GENERAL` layout (caller's responsibility — same
    /// contract as every other `VulkanComputeKernel` consumer in the
    /// engine).
    ///
    /// `color_info` drives the YCbCr → RGB matrix / range_offset /
    /// transfer push constants — typically resolved from
    /// `decoded.color_info.resolve()`; pass the JFIF default
    /// (`(Bt709, Srgb, Smpte170m, Full)`) when the source is known to
    /// be canonical JFIF.
    ///
    /// Allocates per-call HOST_VISIBLE storage buffers for the coefficient
    /// and quant-table uploads, copies the data in, binds, dispatches, and
    /// waits on the kernel's fence before returning. This is the one-shot
    /// convenience for callers without a pre-allocated buffer pool (e.g.
    /// the kernel-level integration test); steady-state hot-path callers
    /// should ride [`Self::dispatch_pooled`] through
    /// [`crate::SimpleJpegDecoder`] instead.
    pub fn dispatch(
        &self,
        full_access: &GpuContextFullAccess,
        decoded: &DecodedJpeg,
        output_texture: &Texture,
        color_info: &ResolvedColorInfo,
    ) -> Result<()> {
        let layout = JpegBufferLayout::from_decoded(decoded)?;
        let coef_words = layout.pack_coefficients(decoded);
        let qt_words = layout.pack_quant_tables(decoded)?;

        let coef_bytes = bytemuck::cast_slice::<i32, u8>(&coef_words);
        let qt_bytes = bytemuck::cast_slice::<u32, u8>(&qt_words);

        let coef_buf = full_access.acquire_storage_buffer(coef_bytes.len() as u64)?;
        let qt_buf = full_access.acquire_storage_buffer(qt_bytes.len() as u64)?;

        self.dispatch_with_buffers(
            decoded,
            &layout,
            output_texture,
            &coef_buf,
            &qt_buf,
            color_info,
        )
    }

    /// Decode `decoded` into `output_texture` using caller-supplied
    /// pre-allocated HOST_VISIBLE SSBOs. Steady-state hot-path entrypoint:
    /// no `vkAllocateMemory`, no `vkCreateBuffer`, no `vkMapMemory`.
    ///
    /// `coef_buf` and `qt_buf` must be HOST_VISIBLE storage buffers
    /// (acquired via `GpuContextFullAccess::acquire_storage_buffer`) sized
    /// to fit the largest decode the caller intends to run — typically the
    /// worst-case 4:2:0 byte count for the decoder's `(max_width,
    /// max_height)`. The kernel checks the sizes against `decoded` and
    /// returns a typed error if too small, so undersized buffers can't
    /// silently corrupt output.
    ///
    /// `color_info` semantics match [`Self::dispatch`].
    ///
    /// Same per-call texture contract as [`Self::dispatch`]: `output_texture`
    /// must be an `Rgba8Unorm` storage image in `GENERAL` layout.
    pub fn dispatch_pooled(
        &self,
        decoded: &DecodedJpeg,
        output_texture: &Texture,
        coef_buf: &StorageBuffer,
        qt_buf: &StorageBuffer,
        color_info: &ResolvedColorInfo,
    ) -> Result<()> {
        let layout = JpegBufferLayout::from_decoded(decoded)?;
        self.dispatch_with_buffers(
            decoded,
            &layout,
            output_texture,
            coef_buf,
            qt_buf,
            color_info,
        )
    }

    /// Shared dispatch tail: pack coefficients + quant tables into the
    /// supplied HOST_VISIBLE buffers, bind, set push constants, dispatch.
    /// Validates that the supplied buffers are large enough so a too-small
    /// pool surfaces as a typed error instead of an out-of-bounds memcpy.
    fn dispatch_with_buffers(
        &self,
        decoded: &DecodedJpeg,
        layout: &JpegBufferLayout,
        output_texture: &Texture,
        coef_buf: &StorageBuffer,
        qt_buf: &StorageBuffer,
        color_info: &ResolvedColorInfo,
    ) -> Result<()> {
        // Pack i16 coefficients into i32 SSBO words. Sign-extension is
        // implicit in `i32::from(i16)`. Quant tables go u16 -> u32 with
        // zero extension. Packing on the host keeps the shader portable
        // (no dependency on `VK_KHR_16bit_storage` / `shaderInt16`).
        let coef_words = layout.pack_coefficients(decoded);
        let qt_words = layout.pack_quant_tables(decoded)?;

        let coef_bytes = bytemuck::cast_slice::<i32, u8>(&coef_words);
        let qt_bytes = bytemuck::cast_slice::<u32, u8>(&qt_words);

        if (coef_bytes.len() as u64) > coef_buf.byte_size() {
            return Err(Error::GpuError(format!(
                "jpeg_decode: coefficient buffer too small — need {} bytes, pool sized for {} \
                 (rebuild SimpleJpegDecoder with larger max_width/max_height)",
                coef_bytes.len(),
                coef_buf.byte_size(),
            )));
        }
        if (qt_bytes.len() as u64) > qt_buf.byte_size() {
            return Err(Error::GpuError(format!(
                "jpeg_decode: quant-table buffer too small — need {} bytes, pool sized for {}",
                qt_bytes.len(),
                qt_buf.byte_size(),
            )));
        }

        // `acquire_storage_buffer` returns HOST_VISIBLE | HOST_COHERENT
        // storage. A null mapped pointer would mean a non-host-visible
        // allocation slipped through — fail loudly instead of memcpy-ing
        // through null.
        let coef_ptr = coef_buf.mapped_ptr();
        let qt_ptr = qt_buf.mapped_ptr();
        if coef_ptr.is_null() || qt_ptr.is_null() {
            return Err(Error::GpuError(
                "jpeg_decode: storage buffer is not HOST_VISIBLE (null mapped pointer)".into(),
            ));
        }

        // SAFETY: size-checked above; mapped pointers non-null (checked)
        // and valid for the full allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(coef_bytes.as_ptr(), coef_ptr, coef_bytes.len());
            std::ptr::copy_nonoverlapping(qt_bytes.as_ptr(), qt_ptr, qt_bytes.len());
        }

        self.kernel.set_storage_buffer_storage(0, coef_buf)?;
        self.kernel.set_storage_buffer_storage(1, qt_buf)?;
        self.kernel.set_storage_image(2, output_texture)?;

        // Build the color-side push constants via the engine helper so
        // the matrix + range_offset + transfer math is sourced from one
        // place (the JPEG kernel and the engine color converter end up
        // with bit-identical fields when fed the same `ResolvedColorInfo`).
        //
        // `dst_transfer = TransferId::Srgb` matches the kernel's
        // `Rgba8Unorm` output texture by convention — 8-bit unorm
        // displays are sRGB-encoded. Every resolution path today
        // (JFIF / Adobe / EXIF sRGB / fallback) returns
        // `info.transfer = Srgb` too, so the shader bypasses the
        // transfer-function chain via `transfer_in == transfer_out`.
        // Once non-sRGB transfer characteristics start being honored
        // (e.g. ICC-profile-driven `Bt709`), the shader will start
        // running the EOTF/OETF closed-forms automatically — and this
        // hardcoded `Srgb` output curve becomes the place to revisit
        // if the JPEG kernel's downstream consumer ever wants linear
        // or HDR output instead.
        //
        // `SourceLayoutInfo` carries plane-stride metadata that the JPEG
        // kernel ignores (it walks coefficients via its own offsets) —
        // pass tight strides for the resolved dimensions; the kernel
        // never reads these slots.
        let width = u32::from(decoded.frame.width);
        let height = u32::from(decoded.frame.height);
        let color_pc = ColorConverterPushConstants::from_resolved(
            color_info,
            TransferId::Srgb,
            width,
            height,
            SourceLayoutInfo::nv12_tight(width, height),
        );

        let push = JpegDecodePushConstants {
            width,
            height,
            y_blocks_h: layout.y_blocks_h,
            y_blocks_v: layout.y_blocks_v,
            chroma_blocks_h: layout.chroma_blocks_h,
            chroma_blocks_v: layout.chroma_blocks_v,
            cb_coef_offset: layout.cb_coef_offset,
            cr_coef_offset: layout.cr_coef_offset,
            chroma_qtable_offset: layout.chroma_qtable_offset,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
            matrix_row0: color_pc.matrix_row0,
            matrix_row1: color_pc.matrix_row1,
            matrix_row2: color_pc.matrix_row2,
            range_offset: color_pc.range_offset,
            transfer_in: color_pc.transfer_in,
            transfer_out: color_pc.transfer_out,
            flags: color_pc.flags,
            _pad3: 0,
        };
        self.kernel.set_push_constants_value(&push)?;

        let dispatch_x = u32::from(decoded.frame.width).div_ceil(JPEG_DECODE_WORKGROUP_SIZE);
        let dispatch_y = u32::from(decoded.frame.height).div_ceil(JPEG_DECODE_WORKGROUP_SIZE);
        self.kernel.dispatch(dispatch_x, dispatch_y, 1)
    }
}

/// Worst-case byte size for the coefficient HOST_VISIBLE SSBO when
/// decoding any 4:2:0 frame up to `(max_width, max_height)` pixels.
///
/// Dimensions are padded up to a 16-pixel MCU boundary (4:2:0's max
/// sampling factor is 2×2, so the MCU is 16×16). Within each MCU the
/// kernel packs `i16` coefficients into `i32` SSBO words: 4 Y blocks
/// (256 coefficients) + 1 Cb block (64) + 1 Cr block (64), times 4
/// bytes/coefficient → 1536 bytes per MCU.
pub fn worst_case_coefficient_buffer_bytes_420(max_width: u32, max_height: u32) -> u64 {
    const MCU_SIDE: u64 = 16;
    const COEFFICIENTS_PER_MCU_420: u64 = 4 * 64 + 64 + 64; // Y(2×2) + Cb + Cr blocks
    const BYTES_PER_COEFFICIENT: u64 = 4; // i32-packed
    let padded_w = u64::from(max_width).div_ceil(MCU_SIDE);
    let padded_h = u64::from(max_height).div_ceil(MCU_SIDE);
    padded_w * padded_h * COEFFICIENTS_PER_MCU_420 * BYTES_PER_COEFFICIENT
}

/// Byte size for the quant-table HOST_VISIBLE SSBO. 128 entries (Y +
/// shared chroma), each packed as `u32`.
pub const QUANT_TABLE_BUFFER_BYTES: u64 = 128 * 4;

/// Indices into `DecodedJpeg.components` for each YCbCr plane plus the
/// per-plane geometry needed to lay out the SSBO uploads and push
/// constants. Computed once per dispatch from the decoded header.
#[derive(Debug, Clone, Copy)]
struct JpegBufferLayout {
    y: usize,
    cb: usize,
    cr: usize,
    y_blocks_h: u32,
    y_blocks_v: u32,
    chroma_blocks_h: u32,
    chroma_blocks_v: u32,
    /// Offsets in i32 words.
    cb_coef_offset: u32,
    cr_coef_offset: u32,
    /// Offset in u32 words; the Y quant table sits at offset 0, the
    /// shared chroma quant table follows immediately. Both occupy 64
    /// entries (always — the GPU shader walks them by zig-zag index).
    chroma_qtable_offset: u32,
}

impl JpegBufferLayout {
    fn from_decoded(decoded: &DecodedJpeg) -> Result<Self> {
        if decoded.frame.components.len() != 3 {
            return Err(Error::NotSupported(format!(
                "jpeg_decode kernel: expected 3 components (YCbCr), found {}",
                decoded.frame.components.len()
            )));
        }
        if decoded.frame.precision != 8 {
            return Err(Error::NotSupported(format!(
                "jpeg_decode kernel: only 8-bit precision supported, found {}",
                decoded.frame.precision
            )));
        }

        let y_idx = Y_POSITION;
        let cb_idx = CB_POSITION;
        let cr_idx = CR_POSITION;

        let y = &decoded.components[y_idx];
        let cb = &decoded.components[cb_idx];
        let cr = &decoded.components[cr_idx];

        if y.h_sampling != Y_SAMPLING_420 || y.v_sampling != Y_SAMPLING_420 {
            return Err(Error::NotSupported(format!(
                "jpeg_decode kernel: only 4:2:0 supported (Y sampling 2x2), \
                 got {}x{}",
                y.h_sampling, y.v_sampling
            )));
        }
        if cb.h_sampling != CHROMA_SAMPLING_420
            || cb.v_sampling != CHROMA_SAMPLING_420
            || cr.h_sampling != CHROMA_SAMPLING_420
            || cr.v_sampling != CHROMA_SAMPLING_420
        {
            return Err(Error::NotSupported(format!(
                "jpeg_decode kernel: chroma sampling must be 1x1 for 4:2:0; \
                 got Cb {}x{}, Cr {}x{}",
                cb.h_sampling, cb.v_sampling, cr.h_sampling, cr.v_sampling
            )));
        }
        if cb.blocks_horizontal != cr.blocks_horizontal
            || cb.blocks_vertical != cr.blocks_vertical
        {
            return Err(Error::GpuError(format!(
                "jpeg_decode kernel: Cb / Cr block grids must match; got Cb \
                 {}x{} vs Cr {}x{}",
                cb.blocks_horizontal,
                cb.blocks_vertical,
                cr.blocks_horizontal,
                cr.blocks_vertical
            )));
        }
        if cb.quant_table_id != cr.quant_table_id {
            return Err(Error::NotSupported(format!(
                "jpeg_decode kernel: Cb and Cr must share the same quant \
                 table id (JFIF convention); got Cb={}, Cr={}",
                cb.quant_table_id, cr.quant_table_id
            )));
        }

        let y_coefs = y.coefficients.len();
        let cb_coefs = cb.coefficients.len();
        let cr_coefs = cr.coefficients.len();
        let y_blocks_h: u32 = y.blocks_horizontal.try_into().map_err(|_| {
            Error::GpuError("jpeg_decode kernel: Y blocks_horizontal overflows u32".into())
        })?;
        let y_blocks_v: u32 = y.blocks_vertical.try_into().map_err(|_| {
            Error::GpuError("jpeg_decode kernel: Y blocks_vertical overflows u32".into())
        })?;
        let chroma_blocks_h: u32 = cb.blocks_horizontal.try_into().map_err(|_| {
            Error::GpuError(
                "jpeg_decode kernel: chroma blocks_horizontal overflows u32".into(),
            )
        })?;
        let chroma_blocks_v: u32 = cb.blocks_vertical.try_into().map_err(|_| {
            Error::GpuError(
                "jpeg_decode kernel: chroma blocks_vertical overflows u32".into(),
            )
        })?;
        let cb_coef_offset: u32 = y_coefs.try_into().map_err(|_| {
            Error::GpuError(
                "jpeg_decode kernel: Y coefficient count overflows u32 offset".into(),
            )
        })?;
        let cr_coef_offset: u32 = (y_coefs + cb_coefs).try_into().map_err(|_| {
            Error::GpuError(
                "jpeg_decode kernel: Y+Cb coefficient count overflows u32 offset".into(),
            )
        })?;
        // Sanity: total coefficient count fits a u32 too.
        let _total: u32 = (y_coefs + cb_coefs + cr_coefs).try_into().map_err(|_| {
            Error::GpuError(
                "jpeg_decode kernel: total coefficient count overflows u32".into(),
            )
        })?;

        Ok(Self {
            y: y_idx,
            cb: cb_idx,
            cr: cr_idx,
            y_blocks_h,
            y_blocks_v,
            chroma_blocks_h,
            chroma_blocks_v,
            cb_coef_offset,
            cr_coef_offset,
            // Y table at 0; chroma table at 64.
            chroma_qtable_offset: 64,
        })
    }

    fn pack_coefficients(&self, decoded: &DecodedJpeg) -> Vec<i32> {
        let y = &decoded.components[self.y];
        let cb = &decoded.components[self.cb];
        let cr = &decoded.components[self.cr];
        let total = y.coefficients.len() + cb.coefficients.len() + cr.coefficients.len();
        let mut packed = Vec::with_capacity(total);
        packed.extend(y.coefficients.iter().map(|&c| i32::from(c)));
        packed.extend(cb.coefficients.iter().map(|&c| i32::from(c)));
        packed.extend(cr.coefficients.iter().map(|&c| i32::from(c)));
        packed
    }

    fn pack_quant_tables(&self, decoded: &DecodedJpeg) -> Result<Vec<u32>> {
        // [Y table][chroma table], 64 entries each.
        let mut packed = Vec::with_capacity(128);

        let y_qt_id = decoded.components[self.y].quant_table_id;
        let chroma_qt_id = decoded.components[self.cb].quant_table_id;

        let y_qt = decoded.quantization_table(y_qt_id).ok_or_else(|| {
            Error::GpuError(format!(
                "jpeg_decode kernel: missing Y quant table id={}",
                y_qt_id
            ))
        })?;
        let chroma_qt = decoded.quantization_table(chroma_qt_id).ok_or_else(|| {
            Error::GpuError(format!(
                "jpeg_decode kernel: missing chroma quant table id={}",
                chroma_qt_id
            ))
        })?;
        if y_qt.precision != 0 || chroma_qt.precision != 0 {
            return Err(Error::NotSupported(
                "jpeg_decode kernel: 16-bit precision quant tables not yet supported".into(),
            ));
        }

        packed.extend(y_qt.values.iter().map(|&q| u32::from(q)));
        packed.extend(chroma_qt.values.iter().map(|&q| u32::from(q)));
        Ok(packed)
    }
}
