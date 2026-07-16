// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Shared `Repr <-> engine` conversions for the hardware video
//! encode/decode surface (M32 #1259).
//!
//! The encoder host-body fill-in (#1376) lands these once; the decoder
//! sibling (#1377) imports them (encoder-inbound + decoder-outbound). The
//! `#[repr(C)]` POD projections live in `streamlib-plugin-abi`
//! (`repr/video.rs`); the engine codec types they map to
//! (`Codec` / `Preset` / `H273ColorVui`) are Linux-only (the `vulkan`
//! module is `#[cfg(target_os = "linux")]`), so these conversions are
//! Linux-gated too.
//!
//! Every discriminant match is exhaustive against the frozen repr
//! enumerants; an unsupported/reserved discriminant (e.g. AV1, no
//! package ships an encoder) returns a typed error string rather than
//! silently defaulting — the caller writes it into the slot's `err_buf`.

use streamlib_plugin_abi::{H273ColorVuiRepr, VideoCodecRepr, VideoEncoderPresetRepr};

use crate::vulkan::video::encode::{Codec, H273ColorVui, Preset};

/// Decode the frozen [`VideoCodecRepr`] discriminant into the engine
/// [`Codec`]. `Av1 = 2` is reserved (no package ships an AV1 encoder);
/// it returns a typed unsupported-codec error, as does any out-of-range
/// discriminant.
pub(in crate::core::plugin::host_services) fn codec_from_repr(
    codec_raw: u32,
) -> Result<Codec, String> {
    match codec_raw {
        x if x == VideoCodecRepr::H264 as u32 => Ok(Codec::H264),
        x if x == VideoCodecRepr::H265 as u32 => Ok(Codec::H265),
        x if x == VideoCodecRepr::Av1 as u32 => {
            Err("codec AV1 is reserved but not yet supported (no AV1 encoder ships)".to_string())
        }
        other => Err(format!("invalid codec discriminant {other}")),
    }
}

/// Encode the engine [`Codec`] back to the frozen [`VideoCodecRepr`]
/// (decoder-outbound direction).
///
/// The decoder-methods vtable frozen by #1253 exposes no codec-reporting
/// slot in v1 — `VideoDecodedFrameRepr` carries no codec field, and the
/// decoder decodes the codec it was configured with, so the codec is
/// already known caller-side. This stays test-only (`#[allow(dead_code)]`)
/// until a future decoder slot reports a detected codec; the round-trip
/// test below still locks it against [`codec_from_repr`].
#[allow(dead_code)]
pub(in crate::core::plugin::host_services) fn codec_to_repr(codec: Codec) -> VideoCodecRepr {
    match codec {
        Codec::H264 => VideoCodecRepr::H264,
        Codec::H265 => VideoCodecRepr::H265,
    }
}

/// Decode the frozen [`VideoEncoderPresetRepr`] discriminant into the
/// engine [`Preset`]. An out-of-range discriminant is a typed error.
pub(in crate::core::plugin::host_services) fn preset_from_repr(
    preset_raw: u32,
) -> Result<Preset, String> {
    match preset_raw {
        x if x == VideoEncoderPresetRepr::Fast as u32 => Ok(Preset::Fast),
        x if x == VideoEncoderPresetRepr::Medium as u32 => Ok(Preset::Medium),
        x if x == VideoEncoderPresetRepr::Quality as u32 => Ok(Preset::Quality),
        other => Err(format!("invalid encoder preset discriminant {other}")),
    }
}

/// Decode the flattened [`H273ColorVuiRepr`] (each axis a `value` byte
/// plus a `present` byte, since `Option` cannot cross the ABI) into the
/// engine [`H273ColorVui`] (encoder-inbound).
pub(in crate::core::plugin::host_services) fn h273_color_vui_from_repr(
    repr: &H273ColorVuiRepr,
) -> H273ColorVui {
    H273ColorVui {
        primaries: (repr.primaries_present != 0).then_some(repr.primaries),
        transfer: (repr.transfer_present != 0).then_some(repr.transfer),
        matrix: (repr.matrix_present != 0).then_some(repr.matrix),
        full_range: (repr.full_range_present != 0).then_some(repr.full_range != 0),
    }
}

/// Encode the engine [`H273ColorVui`] back to the flattened
/// [`H273ColorVuiRepr`] (decoder-outbound: `current_color_vui`). A `None`
/// axis writes `value = 0`, `present = 0`.
///
/// Consumed by the decoder sibling's `current_color_vui` methods-vtable
/// body (#1377); the round-trip test below locks it against
/// [`h273_color_vui_from_repr`].
pub(in crate::core::plugin::host_services) fn h273_color_vui_to_repr(
    vui: &H273ColorVui,
) -> H273ColorVuiRepr {
    H273ColorVuiRepr {
        primaries: vui.primaries.unwrap_or(0),
        primaries_present: u8::from(vui.primaries.is_some()),
        transfer: vui.transfer.unwrap_or(0),
        transfer_present: u8::from(vui.transfer.is_some()),
        matrix: vui.matrix.unwrap_or(0),
        matrix_present: u8::from(vui.matrix.is_some()),
        full_range: u8::from(vui.full_range.unwrap_or(false)),
        full_range_present: u8::from(vui.full_range.is_some()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_round_trips() {
        for codec in [Codec::H264, Codec::H265] {
            let raw = codec_to_repr(codec) as u32;
            assert_eq!(codec_from_repr(raw).expect("valid codec"), codec);
        }
    }

    #[test]
    fn codec_av1_is_typed_unsupported_error() {
        let err = codec_from_repr(VideoCodecRepr::Av1 as u32).expect_err("AV1 unsupported");
        assert!(err.contains("AV1"), "got: {err}");
    }

    #[test]
    fn codec_out_of_range_is_typed_error() {
        let err = codec_from_repr(99).expect_err("out-of-range codec");
        assert!(err.contains("invalid codec discriminant 99"), "got: {err}");
    }

    #[test]
    fn preset_decodes_every_variant() {
        assert_eq!(
            preset_from_repr(VideoEncoderPresetRepr::Fast as u32).unwrap(),
            Preset::Fast
        );
        assert_eq!(
            preset_from_repr(VideoEncoderPresetRepr::Medium as u32).unwrap(),
            Preset::Medium
        );
        assert_eq!(
            preset_from_repr(VideoEncoderPresetRepr::Quality as u32).unwrap(),
            Preset::Quality
        );
    }

    #[test]
    fn preset_out_of_range_is_typed_error() {
        let err = preset_from_repr(7).expect_err("out-of-range preset");
        assert!(
            err.contains("invalid encoder preset discriminant 7"),
            "got: {err}"
        );
    }

    #[test]
    fn color_vui_round_trips_through_repr() {
        // Every axis present with distinct H.273 enumerants + full range.
        let vui = H273ColorVui {
            primaries: Some(9), // bt2020
            transfer: Some(16), // smpte2084
            matrix: Some(9),    // bt2020_ncl
            full_range: Some(true),
        };
        let repr = h273_color_vui_to_repr(&vui);
        assert_eq!(h273_color_vui_from_repr(&repr), vui);
    }

    #[test]
    fn color_vui_all_none_maps_to_all_absent() {
        let vui = H273ColorVui::default();
        let repr = h273_color_vui_to_repr(&vui);
        assert_eq!(repr.primaries_present, 0);
        assert_eq!(repr.transfer_present, 0);
        assert_eq!(repr.matrix_present, 0);
        assert_eq!(repr.full_range_present, 0);
        // Reverse: an all-absent repr decodes back to the all-None default.
        assert_eq!(h273_color_vui_from_repr(&repr), vui);
    }

    #[test]
    fn color_vui_present_byte_gates_value() {
        // A repr with a stale `value` byte but `present == 0` must decode
        // to `None`, never leak the stale value (mental-revert: dropping
        // the `_present != 0` guard leaks `Some(42)`).
        let repr = H273ColorVuiRepr {
            primaries: 42,
            primaries_present: 0,
            ..Default::default()
        };
        assert_eq!(h273_color_vui_from_repr(&repr).primaries, None);
    }
}
