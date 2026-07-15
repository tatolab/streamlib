// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `#[repr(C)]` POD projections for the hardware video encode/decode
//! surface (#1259).
//!
//! All eight structs plus four `#[repr(u32)]` enums are pure-POD
//! (primitive or explicit-`repr` fields only), so their byte layout is
//! fully source-determined. They are locked by the per-struct
//! `offset_of!` / discriminant regression tests here and deliberately
//! excluded from [`crate::PLUGIN_ABI_LAYOUT_FINGERPRINT`] (the POD
//! exclusion rule — see the fold doc-comment in `lib.rs`). The session
//! methods vtables ([`crate::VideoEncoderSessionMethodsVTable`] /
//! [`crate::VideoDecoderSessionMethodsVTable`]) that consume them ARE
//! folded, per the dispatch-surface rule.

/// Codec discriminant. Mirrors `encode::config::Codec`. `Av1 = 2` is
/// reserved (discriminants freeze forever; internal AV1 scaffolding
/// exists, no package ships) — the host returns a typed
/// unsupported-codec error for discriminant 2 until an AV1 surface
/// lands.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodecRepr {
    H264 = 0,
    H265 = 1,
    /// RESERVED — no package ships an AV1 encoder yet.
    Av1 = 2,
}

/// Frame-type discriminant. Mirrors `encode::config::FrameType`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFrameTypeRepr {
    Idr = 0,
    I = 1,
    P = 2,
    B = 3,
}

/// Rate-control-mode discriminant. Mirrors
/// `encode::config::RateControlMode`. Referenced by the reserved
/// `rate_control_mode` descriptor field.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoRateControlModeRepr {
    Default = 0,
    Cbr = 1,
    Vbr = 2,
    Cqp = 3,
}

/// Encoder preset discriminant. Mirrors `encode::config::Preset`.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoEncoderPresetRepr {
    Fast = 0,
    Medium = 1,
    Quality = 2,
}

/// Flattened `color_vui::H273ColorVui`. Each axis is `Option<u8>` /
/// `Option<bool>` on the Rust side, mirrored as a `value` byte + a
/// `present` byte (`Option` cannot cross the ABI). Values are raw H.273
/// enumerants. Used inbound (encoder descriptor) and outbound
/// (`current_color_vui`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct H273ColorVuiRepr {
    pub primaries: u8,
    pub primaries_present: u8,
    pub transfer: u8,
    pub transfer_present: u8,
    pub matrix: u8,
    pub matrix_present: u8,
    pub full_range: u8,
    pub full_range_present: u8,
}

/// Flattened `SimpleEncoderConfig`. `Option<T>` → (value + `has_` byte).
/// The reserved band (`max_bitrate_bps`, `rate_control_mode`,
/// `luma_bit_depth`, `chroma_subsampling`) is carried internally by
/// `EncodeConfig` already (defaulted); reserving it now avoids a
/// descriptor republish for explicit CBR/VBR or 10-bit/HDR.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VideoEncoderSessionDescriptorRepr {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// [`VideoCodecRepr`] discriminant.
    pub codec: u32,
    /// [`VideoEncoderPresetRepr`] discriminant.
    pub preset: u32,
    /// Constant QP; valid iff `has_qp`.
    pub qp: i32,
    /// Target bitrate; valid iff `has_bitrate`.
    pub bitrate_bps: u32,
    /// RESERVED (VBR ceiling; `0` = derive).
    pub max_bitrate_bps: u32,
    pub idr_interval_secs: u32,
    /// Encoder effort level; valid iff `has_effort_level`.
    pub effort_level: u32,
    /// RESERVED ([`VideoRateControlModeRepr`]; `0` = derive).
    pub rate_control_mode: u32,
    /// RESERVED (`8`).
    pub luma_bit_depth: u32,
    /// RESERVED (`0` = 4:2:0).
    pub chroma_subsampling: u32,
    pub has_qp: u8,
    pub has_bitrate: u8,
    pub has_effort_level: u8,
    /// `streaming` bool.
    pub streaming: u8,
    /// `prepend_header_to_idr` is `Option<bool>`: present byte.
    pub prepend_header_present: u8,
    /// `prepend_header_to_idr` value byte.
    pub prepend_header: u8,
    /// `0` = `create_encoder_session` eagerly runs
    /// `prepare_gpu_encode_resources` (matching both consumers); `1` =
    /// NV12-only callers skip the GPU-input pre-allocation.
    pub disable_gpu_input_prealloc: u8,
    /// Reserved padding (zero today, never read).
    pub _pad: u8,
    /// Inbound colorimetry / VUI (8 bytes).
    pub color_vui: H273ColorVuiRepr,
}

/// Flattened `SimpleDecoderConfig`. No bit-depth field: the decoder
/// auto-detects format (incl. P010/10-bit) from the SPS.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VideoDecoderSessionDescriptorRepr {
    /// [`VideoCodecRepr`] discriminant.
    pub codec: u32,
    /// `0` = auto-detect from first SPS.
    pub max_width: u32,
    /// `0` = auto-detect.
    pub max_height: u32,
    /// `DpbOutputMode`: `0 = Coincide`, `1 = Distinct`.
    pub output_mode: u32,
    /// `rgba_output` bool.
    pub rgba_output: u8,
    /// Reserved padding (zero today, never read).
    pub _pad: [u8; 3],
}

/// Metadata mirror of `EncodePacket`. Bitstream bytes travel in
/// `drain_packet`'s `out_data_buf`, never inline; `bitstream_size`
/// lets a meta-only probe size the retry buffer.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VideoEncodedPacketRepr {
    /// [`VideoFrameTypeRepr`] discriminant.
    pub frame_type: u32,
    pub is_keyframe: u8,
    pub has_timestamp: u8,
    pub _pad0: [u8; 2],
    pub pts: u64,
    /// Valid iff `has_timestamp`.
    pub timestamp_ns: i64,
    /// `== out_data_len` for the packet's bitstream.
    pub bitstream_size: u32,
    pub _reserved: u32,
}

/// Metadata mirror of `SimpleDecodedFrame`. Pixel bytes travel in
/// `drain_frame`'s `out_data_buf`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VideoDecodedFrameRepr {
    pub width: u32,
    pub height: u32,
    pub picture_order_count: i32,
    /// `== out_data_len`; `0` for a ring-resident frame (see
    /// `ring_slot_index`).
    pub pixel_size: u32,
    pub decode_order: u64,
    /// `1` = RGBA (W*H*4), `0` = NV12 (W*H*3/2).
    pub is_rgba: u8,
    pub _pad: [u8; 3],
    /// RESERVED (zero until `decode_into_ring` lands): the
    /// `TextureRing` slot a zero-copy decoded frame was written into.
    pub ring_slot_index: u32,
}

#[cfg(all(test, target_pointer_width = "64"))]
mod tests {
    use super::*;
    use core::mem::{align_of, offset_of, size_of};

    #[test]
    fn video_codec_repr_discriminants() {
        assert_eq!(size_of::<VideoCodecRepr>(), 4);
        assert_eq!(align_of::<VideoCodecRepr>(), 4);
        assert_eq!(VideoCodecRepr::H264 as u32, 0);
        assert_eq!(VideoCodecRepr::H265 as u32, 1);
        assert_eq!(VideoCodecRepr::Av1 as u32, 2);
    }

    #[test]
    fn video_frame_type_repr_discriminants() {
        assert_eq!(size_of::<VideoFrameTypeRepr>(), 4);
        assert_eq!(align_of::<VideoFrameTypeRepr>(), 4);
        assert_eq!(VideoFrameTypeRepr::Idr as u32, 0);
        assert_eq!(VideoFrameTypeRepr::I as u32, 1);
        assert_eq!(VideoFrameTypeRepr::P as u32, 2);
        assert_eq!(VideoFrameTypeRepr::B as u32, 3);
    }

    #[test]
    fn video_rate_control_mode_repr_discriminants() {
        assert_eq!(size_of::<VideoRateControlModeRepr>(), 4);
        assert_eq!(align_of::<VideoRateControlModeRepr>(), 4);
        assert_eq!(VideoRateControlModeRepr::Default as u32, 0);
        assert_eq!(VideoRateControlModeRepr::Cbr as u32, 1);
        assert_eq!(VideoRateControlModeRepr::Vbr as u32, 2);
        assert_eq!(VideoRateControlModeRepr::Cqp as u32, 3);
    }

    #[test]
    fn video_encoder_preset_repr_discriminants() {
        assert_eq!(size_of::<VideoEncoderPresetRepr>(), 4);
        assert_eq!(align_of::<VideoEncoderPresetRepr>(), 4);
        assert_eq!(VideoEncoderPresetRepr::Fast as u32, 0);
        assert_eq!(VideoEncoderPresetRepr::Medium as u32, 1);
        assert_eq!(VideoEncoderPresetRepr::Quality as u32, 2);
    }

    #[test]
    fn h273_color_vui_repr_layout() {
        assert_eq!(size_of::<H273ColorVuiRepr>(), 8);
        assert_eq!(align_of::<H273ColorVuiRepr>(), 1);
        assert_eq!(offset_of!(H273ColorVuiRepr, primaries), 0);
        assert_eq!(offset_of!(H273ColorVuiRepr, primaries_present), 1);
        assert_eq!(offset_of!(H273ColorVuiRepr, transfer), 2);
        assert_eq!(offset_of!(H273ColorVuiRepr, transfer_present), 3);
        assert_eq!(offset_of!(H273ColorVuiRepr, matrix), 4);
        assert_eq!(offset_of!(H273ColorVuiRepr, matrix_present), 5);
        assert_eq!(offset_of!(H273ColorVuiRepr, full_range), 6);
        assert_eq!(offset_of!(H273ColorVuiRepr, full_range_present), 7);
    }

    #[test]
    fn video_encoder_session_descriptor_repr_layout() {
        assert_eq!(size_of::<VideoEncoderSessionDescriptorRepr>(), 68);
        assert_eq!(align_of::<VideoEncoderSessionDescriptorRepr>(), 4);
        assert_eq!(offset_of!(VideoEncoderSessionDescriptorRepr, width), 0);
        assert_eq!(offset_of!(VideoEncoderSessionDescriptorRepr, height), 4);
        assert_eq!(offset_of!(VideoEncoderSessionDescriptorRepr, fps), 8);
        assert_eq!(offset_of!(VideoEncoderSessionDescriptorRepr, codec), 12);
        assert_eq!(offset_of!(VideoEncoderSessionDescriptorRepr, preset), 16);
        assert_eq!(offset_of!(VideoEncoderSessionDescriptorRepr, qp), 20);
        assert_eq!(offset_of!(VideoEncoderSessionDescriptorRepr, bitrate_bps), 24);
        assert_eq!(
            offset_of!(VideoEncoderSessionDescriptorRepr, max_bitrate_bps),
            28
        );
        assert_eq!(
            offset_of!(VideoEncoderSessionDescriptorRepr, idr_interval_secs),
            32
        );
        assert_eq!(
            offset_of!(VideoEncoderSessionDescriptorRepr, effort_level),
            36
        );
        assert_eq!(
            offset_of!(VideoEncoderSessionDescriptorRepr, rate_control_mode),
            40
        );
        assert_eq!(
            offset_of!(VideoEncoderSessionDescriptorRepr, luma_bit_depth),
            44
        );
        assert_eq!(
            offset_of!(VideoEncoderSessionDescriptorRepr, chroma_subsampling),
            48
        );
        assert_eq!(offset_of!(VideoEncoderSessionDescriptorRepr, has_qp), 52);
        assert_eq!(offset_of!(VideoEncoderSessionDescriptorRepr, has_bitrate), 53);
        assert_eq!(
            offset_of!(VideoEncoderSessionDescriptorRepr, has_effort_level),
            54
        );
        assert_eq!(offset_of!(VideoEncoderSessionDescriptorRepr, streaming), 55);
        assert_eq!(
            offset_of!(VideoEncoderSessionDescriptorRepr, prepend_header_present),
            56
        );
        assert_eq!(
            offset_of!(VideoEncoderSessionDescriptorRepr, prepend_header),
            57
        );
        assert_eq!(
            offset_of!(VideoEncoderSessionDescriptorRepr, disable_gpu_input_prealloc),
            58
        );
        assert_eq!(offset_of!(VideoEncoderSessionDescriptorRepr, color_vui), 60);
    }

    #[test]
    fn video_decoder_session_descriptor_repr_layout() {
        assert_eq!(size_of::<VideoDecoderSessionDescriptorRepr>(), 20);
        assert_eq!(align_of::<VideoDecoderSessionDescriptorRepr>(), 4);
        assert_eq!(offset_of!(VideoDecoderSessionDescriptorRepr, codec), 0);
        assert_eq!(offset_of!(VideoDecoderSessionDescriptorRepr, max_width), 4);
        assert_eq!(offset_of!(VideoDecoderSessionDescriptorRepr, max_height), 8);
        assert_eq!(
            offset_of!(VideoDecoderSessionDescriptorRepr, output_mode),
            12
        );
        assert_eq!(
            offset_of!(VideoDecoderSessionDescriptorRepr, rgba_output),
            16
        );
    }

    #[test]
    fn video_encoded_packet_repr_layout() {
        assert_eq!(size_of::<VideoEncodedPacketRepr>(), 32);
        assert_eq!(align_of::<VideoEncodedPacketRepr>(), 8);
        assert_eq!(offset_of!(VideoEncodedPacketRepr, frame_type), 0);
        assert_eq!(offset_of!(VideoEncodedPacketRepr, is_keyframe), 4);
        assert_eq!(offset_of!(VideoEncodedPacketRepr, has_timestamp), 5);
        assert_eq!(offset_of!(VideoEncodedPacketRepr, pts), 8);
        assert_eq!(offset_of!(VideoEncodedPacketRepr, timestamp_ns), 16);
        assert_eq!(offset_of!(VideoEncodedPacketRepr, bitstream_size), 24);
        assert_eq!(offset_of!(VideoEncodedPacketRepr, _reserved), 28);
    }

    #[test]
    fn video_decoded_frame_repr_layout() {
        assert_eq!(size_of::<VideoDecodedFrameRepr>(), 32);
        assert_eq!(align_of::<VideoDecodedFrameRepr>(), 8);
        assert_eq!(offset_of!(VideoDecodedFrameRepr, width), 0);
        assert_eq!(offset_of!(VideoDecodedFrameRepr, height), 4);
        assert_eq!(offset_of!(VideoDecodedFrameRepr, picture_order_count), 8);
        assert_eq!(offset_of!(VideoDecodedFrameRepr, pixel_size), 12);
        assert_eq!(offset_of!(VideoDecodedFrameRepr, decode_order), 16);
        assert_eq!(offset_of!(VideoDecodedFrameRepr, is_rgba), 24);
        assert_eq!(offset_of!(VideoDecodedFrameRepr, ring_slot_index), 28);
    }
}
