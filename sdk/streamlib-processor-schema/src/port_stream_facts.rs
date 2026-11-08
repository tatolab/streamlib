// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! An output port's stream facts: what its family states about the stream it
//! carries, as typed fields of that family. `connect` never compares them.

use serde::{Deserialize, Serialize};

use crate::PortFamily;

/// An Image stream's frame rate, as a rational number of frames per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ImageStreamFrameRate {
    pub numerator: u32,
    pub denominator: u32,
}

/// An HDR stream's content light level, in cd/m² (CTA-861.3, the H.265
/// `content_light_level_info` SEI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
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

/// An output port's stream facts, tagged by the family they belong to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(tag = "family", rename_all = "snake_case")]
pub enum PortStreamFacts {
    Image(ImageStreamFacts),
    Packets(PacketsStreamFacts),
}

impl PortStreamFacts {
    /// The family whose facts these are.
    pub fn family(&self) -> PortFamily {
        match self {
            PortStreamFacts::Image(_) => PortFamily::Image,
            PortStreamFacts::Packets(_) => PortFamily::Packets,
        }
    }

    /// The Image facts, or `None` when these are another family's.
    pub fn image(&self) -> Option<&ImageStreamFacts> {
        match self {
            PortStreamFacts::Image(image) => Some(image),
            PortStreamFacts::Packets(_) => None,
        }
    }

    /// The Packets facts, or `None` when these are another family's.
    pub fn packets(&self) -> Option<&PacketsStreamFacts> {
        match self {
            PortStreamFacts::Packets(packets) => Some(packets),
            PortStreamFacts::Image(_) => None,
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
#[error(
    "output port `{port_name}` is of family {port_family} and cannot carry {facts_family} stream facts"
)]
pub struct StreamFactsOfAnotherFamily {
    pub port_name: String,
    pub port_family: PortFamily,
    pub facts_family: PortFamily,
}

/// Refuse `facts` for `port_name` unless they are `port_family`'s.
pub fn refuse_stream_facts_of_another_family(
    port_name: &str,
    port_family: PortFamily,
    facts: &PortStreamFacts,
) -> Result<(), StreamFactsOfAnotherFamily> {
    if facts.family() == port_family {
        return Ok(());
    }
    Err(StreamFactsOfAnotherFamily {
        port_name: port_name.to_string(),
        port_family,
        facts_family: facts.family(),
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
    fn another_familys_facts_are_refused_naming_the_port_and_both_families() {
        let refusal = refuse_stream_facts_of_another_family(
            "video",
            PortFamily::Image,
            &PacketsStreamFacts::default().into(),
        )
        .unwrap_err();
        assert_eq!(
            refusal.to_string(),
            "output port `video` is of family Image and cannot carry Packets stream facts"
        );
        assert!(
            refuse_stream_facts_of_another_family(
                "video",
                PortFamily::Image,
                &ImageStreamFacts::default().into()
            )
            .is_ok()
        );
    }
}
