// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The video-frame bag convention the built-ins produce and consume.
//!
//! A link carries a self-describing msgpack named map; these types are the
//! optional Rust cast for it — never declared on a port, never registered
//! anywhere. The field names ARE the wire contract: a consumer in any
//! language reads the same keys from the bag dict. The map is open — a
//! producer may carry extra keys and this cast ignores them, matching the
//! Python cast's behavior.

use serde::de::{self, DeserializeOwned, IntoDeserializer};
use serde::{Deserialize, Deserializer, Serialize};

/// Video frame bag: references a GPU surface by id — pixels never ride the
/// link. `surface_id` is the handoff contract (texture cache in-process,
/// surface-share DMA-BUF cross-process, pixel buffer for CPU readback);
/// `timestamp_ns` is the ordering primitive; nothing else. Consumers that
/// need "frame N" semantics derive it from timestamps, never from a
/// cross-processor counter.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VideoFrame {
    /// GPU surface id, resolved out-of-band via the engine's surface APIs.
    pub surface_id: String,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Monotonic timestamp in nanoseconds, stamped via `MediaClock` — the
    /// machine's monotonic epoch, comparable to V4L2 / ALSA driver stamps and
    /// to a reading taken in any other process on the host.
    pub timestamp_ns: i64,
    /// H.273 / ITU-T VUI four-tuple describing this frame's color. Absent
    /// means unknown; every consumer treats absent as all-`unspecified`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_info: Option<ColorInfo>,
    /// HDR10 content light level (MaxCLL / MaxFALL). Absent for SDR streams
    /// or when not measured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_light: Option<ContentLight>,
    /// Source frame rate in frames per second, set by the capture device.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<u32>,
    /// SMPTE ST.2086 mastering display color volume (HDR10 static metadata).
    /// Absent for SDR streams.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mastering_display: Option<MasteringDisplay>,
    /// Producer's published `VkImageLayout` for this frame's texture, as the
    /// raw int32 enumerant — a per-frame override of the per-surface layout
    /// published via surface-share. Absent when the per-surface default holds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub texture_layout: Option<i32>,
}

/// Per-frame color description (H.273 / ITU-T VUI 4-tuple).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ColorInfo {
    /// YCbCr matrix coefficients (H.273 `MatrixCoefficients`). Absent =
    /// unspecified (H.273 value 2).
    #[serde(
        default,
        deserialize_with = "matrix_axis_named",
        skip_serializing_if = "Option::is_none"
    )]
    pub matrix: Option<Matrix>,
    /// Color primaries (H.273 `ColourPrimaries`). Absent = unspecified.
    #[serde(
        default,
        deserialize_with = "primaries_axis_named",
        skip_serializing_if = "Option::is_none"
    )]
    pub primaries: Option<Primaries>,
    /// Quantization range (VUI `video_full_range_flag`). Absent = unspecified.
    #[serde(
        default,
        deserialize_with = "range_axis_named",
        skip_serializing_if = "Option::is_none"
    )]
    pub range: Option<Range>,
    /// Transfer characteristic (H.273 `TransferCharacteristics`). Absent =
    /// unspecified.
    #[serde(
        default,
        deserialize_with = "transfer_axis_named",
        skip_serializing_if = "Option::is_none"
    )]
    pub transfer: Option<Transfer>,
}

/// One H.273 axis read by its wire name. A name the vocabulary cannot place
/// is a producer's mistake — the engine's own producers narrow an unknown
/// H.273 byte to absent before it reaches a bag — so it is refused naming
/// the axis and the value, rather than failing the whole bag under serde's
/// bare `unknown variant`.
fn h273_axis_named<'de, D, Axis>(deserializer: D, axis_name: &str) -> Result<Option<Axis>, D::Error>
where
    D: Deserializer<'de>,
    Axis: DeserializeOwned,
{
    let Some(axis_value) = Option::<String>::deserialize(deserializer).map_err(|error| {
        de::Error::custom(format!(
            "colour `{axis_name}` must be a string or absent ({error})"
        ))
    })?
    else {
        return Ok(None);
    };
    Axis::deserialize(IntoDeserializer::<de::value::Error>::into_deserializer(
        axis_value.as_str(),
    ))
    .map(Some)
    .map_err(|_| {
        de::Error::custom(format!(
            "colour `{axis_name}` is {axis_value:?}, which is not an H.273 {axis_name} name"
        ))
    })
}

fn matrix_axis_named<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Matrix>, D::Error> {
    h273_axis_named(deserializer, "matrix")
}

fn primaries_axis_named<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Primaries>, D::Error> {
    h273_axis_named(deserializer, "primaries")
}

fn range_axis_named<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Range>, D::Error> {
    h273_axis_named(deserializer, "range")
}

fn transfer_axis_named<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Transfer>, D::Error> {
    h273_axis_named(deserializer, "transfer")
}

/// YCbCr matrix coefficients (H.273 `MatrixCoefficients` enumerant).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Matrix {
    #[serde(rename = "bt2020_cl")]
    Bt2020Cl,
    #[serde(rename = "bt2020_ncl")]
    Bt2020Ncl,
    #[serde(rename = "bt470_bg")]
    Bt470Bg,
    #[serde(rename = "bt709")]
    Bt709,
    #[serde(rename = "chroma_cl")]
    ChromaCl,
    #[serde(rename = "chroma_ncl")]
    ChromaNcl,
    #[serde(rename = "fcc")]
    Fcc,
    #[serde(rename = "ictcp")]
    Ictcp,
    #[serde(rename = "identity")]
    Identity,
    #[serde(rename = "smpte170m")]
    Smpte170m,
    #[serde(rename = "smpte2085")]
    Smpte2085,
    #[serde(rename = "smpte240m")]
    Smpte240m,
    #[serde(rename = "ycgco")]
    Ycgco,
}

/// Color primaries (H.273 `ColourPrimaries` enumerant).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Primaries {
    #[serde(rename = "bt2020")]
    Bt2020,
    #[serde(rename = "bt470_bg")]
    Bt470Bg,
    #[serde(rename = "bt470_m")]
    Bt470M,
    #[serde(rename = "bt709")]
    Bt709,
    #[serde(rename = "ebu3213")]
    Ebu3213,
    #[serde(rename = "film")]
    Film,
    #[serde(rename = "smpte170m")]
    Smpte170m,
    #[serde(rename = "smpte240m")]
    Smpte240m,
    #[serde(rename = "smpte428")]
    Smpte428,
    #[serde(rename = "smpte431")]
    Smpte431,
    #[serde(rename = "smpte432")]
    Smpte432,
}

/// Quantization range (H.264/H.265 VUI `video_full_range_flag`:
/// `limited` = 0, `full` = 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Range {
    #[serde(rename = "full")]
    Full,
    #[serde(rename = "limited")]
    Limited,
}

/// Transfer characteristic (H.273 `TransferCharacteristics` enumerant).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Transfer {
    #[serde(rename = "arib_std_b67")]
    AribStdB67,
    #[serde(rename = "bt1361")]
    Bt1361,
    #[serde(rename = "bt2020_ten_bit")]
    Bt2020TenBit,
    #[serde(rename = "bt2020_twelve_bit")]
    Bt2020TwelveBit,
    #[serde(rename = "bt709")]
    Bt709,
    #[serde(rename = "gamma22")]
    Gamma22,
    #[serde(rename = "gamma28")]
    Gamma28,
    #[serde(rename = "linear")]
    Linear,
    #[serde(rename = "log100")]
    Log100,
    #[serde(rename = "log100_sqrt10")]
    Log100Sqrt10,
    #[serde(rename = "smpte170m")]
    Smpte170m,
    #[serde(rename = "smpte2084")]
    Smpte2084,
    #[serde(rename = "smpte240m")]
    Smpte240m,
    #[serde(rename = "smpte428")]
    Smpte428,
    #[serde(rename = "srgb")]
    Srgb,
    #[serde(rename = "xvycc")]
    Xvycc,
}

/// HDR10 content light level (MaxCLL / MaxFALL).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ContentLight {
    /// Maximum content light level in cd/m², peak single-pixel.
    pub max_cll: u32,
    /// Maximum frame-average light level in cd/m².
    pub max_fall: u32,
}

/// SMPTE ST.2086 mastering display color volume (HDR10 static metadata).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MasteringDisplay {
    /// Blue primary x chromaticity in 1/50000 increments.
    pub display_primaries_b_x: u32,
    /// Blue primary y chromaticity in 1/50000 increments.
    pub display_primaries_b_y: u32,
    /// Green primary x chromaticity in 1/50000 increments.
    pub display_primaries_g_x: u32,
    /// Green primary y chromaticity in 1/50000 increments.
    pub display_primaries_g_y: u32,
    /// Red primary x chromaticity in 1/50000 increments.
    pub display_primaries_r_x: u32,
    /// Red primary y chromaticity in 1/50000 increments.
    pub display_primaries_r_y: u32,
    /// Maximum mastering display luminance in 0.0001 cd/m² increments.
    pub max_luminance: u32,
    /// Minimum mastering display luminance in 0.0001 cd/m² increments.
    pub min_luminance: u32,
    /// White point x chromaticity in 1/50000 increments.
    pub white_point_x: u32,
    /// White point y chromaticity in 1/50000 increments.
    pub white_point_y: u32,
}

// Per-axis maps from the bag's H.273 vocabulary to the engine's color IDs.
// The engine accepts only its own primitive types in public signatures, so
// this crate translates at the boundary.

impl Primaries {
    pub(crate) fn engine_id(&self) -> streamlib::sdk::color::PrimariesId {
        use streamlib::sdk::color::PrimariesId;
        match self {
            Primaries::Bt709 => PrimariesId::Bt709,
            Primaries::Bt470M => PrimariesId::Bt470M,
            Primaries::Bt470Bg => PrimariesId::Bt470Bg,
            Primaries::Smpte170m => PrimariesId::Smpte170m,
            Primaries::Smpte240m => PrimariesId::Smpte240m,
            Primaries::Film => PrimariesId::Film,
            Primaries::Bt2020 => PrimariesId::Bt2020,
            Primaries::Smpte428 => PrimariesId::Smpte428,
            Primaries::Smpte431 => PrimariesId::Smpte431,
            Primaries::Smpte432 => PrimariesId::Smpte432,
            Primaries::Ebu3213 => PrimariesId::Ebu3213,
        }
    }
}

impl Transfer {
    pub(crate) fn engine_id(&self) -> streamlib::sdk::color::TransferId {
        use streamlib::sdk::color::TransferId;
        match self {
            Transfer::Srgb => TransferId::Srgb,
            Transfer::Bt709
            | Transfer::Smpte170m
            | Transfer::Smpte240m
            | Transfer::Bt2020TenBit
            | Transfer::Bt2020TwelveBit => TransferId::Bt709,
            Transfer::Smpte2084 => TransferId::Pq,
            Transfer::AribStdB67 => TransferId::Hlg,
            Transfer::Linear => TransferId::Linear,
            // No exact engine id for these encoded transfers; a BT.709-shaped
            // approximation beats Linear, which would skip decoding entirely.
            Transfer::Gamma22
            | Transfer::Gamma28
            | Transfer::Bt1361
            | Transfer::Log100
            | Transfer::Log100Sqrt10
            | Transfer::Smpte428
            | Transfer::Xvycc => TransferId::Bt709,
        }
    }
}

impl Matrix {
    pub(crate) fn engine_id(&self) -> streamlib::sdk::color::MatrixId {
        use streamlib::sdk::color::MatrixId;
        match self {
            Matrix::Identity => MatrixId::Identity,
            Matrix::Bt709 => MatrixId::Bt709,
            Matrix::Fcc => MatrixId::Fcc,
            Matrix::Bt470Bg => MatrixId::Bt470Bg,
            Matrix::Smpte170m => MatrixId::Smpte170m,
            Matrix::Smpte240m => MatrixId::Smpte240m,
            Matrix::Ycgco => MatrixId::Ycgco,
            Matrix::Bt2020Ncl => MatrixId::Bt2020Ncl,
            Matrix::Bt2020Cl => MatrixId::Bt2020Cl,
            Matrix::Smpte2085 => MatrixId::Smpte2085,
            Matrix::ChromaNcl => MatrixId::ChromaNcl,
            Matrix::ChromaCl => MatrixId::ChromaCl,
            Matrix::Ictcp => MatrixId::Ictcp,
        }
    }
}

impl Range {
    pub(crate) fn engine_id(&self) -> streamlib::sdk::color::RangeId {
        use streamlib::sdk::color::RangeId;
        match self {
            Range::Limited => RangeId::Limited,
            Range::Full => RangeId::Full,
        }
    }
}

impl ColorInfo {
    /// The fully resolved description the engine's colour kernels take:
    /// every absent axis filled with `resolve_color_defaults`' answer for a
    /// source of `kind`.
    pub(crate) fn resolve_defaults(
        &self,
        kind: streamlib::sdk::color::ColorSpaceKind,
    ) -> streamlib::sdk::color::ResolvedColorInfo {
        streamlib::sdk::color::resolve_color_defaults(
            self.primaries.as_ref().map(Primaries::engine_id),
            self.transfer.as_ref().map(Transfer::engine_id),
            self.matrix.as_ref().map(Matrix::engine_id),
            self.range.as_ref().map(Range::engine_id),
            kind,
        )
    }

    /// The engine's colorspace-pick input: primaries + transfer only.
    pub fn engine_color_traits(&self) -> streamlib::sdk::color::ColorTraits {
        streamlib::sdk::color::ColorTraits {
            primaries: self.primaries.as_ref().map(Primaries::engine_id),
            transfer: self.transfer.as_ref().map(Transfer::engine_id),
        }
    }
}

impl MasteringDisplay {
    /// This sidecar as the engine's f32 metadata: chromaticities are 1/50000
    /// increments → CIE xy, luminances 0.0001 cd/m² increments → cd/m².
    ///
    /// Content light travels with it because the driver takes one struct: a
    /// bag carrying a mastering display and no content light still describes
    /// a display, and MaxCLL/MaxFALL of zero is how that is spelled.
    pub fn engine_hdr_static_metadata(
        &self,
        content_light: Option<&ContentLight>,
    ) -> streamlib::sdk::color::HdrStaticMetadata {
        let chromaticity = |increments: u32| increments as f32 / 50_000.0;
        streamlib::sdk::color::HdrStaticMetadata {
            display_primary_red: [
                chromaticity(self.display_primaries_r_x),
                chromaticity(self.display_primaries_r_y),
            ],
            display_primary_green: [
                chromaticity(self.display_primaries_g_x),
                chromaticity(self.display_primaries_g_y),
            ],
            display_primary_blue: [
                chromaticity(self.display_primaries_b_x),
                chromaticity(self.display_primaries_b_y),
            ],
            white_point: [
                chromaticity(self.white_point_x),
                chromaticity(self.white_point_y),
            ],
            min_luminance_cd_m2: self.min_luminance as f32 * 0.0001,
            max_luminance_cd_m2: self.max_luminance as f32 * 0.0001,
            max_content_light_level: content_light
                .map(|content_light| content_light.max_cll as f32)
                .unwrap_or(0.0),
            max_frame_average_light_level: content_light
                .map(|content_light| content_light.max_fall as f32)
                .unwrap_or(0.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::msgpack_wire_test_support::{
        decode_msgpack_named_map_entries, wire_map_entry_named,
    };

    #[test]
    fn hdr_metadata_translation_scales_the_wire_integers() {
        let mastering = MasteringDisplay {
            display_primaries_r_x: 35_400, // 0.708 in 1/50000
            display_primaries_r_y: 14_600,
            display_primaries_g_x: 8_500,
            display_primaries_g_y: 39_850,
            display_primaries_b_x: 6_550,
            display_primaries_b_y: 2_300,
            white_point_x: 15_635,
            white_point_y: 16_450,
            max_luminance: 10_000_000, // 1000 cd/m² in 0.0001 increments
            min_luminance: 50,         // 0.005 cd/m²
        };
        let content_light = ContentLight {
            max_cll: 1_000,
            max_fall: 400,
        };
        let metadata = mastering.engine_hdr_static_metadata(Some(&content_light));
        assert!((metadata.display_primary_red[0] - 0.708).abs() < 1e-6);
        assert!((metadata.max_luminance_cd_m2 - 1_000.0).abs() < 1e-3);
        assert!((metadata.min_luminance_cd_m2 - 0.005).abs() < 1e-6);
        assert_eq!(metadata.max_content_light_level, 1_000.0);
        assert_eq!(metadata.max_frame_average_light_level, 400.0);
    }

    /// The bag is a named map: field names are the cross-language contract.
    /// Locks the wire keys against accidental rename.
    #[test]
    fn video_frame_bag_carries_the_documented_keys() {
        let frame = VideoFrame {
            surface_id: "42".to_string(),
            width: 1280,
            height: 720,
            timestamp_ns: 123_456_789,
            fps: Some(30),
            color_info: Some(ColorInfo {
                primaries: Some(Primaries::Bt709),
                transfer: Some(Transfer::Srgb),
                matrix: None,
                range: Some(Range::Full),
            }),
            content_light: None,
            mastering_display: None,
            texture_layout: None,
        };
        let value = serde_json::to_value(&frame).expect("serialize");
        let map = value.as_object().expect("named map");
        assert_eq!(map["surface_id"], "42");
        assert_eq!(map["width"], 1280);
        assert_eq!(map["height"], 720);
        assert_eq!(map["timestamp_ns"], 123_456_789);
        assert_eq!(map["fps"], 30);
        assert_eq!(map["color_info"]["primaries"], "bt709");
        assert_eq!(map["color_info"]["transfer"], "srgb");
        assert_eq!(map["color_info"]["range"], "full");
        assert!(
            !map.contains_key("content_light") && !map.contains_key("mastering_display"),
            "absent optionals stay off the wire"
        );
    }

    /// The bag map is open: a producer carrying extra keys must not break
    /// this cast (mirrors the Python cast's behavior).
    #[test]
    fn video_frame_cast_ignores_unknown_keys() {
        let bag = serde_json::json!({
            "surface_id": "9",
            "width": 16,
            "height": 16,
            "timestamp_ns": 5,
            "a_future_key": "ignored",
        });
        let frame: VideoFrame = serde_json::from_value(bag).expect("open map");
        assert_eq!(frame.surface_id, "9");
    }

    /// The actual wire is msgpack via `rmp_serde::to_vec_named` (what
    /// `OutputWriter::write` does) — lock the named-map encoding and the
    /// documented keys at that boundary, not just JSON.
    #[test]
    fn video_frame_msgpack_wire_is_a_named_map_with_the_documented_keys() {
        let frame = VideoFrame {
            surface_id: "42".to_string(),
            width: 1280,
            height: 720,
            timestamp_ns: 123_456_789,
            fps: Some(30),
            ..VideoFrame::default()
        };
        let wire_bytes = rmp_serde::to_vec_named(&frame).expect("msgpack serialize");
        let entries = decode_msgpack_named_map_entries(&wire_bytes);
        let key = |name: &str| wire_map_entry_named(&entries, name);
        assert_eq!(key("surface_id").as_str(), Some("42"));
        assert_eq!(key("width").as_u64(), Some(1280));
        assert_eq!(key("height").as_u64(), Some(720));
        assert_eq!(key("timestamp_ns").as_i64(), Some(123_456_789));
        assert_eq!(key("fps").as_u64(), Some(30));

        let round_tripped: VideoFrame =
            rmp_serde::from_slice(&wire_bytes).expect("msgpack deserialize");
        assert_eq!(round_tripped, frame);
    }

    #[test]
    fn video_frame_round_trips() {
        let frame = VideoFrame {
            surface_id: "7".to_string(),
            width: 640,
            height: 480,
            timestamp_ns: 1,
            ..VideoFrame::default()
        };
        let bytes = serde_json::to_vec(&frame).expect("serialize");
        let back: VideoFrame = serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(back, frame);
    }

    fn a_frame_bag_whose_colour_axis_is(axis: &str, wire_value: &str) -> Vec<u8> {
        let bag: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"surface_id":"9","width":16,"height":16,"timestamp_ns":5,
                "color_info":{{"{axis}":{wire_value}}}}}"#
        ))
        .expect("a bag literal");
        rmp_serde::to_vec_named(&bag).expect("msgpack serialize")
    }

    /// Over the real wire, on every axis: the refusal names the axis and the
    /// value, so a producer's typo reads as its own mistake rather than as
    /// serde's bare `unknown variant`.
    #[test]
    fn a_colour_name_the_vocabulary_cannot_place_is_refused_naming_the_axis_and_the_value() {
        for axis in ["primaries", "transfer", "matrix", "range"] {
            let refusal = rmp_serde::from_slice::<VideoFrame>(&a_frame_bag_whose_colour_axis_is(
                axis,
                r#""bt_709""#,
            ))
            .expect_err("must be refused")
            .to_string();
            assert!(
                refusal.contains(&format!("`{axis}`")) && refusal.contains("bt_709"),
                "{axis}: {refusal}"
            );
        }
    }

    #[test]
    fn a_colour_axis_that_is_not_a_string_is_refused_naming_the_axis() {
        let refusal = rmp_serde::from_slice::<VideoFrame>(&a_frame_bag_whose_colour_axis_is(
            "primaries",
            "6",
        ))
        .expect_err("must be refused")
        .to_string();
        assert!(
            refusal.contains("`primaries`") && refusal.contains("must be a string"),
            "{refusal}"
        );
    }

    /// The refusal is scoped to the enumerant: a placeable name on one axis
    /// beside an absent one still reads, so the stricter read costs a valid
    /// bag nothing.
    #[test]
    fn a_placeable_colour_name_beside_absent_axes_still_reads() {
        let frame = rmp_serde::from_slice::<VideoFrame>(&a_frame_bag_whose_colour_axis_is(
            "transfer",
            r#""smpte2084""#,
        ))
        .expect("a valid bag reads");
        assert_eq!(
            frame.color_info,
            Some(ColorInfo {
                transfer: Some(Transfer::Smpte2084),
                ..ColorInfo::default()
            })
        );
    }
}
