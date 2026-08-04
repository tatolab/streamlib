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

use serde::{Deserialize, Serialize};

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
    /// Monotonic timestamp in nanoseconds, stamped via `MediaClock`. Today
    /// that epoch is process-relative; it becomes the machine's monotonic
    /// epoch (comparable to V4L2/ALSA driver stamps) when #1725 lands —
    /// until then, compare stamps only within one process.
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matrix: Option<Matrix>,
    /// Color primaries (H.273 `ColourPrimaries`). Absent = unspecified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primaries: Option<Primaries>,
    /// Quantization range (VUI `video_full_range_flag`). Absent = unspecified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
    /// Transfer characteristic (H.273 `TransferCharacteristics`). Absent =
    /// unspecified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer: Option<Transfer>,
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let value: rmpv::Value =
            rmpv::decode::read_value(&mut wire_bytes.as_slice()).expect("msgpack decode");
        let rmpv::Value::Map(entries) = value else {
            panic!("wire value must be a named map, got {value:?}");
        };
        let key = |name: &str| {
            entries
                .iter()
                .find(|(k, _)| k.as_str() == Some(name))
                .unwrap_or_else(|| panic!("wire map missing key {name:?}"))
                .1
                .clone()
        };
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
}
