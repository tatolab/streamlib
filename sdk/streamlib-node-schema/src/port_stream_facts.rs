// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! An output port's stream facts: what a Texture or an encoded output states about the stream
//! it carries. `connect` never compares them.

use serde::{Deserialize, Serialize};

use crate::pin_type::{BuiltInPinTypeName, PinType};

/// An Image stream's frame rate, as a rational number of frames per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageStreamFrameRate {
    pub numerator: u32,
    pub denominator: u32,
}

/// An HDR stream's content light level, in cd/m² (CTA-861.3, the H.265
/// `content_light_level_info` SEI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageStreamContentLightLevel {
    /// MaxCLL: the brightest pixel of the stream.
    pub max_content_light_level: u16,
    /// MaxFALL: the brightest frame's average.
    pub max_frame_average_light_level: u16,
}

/// The mastering display an HDR stream was graded on (SMPTE ST 2086), in the
/// H.265 `mastering_display_colour_volume` SEI's units: chromaticity in steps
/// of 0.00002, luminance in steps of 0.0001 cd/m².
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageStreamMasteringDisplayColourVolume {
    /// The green, blue and red primaries' x, in the SEI's order.
    pub display_primaries_x: [u16; 3],
    /// The green, blue and red primaries' y, in the SEI's order.
    pub display_primaries_y: [u16; 3],
    pub white_point_x: u16,
    pub white_point_y: u16,
    pub max_display_mastering_luminance: u32,
    pub min_display_mastering_luminance: u32,
}

/// What an Image output states about its stream; every field optional.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct ImageStreamFacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<ImageStreamFrameRate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_light_level: Option<ImageStreamContentLightLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mastering_display_colour_volume: Option<ImageStreamMasteringDisplayColourVolume>,
}

/// What a Packets output states about its stream before its first unit, for a
/// decoder or a muxer; every field optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PacketsStreamFacts {
    /// The codec with its profile and level, as an RFC 6381 `codecs`
    /// parameter spells it (`avc1.64001f`, `hvc1.1.6.L93.B0`, `opus`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rfc6381_codec: Option<String>,
    /// Width of the coded picture, in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coded_width: Option<u32>,
    /// Height of the coded picture, in pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coded_height: Option<u32>,
    /// Samples per second, per channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_count: Option<u32>,
    /// The codec's out-of-band configuration record (`avcC`, `hvcC`,
    /// `OpusHead`) as lowercase hex; absent when the stream carries it in band.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec_configuration_hex: Option<String>,
}

/// An output port's stream facts: a Texture's, or an encoded stream's. The wire tags them
/// `image` and `packets` under `family`, as the graph has always served them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "family", rename_all = "snake_case")]
pub enum PortStreamFacts {
    Image(ImageStreamFacts),
    Packets(PacketsStreamFacts),
}

impl PortStreamFacts {
    /// The Image facts, or `None` when these are an encoded stream's.
    pub fn image(&self) -> Option<&ImageStreamFacts> {
        match self {
            PortStreamFacts::Image(image) => Some(image),
            PortStreamFacts::Packets(_) => None,
        }
    }

    /// The Packets facts, or `None` when these are a Texture's.
    pub fn packets(&self) -> Option<&PacketsStreamFacts> {
        match self {
            PortStreamFacts::Packets(packets) => Some(packets),
            PortStreamFacts::Image(_) => None,
        }
    }

    /// The name of `pin_type` when these facts describe a stream of it, as Image facts describe
    /// a Texture and Packets facts an encoded type; `None` for any other type.
    pub fn the_pin_type_they_describe(&self, pin_type: &PinType) -> Option<BuiltInPinTypeName> {
        let name = pin_type.built_in_name();
        let describes = match self {
            PortStreamFacts::Image(_) => name == BuiltInPinTypeName::Texture,
            PortStreamFacts::Packets(_) => matches!(
                name,
                BuiltInPinTypeName::EncodedVideo | BuiltInPinTypeName::EncodedAudio
            ),
        };
        describes.then_some(name)
    }

    /// What these facts describe, as a refusal names it.
    fn what_they_describe(&self) -> &'static str {
        match self {
            PortStreamFacts::Image(_) => "Texture",
            PortStreamFacts::Packets(_) => "encoded",
        }
    }
}

impl From<ImageStreamFacts> for PortStreamFacts {
    fn from(image: ImageStreamFacts) -> Self {
        PortStreamFacts::Image(image)
    }
}

impl From<PacketsStreamFacts> for PortStreamFacts {
    fn from(packets: PacketsStreamFacts) -> Self {
        PortStreamFacts::Packets(packets)
    }
}

/// Why stream facts were refused for an output port.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("output port `{port_name}` carries {pin_type} and cannot carry {facts_describe} stream facts")]
pub struct StreamFactsOfAnotherPinType {
    pub port_name: String,
    pub pin_type: PinType,
    pub facts_describe: &'static str,
}

/// Refuse `facts` for `port_name`, of `pin_type`, unless they describe a stream of that type.
pub fn refuse_stream_facts_of_another_pin_type(
    port_name: &str,
    pin_type: &PinType,
    facts: &PortStreamFacts,
) -> Result<(), StreamFactsOfAnotherPinType> {
    if facts.the_pin_type_they_describe(pin_type).is_some() {
        return Ok(());
    }
    Err(StreamFactsOfAnotherPinType {
        port_name: port_name.to_string(),
        pin_type: pin_type.clone(),
        facts_describe: facts.what_they_describe(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stream_facts_render_tagged_by_their_family_omitting_what_is_unstated() {
        let image = PortStreamFacts::from(ImageStreamFacts {
            frame_rate: Some(ImageStreamFrameRate {
                numerator: 30_000,
                denominator: 1_001,
            }),
            content_light_level: Some(ImageStreamContentLightLevel {
                max_content_light_level: 1_000,
                max_frame_average_light_level: 400,
            }),
            mastering_display_colour_volume: None,
        });
        assert_eq!(
            serde_json::to_value(&image).unwrap(),
            json!({
                "family": "image",
                "frame_rate": {"numerator": 30000, "denominator": 1001},
                "content_light_level": {"max_content_light_level": 1000, "max_frame_average_light_level": 400},
            })
        );
        let packets = PortStreamFacts::from(PacketsStreamFacts {
            rfc6381_codec: Some("opus".into()),
            sample_rate: Some(48_000),
            channel_count: Some(2),
            ..PacketsStreamFacts::default()
        });
        assert_eq!(
            serde_json::to_value(&packets).unwrap(),
            json!({"family": "packets", "rfc6381_codec": "opus", "sample_rate": 48000, "channel_count": 2})
        );
        assert_eq!(
            serde_json::from_value::<PortStreamFacts>(serde_json::to_value(&packets).unwrap())
                .unwrap(),
            packets
        );
    }

    #[test]
    fn facts_of_a_family_without_stream_facts_or_with_an_unknown_field_do_not_read() {
        assert!(serde_json::from_value::<PortStreamFacts>(json!({"family": "records"})).is_err());
        assert!(
            serde_json::from_value::<PortStreamFacts>(json!({"family": "image", "fps": 30}))
                .is_err()
        );
    }

    #[test]
    fn facts_describe_the_pin_type_their_family_carries_and_no_other() {
        let image = PortStreamFacts::from(ImageStreamFacts::default());
        let packets = PortStreamFacts::from(PacketsStreamFacts::default());
        let texture = PinType::Texture {
            pixel_formats: Vec::new(),
        };
        let encoded_video = PinType::EncodedVideo { codecs: Vec::new() };
        let encoded_audio = PinType::EncodedAudio { codecs: Vec::new() };

        assert_eq!(
            image.the_pin_type_they_describe(&texture),
            Some(BuiltInPinTypeName::Texture)
        );
        assert_eq!(
            packets.the_pin_type_they_describe(&encoded_video),
            Some(BuiltInPinTypeName::EncodedVideo)
        );
        assert_eq!(
            packets.the_pin_type_they_describe(&encoded_audio),
            Some(BuiltInPinTypeName::EncodedAudio)
        );
        assert_eq!(image.the_pin_type_they_describe(&encoded_video), None);
        assert_eq!(packets.the_pin_type_they_describe(&texture), None);
        assert_eq!(image.the_pin_type_they_describe(&PinType::Audio {}), None);
    }

    #[test]
    fn facts_of_another_pin_type_are_refused_naming_the_port_and_both() {
        let texture = PinType::Texture {
            pixel_formats: Vec::new(),
        };
        let refusal = refuse_stream_facts_of_another_pin_type(
            "video",
            &texture,
            &PacketsStreamFacts::default().into(),
        )
        .unwrap_err();
        assert_eq!(
            refusal.to_string(),
            format!("output port `video` carries {texture} and cannot carry encoded stream facts")
        );
        assert!(
            refuse_stream_facts_of_another_pin_type(
                "video",
                &texture,
                &ImageStreamFacts::default().into()
            )
            .is_ok()
        );
    }
}
