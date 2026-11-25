// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What an output states about the stream it carries beside its pin type, as a pin annotation
//! holds it, and the codec nodes a person places to encode or decode a stream.

use serde::{Deserialize, Serialize};

use crate::pin_type::PinType;

/// The sample rate an Audio output states, written on the class or read
/// from one of its node's settings, which the wire names `from_setting`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelsOutputPortSampleRate {
    Stated(u32),
    #[serde(alias = "from_config")]
    FromSetting {
        #[serde(alias = "config_key")]
        setting: String,
    },
}

/// The frame rate a Texture output states, written on the class or read from
/// one of its node's settings, which the wire names `from_setting`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageOutputPortFrameRate {
    Stated(crate::ImageStreamFrameRate),
    #[serde(alias = "from_config")]
    FromSetting {
        #[serde(alias = "config_key")]
        setting: String,
    },
}

/// The sample type of an Audio output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChannelsOutputPortSampleType {
    F32,
    F64,
}

/// Where a Tensor output's values live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArrayOutputPortResidency {
    Gpu,
    Cpu,
}

/// The pin type `held`, the value of the pin type choice `setting`, chooses, or why it holds
/// none. Refused for a value holding no pin type, and for a type only a setting holds, which no
/// link carries.
pub fn the_pin_type_a_choice_holds(
    setting: &str,
    held: &serde_json::Value,
) -> Result<PinType, String> {
    let chosen = PinType::deserialize(held).map_err(|refusal| {
        format!("`{setting}` holds {held}, which chooses no pin type: {refusal}")
    })?;
    if matches!(
        chosen,
        PinType::NodeReference { .. } | PinType::PinTypeChoice { .. }
    ) {
        return Err(format!(
            "`{setting}` chooses {chosen}, which only a setting holds, and a stream's port \
             carries what a link carries"
        ));
    }
    Ok(chosen)
}

/// The frame rate `frame_rate`, the one a Texture output states, resolves to against its node's
/// config — a whole number of frames per second or a `{numerator, denominator}` map — or why it
/// cannot be resolved; `None` when it states none or its config key is unset.
pub fn output_port_frame_rate_resolved_against_config(
    frame_rate: Option<&ImageOutputPortFrameRate>,
    node_config: Option<&serde_json::Value>,
) -> Result<Option<crate::ImageStreamFrameRate>, String> {
    let config_key = match frame_rate {
        None => return Ok(None),
        Some(ImageOutputPortFrameRate::Stated(frame_rate)) => return Ok(Some(*frame_rate)),
        Some(ImageOutputPortFrameRate::FromSetting { setting }) => setting,
    };
    let refusal = |held: &serde_json::Value| {
        format!(
            "config key {config_key:?} states the frame rate as a positive whole number or a \
             {{numerator, denominator}} map, and holds {held}"
        )
    };
    match node_config.and_then(|config| config.get(config_key)) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(held @ serde_json::Value::Number(number)) => number
            .as_u64()
            .and_then(|whole| u32::try_from(whole).ok())
            .filter(|whole| *whole > 0)
            .map(|numerator| {
                Some(crate::ImageStreamFrameRate {
                    numerator,
                    denominator: 1,
                })
            })
            .ok_or_else(|| refusal(held)),
        Some(held) => serde_json::from_value::<crate::ImageStreamFrameRate>(held.clone())
            .ok()
            .filter(|frame_rate| frame_rate.numerator > 0 && frame_rate.denominator > 0)
            .map(Some)
            .ok_or_else(|| refusal(held)),
    }
}

/// What the packets of a codec encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EncodedMedia {
    Video,
    Audio,
}

/// A codec, what its packets encode, and the nodes that encode into it and decode out of it.
pub(crate) struct CodecNodes {
    pub(crate) codec: &'static str,
    pub(crate) media: EncodedMedia,
    pub(crate) encoder: &'static str,
    pub(crate) decoder: &'static str,
}

/// Each codec a node of the runtime's encodes into and decodes out of.
pub(crate) const CODEC_NODES: [CodecNodes; 3] = [
    CodecNodes {
        codec: "h264",
        media: EncodedMedia::Video,
        encoder: "H264Encoder",
        decoder: "H264Decoder",
    },
    CodecNodes {
        codec: "h265",
        media: EncodedMedia::Video,
        encoder: "H265Encoder",
        decoder: "H265Decoder",
    },
    CodecNodes {
        codec: "opus",
        media: EncodedMedia::Audio,
        encoder: "OpusEncoder",
        decoder: "OpusDecoder",
    },
];

/// Whether a person converts toward the packets of a codec, or away from them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodecDirection {
    Encode,
    Decode,
}

/// The words placing the encoder or decoder, as `direction` says, of each codec of `media`
/// that `stated` lists, or of each when it lists none; `None` when no codec node fits. One
/// decoder takes one codec, so packets stating several are told to state one a decoder takes
/// first.
pub(crate) fn codec_nodes_placed_between(
    media: EncodedMedia,
    direction: CodecDirection,
    stated: &[String],
) -> Option<String> {
    let fitting: Vec<&CodecNodes> = CODEC_NODES
        .iter()
        .filter(|nodes| nodes.media == media)
        .filter(|nodes| {
            stated.is_empty()
                || stated
                    .iter()
                    .any(|codec| codec.eq_ignore_ascii_case(nodes.codec))
        })
        .collect();
    let node_of = |nodes: &CodecNodes| match direction {
        CodecDirection::Encode => nodes.encoder,
        CodecDirection::Decode => nodes.decoder,
    };
    let one_decoder_takes_every_codec_stated =
        direction == CodecDirection::Encode || stated.len() <= 1;
    match fitting.as_slice() {
        [] => None,
        [one] if one_decoder_takes_every_codec_stated => {
            Some(format!("place an {} node between them", node_of(one)))
        }
        several if one_decoder_takes_every_codec_stated => Some(format!(
            "place one of {} between them",
            several
                .iter()
                .map(|nodes| node_of(nodes))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        [one] => Some(format!(
            "no one decoder takes every codec the output states; narrow the output to {}, then \
             place an {} node between them",
            one.codec,
            node_of(one)
        )),
        several => Some(format!(
            "no one decoder takes every codec the output states; narrow the output to one of \
             {}, then place its decoder between them",
            several
                .iter()
                .map(|nodes| nodes.codec)
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[cfg(feature = "contract-json-schema")]
mod contract_json_schema;

#[cfg(test)]
mod tests;
