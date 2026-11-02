// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `VideoFrame` record on a `video` port: read as far as an encoder needs
//! it, written as a decoder publishes it, and its colour translated to and
//! from H.273 enumerants.

use serde::Deserialize;
use streamlib_consumer_rhi::H273ColorVui;
use streamlib_consumer_rhi::h273_color_vui::{matrix, primaries, transfer};

/// The fields of a `VideoFrame` the encoder reads.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct VideoFrameMessage {
    /// The published surface the frame's pixels are checked out from.
    pub surface_id: String,
    pub width: u32,
    pub height: u32,
    /// Monotonic nanoseconds, the stamp the media frame carries.
    pub timestamp_ns: i64,
    /// The source's frame rate, when its producer states one.
    #[serde(default)]
    pub fps: Option<u32>,
    #[serde(default)]
    pub color_info: Option<VideoFrameColorInfo>,
}

/// The H.273 four-tuple a `VideoFrame` describes its colour with, by wire name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct VideoFrameColorInfo {
    #[serde(default)]
    pub matrix: Option<String>,
    #[serde(default)]
    pub primaries: Option<String>,
    #[serde(default)]
    pub range: Option<String>,
    #[serde(default)]
    pub transfer: Option<String>,
}

impl VideoFrameMessage {
    /// Read one `video` message, refusing by name what is not a `VideoFrame`.
    pub fn from_msgpack(payload: &[u8]) -> Result<Self, String> {
        rmp_serde::from_slice(payload)
            .map_err(|refusal| format!("a `video` message is not a VideoFrame: {refusal}"))
    }
}

impl VideoFrameColorInfo {
    /// The H.273 enumerants these wire names stand for; a name outside the
    /// `ColorInfo` vocabulary is refused naming its axis.
    pub fn to_h273_color_vui(&self) -> Result<H273ColorVui, String> {
        Ok(H273ColorVui {
            primaries: axis_byte("primaries", self.primaries.as_deref(), PRIMARIES_WIRE_NAMES)?,
            transfer: axis_byte("transfer", self.transfer.as_deref(), TRANSFER_WIRE_NAMES)?,
            matrix: axis_byte("matrix", self.matrix.as_deref(), MATRIX_WIRE_NAMES)?,
            full_range: match self.range.as_deref() {
                None => None,
                Some("full") => Some(true),
                Some("limited") => Some(false),
                Some(other) => {
                    return Err(format!(
                        "colour `range` is {other:?}, which is not an H.273 range name"
                    ));
                }
            },
        })
    }
}

fn axis_byte(
    axis_name: &str,
    wire_name: Option<&str>,
    wire_names: &[(&str, u8)],
) -> Result<Option<u8>, String> {
    wire_name
        .map(|wire_name| {
            byte_named(wire_names, wire_name).ok_or_else(|| {
                format!(
                    "colour `{axis_name}` is {wire_name:?}, which is not an H.273 {axis_name} name"
                )
            })
        })
        .transpose()
}

/// `ColorInfo.primaries` wire names and the H.273 enumerants they stand for.
const PRIMARIES_WIRE_NAMES: &[(&str, u8)] = &[
    ("bt709", primaries::BT709),
    ("bt470_m", primaries::BT470_M),
    ("bt470_bg", primaries::BT470_BG),
    ("smpte170m", primaries::SMPTE170M),
    ("smpte240m", primaries::SMPTE240M),
    ("film", primaries::FILM),
    ("bt2020", primaries::BT2020),
    ("smpte428", primaries::SMPTE428),
    ("smpte431", primaries::SMPTE431),
    ("smpte432", primaries::SMPTE432),
    ("ebu3213", primaries::EBU3213),
];

/// `ColorInfo.transfer` wire names and the H.273 enumerants they stand for.
const TRANSFER_WIRE_NAMES: &[(&str, u8)] = &[
    ("bt709", transfer::BT709),
    ("gamma22", transfer::GAMMA22),
    ("gamma28", transfer::GAMMA28),
    ("smpte170m", transfer::SMPTE170M),
    ("smpte240m", transfer::SMPTE240M),
    ("linear", transfer::LINEAR),
    ("log100", transfer::LOG100),
    ("log100_sqrt10", transfer::LOG100_SQRT10),
    ("xvycc", transfer::XVYCC),
    ("bt1361", transfer::BT1361),
    ("srgb", transfer::SRGB),
    ("bt2020_ten_bit", transfer::BT2020_TEN_BIT),
    ("bt2020_twelve_bit", transfer::BT2020_TWELVE_BIT),
    ("smpte2084", transfer::SMPTE2084),
    ("smpte428", transfer::SMPTE428),
    ("arib_std_b67", transfer::ARIB_STD_B67),
];

/// `ColorInfo.matrix` wire names and the H.273 enumerants they stand for.
const MATRIX_WIRE_NAMES: &[(&str, u8)] = &[
    ("identity", matrix::IDENTITY),
    ("bt709", matrix::BT709),
    ("fcc", matrix::FCC),
    ("bt470_bg", matrix::BT470_BG),
    ("smpte170m", matrix::SMPTE170M),
    ("smpte240m", matrix::SMPTE240M),
    ("ycgco", matrix::YCGCO),
    ("bt2020_ncl", matrix::BT2020_NCL),
    ("bt2020_cl", matrix::BT2020_CL),
    ("smpte2085", matrix::SMPTE2085),
    ("chroma_ncl", matrix::CHROMA_NCL),
    ("chroma_cl", matrix::CHROMA_CL),
    ("ictcp", matrix::ICTCP),
];

fn byte_named(wire_names: &[(&str, u8)], wire_name: &str) -> Option<u8> {
    wire_names
        .iter()
        .find(|(name, _)| *name == wire_name)
        .map(|&(_, byte)| byte)
}

fn wire_name_of_byte(wire_names: &[(&'static str, u8)], byte: u8) -> Option<&'static str> {
    wire_names
        .iter()
        .find(|(_, named_byte)| *named_byte == byte)
        .map(|&(name, _)| name)
}

/// A colour's `ColorInfo` wire names, an axis H.273 leaves unspecified or the
/// vocabulary does not name left `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoFrameColorInfoWireNames {
    pub primaries: Option<&'static str>,
    pub transfer: Option<&'static str>,
    pub matrix: Option<&'static str>,
    pub range: Option<&'static str>,
}

impl VideoFrameColorInfoWireNames {
    /// The wire names `color_vui`'s enumerants stand for.
    pub fn of_h273_color_vui(color_vui: &H273ColorVui) -> Self {
        Self {
            primaries: color_vui
                .primaries
                .and_then(|byte| wire_name_of_byte(PRIMARIES_WIRE_NAMES, byte)),
            transfer: color_vui
                .transfer
                .and_then(|byte| wire_name_of_byte(TRANSFER_WIRE_NAMES, byte)),
            matrix: color_vui
                .matrix
                .and_then(|byte| wire_name_of_byte(MATRIX_WIRE_NAMES, byte)),
            range: color_vui
                .full_range
                .map(|full_range| if full_range { "full" } else { "limited" }),
        }
    }
}

/// The `VideoFrame` a decoder publishes: a picture named by the surface it
/// sits in, stamped with the media frame it decoded from.
#[derive(Debug, Clone, Copy)]
pub struct DecodedVideoFrameRecord<'surface> {
    pub surface_id: &'surface str,
    pub width: u32,
    pub height: u32,
    pub timestamp_ns: i64,
    pub color_info: VideoFrameColorInfoWireNames,
}

#[derive(serde::Serialize)]
struct DecodedVideoFrameOnTheWire<'surface> {
    surface_id: &'surface str,
    width: u32,
    height: u32,
    timestamp_ns: i64,
    color_info: Option<ColorInfoOnTheWire>,
}

#[derive(serde::Serialize)]
struct ColorInfoOnTheWire {
    primaries: Option<&'static str>,
    transfer: Option<&'static str>,
    matrix: Option<&'static str>,
    range: Option<&'static str>,
}

impl DecodedVideoFrameRecord<'_> {
    /// The msgpack map `tatolab.stream.VideoFrame.from_bytes` reads; a
    /// colour with no axis named is written as nil.
    pub fn to_msgpack(&self) -> Vec<u8> {
        let color_info = self.color_info;
        let wire = DecodedVideoFrameOnTheWire {
            surface_id: self.surface_id,
            width: self.width,
            height: self.height,
            timestamp_ns: self.timestamp_ns,
            color_info: (color_info != VideoFrameColorInfoWireNames::default()).then_some(
                ColorInfoOnTheWire {
                    primaries: color_info.primaries,
                    transfer: color_info.transfer,
                    matrix: color_info.matrix,
                    range: color_info.range,
                },
            ),
        };
        rmp_serde::to_vec_named(&wire).expect("a VideoFrame of strings and integers serializes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct VideoFrameAsAProducerWritesIt<'a> {
        surface_id: &'a str,
        width: u32,
        height: u32,
        timestamp_ns: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        fps: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        color_info: Option<ColorInfoAsAProducerWritesIt<'a>>,
        texture_layout: Option<i32>,
    }

    #[derive(Serialize)]
    struct ColorInfoAsAProducerWritesIt<'a> {
        matrix: &'a str,
        primaries: &'a str,
        range: &'a str,
        transfer: &'a str,
    }

    #[test]
    fn a_named_map_video_frame_reads_with_its_colour() {
        let payload = rmp_serde::to_vec_named(&VideoFrameAsAProducerWritesIt {
            surface_id: "surface-7",
            width: 1280,
            height: 720,
            timestamp_ns: 42,
            fps: Some(30),
            color_info: Some(ColorInfoAsAProducerWritesIt {
                matrix: "bt709",
                primaries: "bt709",
                range: "full",
                transfer: "srgb",
            }),
            texture_layout: None,
        })
        .unwrap();

        let frame = VideoFrameMessage::from_msgpack(&payload).unwrap();

        assert_eq!(frame.surface_id, "surface-7");
        assert_eq!(
            (frame.width, frame.height, frame.fps),
            (1280, 720, Some(30))
        );
        let color_vui = frame.color_info.unwrap().to_h273_color_vui().unwrap();
        assert_eq!(color_vui.primaries, Some(primaries::BT709));
        assert_eq!(color_vui.transfer, Some(transfer::SRGB));
        assert_eq!(color_vui.matrix, Some(matrix::BT709));
        assert_eq!(color_vui.full_range, Some(true));
    }

    #[test]
    fn a_video_frame_without_colour_or_rate_reads() {
        let payload = rmp_serde::to_vec_named(&VideoFrameAsAProducerWritesIt {
            surface_id: "s",
            width: 2,
            height: 2,
            timestamp_ns: 0,
            fps: None,
            color_info: None,
            texture_layout: Some(5),
        })
        .unwrap();

        let frame = VideoFrameMessage::from_msgpack(&payload).unwrap();

        assert_eq!(frame.fps, None);
        assert_eq!(frame.color_info, None);
    }

    #[test]
    fn an_unknown_colour_name_is_refused_naming_its_axis() {
        let color_info = VideoFrameColorInfo {
            transfer: Some("pq".to_owned()),
            ..Default::default()
        };
        let refusal = color_info.to_h273_color_vui().unwrap_err();
        assert!(refusal.contains("`transfer`"), "{refusal}");
    }

    #[test]
    fn bytes_that_are_not_a_video_frame_are_refused() {
        assert!(VideoFrameMessage::from_msgpack(b"\x01").is_err());
    }

    #[test]
    fn a_decoded_frame_reads_back_as_a_video_frame_with_its_colour() {
        let record = DecodedVideoFrameRecord {
            surface_id: "pool-texture-3",
            width: 320,
            height: 180,
            timestamp_ns: 7,
            color_info: VideoFrameColorInfoWireNames::of_h273_color_vui(&H273ColorVui {
                primaries: Some(primaries::BT709),
                transfer: Some(transfer::BT709),
                matrix: Some(matrix::SMPTE170M),
                full_range: Some(false),
            }),
        }
        .to_msgpack();

        let frame = VideoFrameMessage::from_msgpack(&record).unwrap();

        assert_eq!(frame.surface_id, "pool-texture-3");
        assert_eq!(
            (frame.width, frame.height, frame.timestamp_ns),
            (320, 180, 7)
        );
        let color_info = frame.color_info.unwrap();
        assert_eq!(color_info.matrix.as_deref(), Some("smpte170m"));
        assert_eq!(color_info.range.as_deref(), Some("limited"));
    }

    #[test]
    fn every_wire_name_round_trips_through_its_enumerant() {
        for wire_names in [PRIMARIES_WIRE_NAMES, TRANSFER_WIRE_NAMES, MATRIX_WIRE_NAMES] {
            for &(name, byte) in wire_names {
                assert_eq!(wire_name_of_byte(wire_names, byte), Some(name));
            }
        }
    }

    #[test]
    fn a_decoded_frame_with_no_colour_writes_nil() {
        let record = DecodedVideoFrameRecord {
            surface_id: "s",
            width: 2,
            height: 2,
            timestamp_ns: 0,
            color_info: VideoFrameColorInfoWireNames::default(),
        }
        .to_msgpack();
        assert_eq!(
            VideoFrameMessage::from_msgpack(&record).unwrap().color_info,
            None
        );
    }
}
