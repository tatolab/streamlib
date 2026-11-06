// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The five port families a port declares, the keys each takes, and the
//! checks `connect` makes between an output and an input of one family.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

/// The memory layout a port carries; a link joins two ports of one family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum PortFamily {
    Image,
    Array,
    Channels,
    Packets,
    Records,
}

impl PortFamily {
    /// Every family, in declaration order.
    pub const ALL: [PortFamily; 5] = [
        PortFamily::Image,
        PortFamily::Array,
        PortFamily::Channels,
        PortFamily::Packets,
        PortFamily::Records,
    ];

    /// The name a declaration spells this family by.
    pub fn declaration_name(self) -> &'static str {
        match self {
            PortFamily::Image => "Image",
            PortFamily::Array => "Array",
            PortFamily::Channels => "Channels",
            PortFamily::Packets => "Packets",
            PortFamily::Records => "Records",
        }
    }

    /// Whether a port of this family states names: channel names, attribute
    /// names or record keys.
    pub fn has_names(self) -> bool {
        matches!(
            self,
            PortFamily::Array | PortFamily::Channels | PortFamily::Records
        )
    }
}

impl fmt::Display for PortFamily {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.declaration_name())
    }
}

/// The names an output port states: written on the class, or read from one
/// key of its node's config when `connect` resolves them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum OutputPortDeclaredNames {
    Stated(Vec<String>),
    FromConfig { config_key: String },
}

/// The sample rate a Channels output states, written on the class or read
/// from its node's config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ChannelsOutputPortSampleRate {
    Stated(u32),
    FromConfig { config_key: String },
}

/// The sample type of a Channels output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ChannelsOutputPortSampleType {
    F32,
    F64,
}

/// Where an Array output's attributes live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ArrayOutputPortResidency {
    Gpu,
    Cpu,
}

/// A port's family and the keys its family takes.
///
/// An output states `names`, `codec`, `rate`, `dtype`, `formats` and
/// `residency` as its family allows; an input states `requires`,
/// `requires_codec` and `formats`. [`Self::refusal_of_its_keys`] names a key
/// the family or the direction does not take.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct PortFamilyDeclaration {
    pub family: PortFamily,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<OutputPortDeclaredNames>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_codec: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate: Option<ChannelsOutputPortSampleRate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dtype: Option<ChannelsOutputPortSampleType>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formats: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub residency: Option<ArrayOutputPortResidency>,
}

/// Which end of a link a port declaration sits at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortFamilyDeclarationDirection {
    Input,
    Output,
}

impl PortFamilyDeclaration {
    /// A declaration of `family` with none of its keys stated.
    pub fn of_family(family: PortFamily) -> Self {
        Self {
            family,
            names: None,
            requires: Vec::new(),
            codec: None,
            requires_codec: Vec::new(),
            rate: None,
            dtype: None,
            formats: Vec::new(),
            residency: None,
        }
    }

    /// The first key this declaration states that its family or its
    /// direction does not take, as a refusal naming the port and the key.
    pub fn refusal_of_its_keys(
        &self,
        port_name: &str,
        direction: PortFamilyDeclarationDirection,
    ) -> Option<String> {
        let family = self.family;
        let stated_keys: [(&str, bool, bool); 9] = [
            (
                "names",
                self.names.is_some(),
                direction == PortFamilyDeclarationDirection::Output && family.has_names(),
            ),
            (
                "requires",
                !self.requires.is_empty(),
                direction == PortFamilyDeclarationDirection::Input && family.has_names(),
            ),
            (
                "codec",
                self.codec.is_some(),
                direction == PortFamilyDeclarationDirection::Output
                    && family == PortFamily::Packets,
            ),
            (
                "requires_codec",
                !self.requires_codec.is_empty(),
                direction == PortFamilyDeclarationDirection::Input && family == PortFamily::Packets,
            ),
            (
                "rate",
                self.rate.is_some(),
                direction == PortFamilyDeclarationDirection::Output
                    && family == PortFamily::Channels,
            ),
            (
                "dtype",
                self.dtype.is_some(),
                direction == PortFamilyDeclarationDirection::Output
                    && family == PortFamily::Channels,
            ),
            (
                "formats",
                !self.formats.is_empty(),
                family == PortFamily::Image,
            ),
            (
                "residency",
                self.residency.is_some(),
                direction == PortFamilyDeclarationDirection::Output && family == PortFamily::Array,
            ),
            ("family", false, true),
        ];
        let direction_word = match direction {
            PortFamilyDeclarationDirection::Input => "input",
            PortFamilyDeclarationDirection::Output => "output",
        };
        stated_keys
            .iter()
            .find(|(_, stated, taken)| *stated && !*taken)
            .map(|(key, _, _)| {
                format!(
                    "{direction_word} port {port_name:?} of family {family} states `{key}=`, \
                     which a {family} {direction_word} does not take"
                )
            })
            .or_else(|| {
                self.formats
                    .iter()
                    .find(|format| !IMAGE_TEXTURE_FORMAT_WIRE_NAMES.contains(&format.as_str()))
                    .map(|format| {
                        format!(
                            "{direction_word} port {port_name:?} states format {format:?}; \
                             `formats=` takes the engine's texture format names: {}",
                            IMAGE_TEXTURE_FORMAT_WIRE_NAMES.join(", ")
                        )
                    })
            })
    }
}

/// The texture format wire names an Image port's `formats=` takes: every
/// `TextureFormat` the engine's RHI defines, spelled as the escalate wire spells it.
pub const IMAGE_TEXTURE_FORMAT_WIRE_NAMES: [&str; 7] = [
    "rgba8_unorm",
    "rgba8_unorm_srgb",
    "bgra8_unorm",
    "bgra8_unorm_srgb",
    "rgba16_float",
    "rgba32_float",
    "nv12",
];

/// One end of a link, as a refusal names it.
#[derive(Debug, Clone, Copy)]
pub struct PortFamilyLinkEndpoint<'a> {
    pub node_name: &'a str,
    pub port_name: &'a str,
    pub declaration: Option<&'a PortFamilyDeclaration>,
}

impl fmt::Display for PortFamilyLinkEndpoint<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.node_name, self.port_name)
    }
}

/// The names an output's declaration resolves to against its node's config,
/// or why they cannot be resolved; `None` when the output states no names.
pub fn output_port_names_resolved_against_config(
    names: Option<&OutputPortDeclaredNames>,
    node_config: Option<&serde_json::Value>,
) -> Option<Result<Vec<String>, String>> {
    match names? {
        OutputPortDeclaredNames::Stated(names) => Some(Ok(names.clone())),
        OutputPortDeclaredNames::FromConfig { config_key } => {
            let value = node_config.and_then(|config| config.get(config_key));
            Some(match value {
                Some(serde_json::Value::Array(entries)) => entries
                    .iter()
                    .map(|entry| {
                        entry.as_str().map(str::to_owned).ok_or_else(|| {
                            format!(
                                "config key {config_key:?} must be a list of strings, and holds \
                                 {entry}"
                            )
                        })
                    })
                    .collect(),
                Some(other) => Err(format!(
                    "config key {config_key:?} must be a list of strings, and holds {other}"
                )),
                None => Err(format!(
                    "the names come from config key {config_key:?}, which the node's config \
                     does not set"
                )),
            })
        }
    }
}

/// The node a person places between `from` and `to` to convert one family to
/// the other.
fn conversion_between_families(from: PortFamily, to: PortFamily) -> &'static str {
    use PortFamily::*;
    match (from, to) {
        (Image, Array) => "place an ImageToArray node between them",
        (Array, Image) => "place an ArrayToImage node between them",
        (Channels, Array) => "place a ChannelsToArray node between them",
        (Array, Channels) => "place an ArrayToChannels node between them",
        (Channels, Records) => "place a ChannelsToRecords node between them",
        (Records, Channels) => "place a RecordsToChannels node between them",
        (Image, Packets) => "place a video encoder (H264Encoder, H265Encoder) between them",
        (Packets, Image) => "place a video decoder (H264Decoder, H265Decoder) between them",
        (Channels, Packets) => "place an OpusEncoder between them",
        (Packets, Channels) => "place an OpusDecoder between them",
        _ => "no one node converts between them; chain the conversion nodes that do",
    }
}

/// Why `connect` refuses a link from `output` to `input`, or `None` when it
/// takes it.
///
/// Families compare by equality, then the input's `requires=` against the
/// output's resolved names, then `requires_codec=` against its codec, then
/// the two `formats=` sets, which must overlap. A port with no family — a
/// decorator port, read as unchecked Records — takes any link.
pub fn refusal_of_a_link_between_port_families(
    output: PortFamilyLinkEndpoint<'_>,
    output_node_config: Option<&serde_json::Value>,
    input: PortFamilyLinkEndpoint<'_>,
) -> Option<String> {
    let (Some(output_declaration), Some(input_declaration)) =
        (output.declaration, input.declaration)
    else {
        return None;
    };
    let output_family = output_declaration.family;
    let input_family = input_declaration.family;
    if output_family != input_family {
        return Some(format!(
            "cannot link {output} ({output_family}) to {input} ({input_family}): a link joins \
             two ports of one family; {}",
            conversion_between_families(output_family, input_family)
        ));
    }
    if !input_declaration.requires.is_empty() {
        let resolved_names = output_port_names_resolved_against_config(
            output_declaration.names.as_ref(),
            output_node_config,
        );
        let output_names = match resolved_names {
            None => {
                return Some(format!(
                    "cannot link {output} to {input}: {input} requires {:?}, and {output} \
                     states no names",
                    input_declaration.requires
                ));
            }
            Some(Err(unresolved)) => {
                return Some(format!(
                    "cannot link {output} to {input}: {input} requires {:?}, and {output}'s \
                     names cannot be resolved: {unresolved}",
                    input_declaration.requires
                ));
            }
            Some(Ok(names)) => names,
        };
        if let Some(refusal) =
            refusal_of_required_names(output, &output_names, input, &input_declaration.requires)
        {
            return Some(refusal);
        }
    }
    if !input_declaration.requires_codec.is_empty() {
        let codec_matches = output_declaration
            .codec
            .as_ref()
            .is_some_and(|codec| input_declaration.requires_codec.contains(codec));
        if !codec_matches {
            return Some(format!(
                "cannot link {output} to {input}: {input} requires a codec in {:?}, and {output} \
                 states {}",
                input_declaration.requires_codec,
                output_declaration
                    .codec
                    .as_deref()
                    .map_or_else(|| "no codec".to_owned(), |codec| format!("{codec:?}"))
            ));
        }
    }
    if !output_declaration.formats.is_empty() && !input_declaration.formats.is_empty() {
        let output_formats: BTreeSet<&String> = output_declaration.formats.iter().collect();
        let formats_overlap = input_declaration
            .formats
            .iter()
            .any(|format| output_formats.contains(format));
        if !formats_overlap {
            return Some(format!(
                "cannot link {output} to {input}: {output} writes formats {:?}, and {input} \
                 reads only {:?}",
                output_declaration.formats, input_declaration.formats
            ));
        }
    }
    None
}

/// Why an output whose names resolve to `output_names` cannot feed an input
/// requiring `required_names`, naming each missing one.
pub fn refusal_of_required_names(
    output: PortFamilyLinkEndpoint<'_>,
    output_names: &[String],
    input: PortFamilyLinkEndpoint<'_>,
    required_names: &[String],
) -> Option<String> {
    let missing_names: Vec<&String> = required_names
        .iter()
        .filter(|required| !output_names.contains(required))
        .collect();
    if missing_names.is_empty() {
        return None;
    }
    Some(format!(
        "cannot link {output} to {input}: {input} requires {missing_names:?}, which {output} \
         does not state (it states {output_names:?})"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint<'a>(
        node_name: &'a str,
        port_name: &'a str,
        declaration: Option<&'a PortFamilyDeclaration>,
    ) -> PortFamilyLinkEndpoint<'a> {
        PortFamilyLinkEndpoint {
            node_name,
            port_name,
            declaration,
        }
    }

    fn channels_output_stating(names: &[&str]) -> PortFamilyDeclaration {
        PortFamilyDeclaration {
            names: Some(OutputPortDeclaredNames::Stated(
                names.iter().map(|name| name.to_string()).collect(),
            )),
            ..PortFamilyDeclaration::of_family(PortFamily::Channels)
        }
    }

    fn channels_input_requiring(names: &[&str]) -> PortFamilyDeclaration {
        PortFamilyDeclaration {
            requires: names.iter().map(|name| name.to_string()).collect(),
            ..PortFamilyDeclaration::of_family(PortFamily::Channels)
        }
    }

    #[test]
    fn a_family_mismatch_is_refused_naming_both_endpoints_and_the_conversion() {
        let image = PortFamilyDeclaration::of_family(PortFamily::Image);
        let array = PortFamilyDeclaration::of_family(PortFamily::Array);
        let refusal = refusal_of_a_link_between_port_families(
            endpoint("camera", "video", Some(&image)),
            None,
            endpoint("model", "tensor", Some(&array)),
        )
        .expect("an Image output never feeds an Array input");
        assert!(refusal.contains("camera.video (Image)"), "{refusal}");
        assert!(refusal.contains("model.tensor (Array)"), "{refusal}");
        assert!(refusal.contains("ImageToArray"), "{refusal}");
    }

    #[test]
    fn a_link_of_one_family_with_nothing_required_is_taken() {
        let image = PortFamilyDeclaration::of_family(PortFamily::Image);
        assert_eq!(
            refusal_of_a_link_between_port_families(
                endpoint("camera", "video", Some(&image)),
                None,
                endpoint("display", "video", Some(&image)),
            ),
            None
        );
    }

    #[test]
    fn a_decorator_port_at_either_end_takes_any_link() {
        let image = PortFamilyDeclaration::of_family(PortFamily::Image);
        assert_eq!(
            refusal_of_a_link_between_port_families(
                endpoint("camera", "video", Some(&image)),
                None,
                endpoint("legacy", "video", None),
            ),
            None
        );
        assert_eq!(
            refusal_of_a_link_between_port_families(
                endpoint("legacy", "video", None),
                None,
                endpoint("display", "video", Some(&image)),
            ),
            None
        );
    }

    #[test]
    fn a_required_name_the_output_does_not_state_is_refused_naming_it() {
        let output = channels_output_stating(&["left", "right"]);
        let input = channels_input_requiring(&["left", "rpm"]);
        let refusal = refusal_of_a_link_between_port_families(
            endpoint("mic", "audio", Some(&output)),
            None,
            endpoint("meter", "audio", Some(&input)),
        )
        .expect("rpm is not stated");
        assert!(
            refusal.contains("requires [\"rpm\"], which"),
            "only the missing name: {refusal}"
        );
        assert!(refusal.contains("mic.audio") && refusal.contains("meter.audio"));
    }

    #[test]
    fn a_requirement_on_an_output_stating_no_names_is_refused() {
        let output = PortFamilyDeclaration::of_family(PortFamily::Channels);
        let input = channels_input_requiring(&["left"]);
        let refusal = refusal_of_a_link_between_port_families(
            endpoint("mic", "audio", Some(&output)),
            None,
            endpoint("meter", "audio", Some(&input)),
        )
        .expect("an output with no names satisfies no requirement");
        assert!(refusal.contains("states no names"), "{refusal}");
    }

    #[test]
    fn names_from_config_resolve_against_the_output_nodes_config() {
        let output = PortFamilyDeclaration {
            names: Some(OutputPortDeclaredNames::FromConfig {
                config_key: "channel_names".into(),
            }),
            ..PortFamilyDeclaration::of_family(PortFamily::Channels)
        };
        let input = channels_input_requiring(&["rpm"]);
        let config = serde_json::json!({ "channel_names": ["rpm", "torque"] });
        assert_eq!(
            refusal_of_a_link_between_port_families(
                endpoint("engine", "signals", Some(&output)),
                Some(&config),
                endpoint("gauge", "signals", Some(&input)),
            ),
            None
        );
        let config_without_rpm = serde_json::json!({ "channel_names": ["torque"] });
        assert!(
            refusal_of_a_link_between_port_families(
                endpoint("engine", "signals", Some(&output)),
                Some(&config_without_rpm),
                endpoint("gauge", "signals", Some(&input)),
            )
            .is_some()
        );
        let refusal = refusal_of_a_link_between_port_families(
            endpoint("engine", "signals", Some(&output)),
            None,
            endpoint("gauge", "signals", Some(&input)),
        )
        .expect("unset config key cannot satisfy rpm");
        assert!(refusal.contains("channel_names"), "{refusal}");
    }

    #[test]
    fn a_codec_outside_the_required_set_is_refused() {
        let output = PortFamilyDeclaration {
            codec: Some("h265".into()),
            ..PortFamilyDeclaration::of_family(PortFamily::Packets)
        };
        let input = PortFamilyDeclaration {
            requires_codec: vec!["h264".into()],
            ..PortFamilyDeclaration::of_family(PortFamily::Packets)
        };
        let refusal = refusal_of_a_link_between_port_families(
            endpoint("encoder", "packets", Some(&output)),
            None,
            endpoint("publisher", "video", Some(&input)),
        )
        .expect("h265 is not h264");
        assert!(refusal.contains("\"h265\"") && refusal.contains("\"h264\""));
        assert!(refusal.contains("encoder.packets") && refusal.contains("publisher.video"));
    }

    #[test]
    fn image_formats_that_do_not_overlap_are_refused() {
        let output = PortFamilyDeclaration {
            formats: vec!["rgba16_float".into()],
            ..PortFamilyDeclaration::of_family(PortFamily::Image)
        };
        let input = PortFamilyDeclaration {
            formats: vec!["rgba8_unorm".into()],
            ..PortFamilyDeclaration::of_family(PortFamily::Image)
        };
        let refusal = refusal_of_a_link_between_port_families(
            endpoint("render", "video", Some(&output)),
            None,
            endpoint("compositor", "video", Some(&input)),
        )
        .expect("rgba16_float never reaches an rgba8_unorm-only input");
        assert!(refusal.contains("rgba16_float") && refusal.contains("rgba8_unorm"));
        let input_reading_both = PortFamilyDeclaration {
            formats: vec!["rgba8_unorm".into(), "rgba16_float".into()],
            ..PortFamilyDeclaration::of_family(PortFamily::Image)
        };
        assert_eq!(
            refusal_of_a_link_between_port_families(
                endpoint("render", "video", Some(&output)),
                None,
                endpoint("compositor", "video", Some(&input_reading_both)),
            ),
            None
        );
    }

    #[test]
    fn a_format_that_is_no_engine_texture_name_is_refused() {
        let image_in_shorthand = PortFamilyDeclaration {
            formats: vec!["rgba8".into()],
            ..PortFamilyDeclaration::of_family(PortFamily::Image)
        };
        let refusal = image_in_shorthand
            .refusal_of_its_keys("video", PortFamilyDeclarationDirection::Output)
            .expect("rgba8 is no engine texture name");
        assert!(refusal.contains("\"rgba8\"") && refusal.contains("rgba8_unorm"));
    }

    #[test]
    fn a_key_the_family_or_direction_does_not_take_is_refused_by_name() {
        let image_with_names = PortFamilyDeclaration {
            names: Some(OutputPortDeclaredNames::Stated(vec!["x".into()])),
            ..PortFamilyDeclaration::of_family(PortFamily::Image)
        };
        let refusal = image_with_names
            .refusal_of_its_keys("video", PortFamilyDeclarationDirection::Output)
            .expect("Image has no names");
        assert!(refusal.contains("`names=`") && refusal.contains("Image"));
        let input_with_codec = PortFamilyDeclaration {
            codec: Some("h264".into()),
            ..PortFamilyDeclaration::of_family(PortFamily::Packets)
        };
        assert!(
            input_with_codec
                .refusal_of_its_keys("packets", PortFamilyDeclarationDirection::Input)
                .is_some()
        );
        assert_eq!(
            channels_output_stating(&["left"])
                .refusal_of_its_keys("audio", PortFamilyDeclarationDirection::Output),
            None
        );
    }

    #[test]
    fn a_declaration_round_trips_through_its_wire_form() {
        let declaration = PortFamilyDeclaration {
            names: Some(OutputPortDeclaredNames::FromConfig {
                config_key: "channel_names".into(),
            }),
            rate: Some(ChannelsOutputPortSampleRate::Stated(48_000)),
            dtype: Some(ChannelsOutputPortSampleType::F32),
            ..PortFamilyDeclaration::of_family(PortFamily::Channels)
        };
        let wire = serde_json::to_value(&declaration).unwrap();
        assert_eq!(wire["family"], "channels");
        assert_eq!(wire["names"]["from_config"]["config_key"], "channel_names");
        assert_eq!(
            serde_json::from_value::<PortFamilyDeclaration>(wire).unwrap(),
            declaration
        );
    }
}
