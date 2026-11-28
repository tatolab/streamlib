// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The facts a port states of its stream beside its pin type: a Texture's `formats` and
//! `frame_rate`, an Audio output's `rate` and `dtype`, a Tensor output's `residency`, each a
//! literal or, for a rate, read `from_input(<value input>)`.

use streamlib_node_schema::{
    ArrayOutputPortResidency, ChannelsOutputPortSampleRate, ChannelsOutputPortSampleType,
    ImageOutputPortFrameRate,
    pin_type::{PinAnnotations, PinType},
};
use syn::parse::ParseStream;
use syn::{LitInt, LitStr};

use super::PortDirection;
use super::pin_type_chosen_by_an_input::{
    PinTypeChosenBy, StreamFactReadFromAValueInput, parse_the_input_choosing_a_pin_type,
};
use super::port_grammar::peeks_a_call;
use super::string_literal_list::parse_string_literal_list;

/// Parse `from_input(<value input>)`, when that is what follows, into the value input the fact
/// is read from.
fn parse_the_input_a_fact_is_read_from(
    content: ParseStream<'_>,
) -> syn::Result<Option<PinTypeChosenBy>> {
    if !peeks_a_call(content) {
        return Ok(None);
    }
    parse_the_input_choosing_a_pin_type(content, |function| {
        format!(
            "expected `from_input(<value input>)`, found `{function}(…)` — a fact a port \
             states of its stream is a number, or read from a value input of its node"
        )
    })
    .map(Some)
}

/// The stream facts a port states beside its pin type: the pixel formats of a Texture, and the
/// annotations of an output's pin.
#[derive(Default)]
pub(super) struct StreamFactsStated {
    pub(super) formats: Vec<String>,
    pub(super) annotations: PinAnnotations,
    pub(super) read_from_value_inputs: Vec<StreamFactReadFromAValueInput>,
}

impl StreamFactsStated {
    /// The first fact stated that a port of `pin_type`, at `direction`, does not take, as a
    /// refusal naming the port and the key; a port typed by a setting takes none.
    pub(super) fn refusal_of_a_fact_the_port_does_not_take(
        &self,
        port_name: &str,
        direction: PortDirection,
        pin_type: Option<&PinType>,
    ) -> Option<String> {
        let is_an_output = direction == PortDirection::Output;
        let is_a = |wanted: fn(&PinType) -> bool| pin_type.is_some_and(wanted);
        let is_a_texture = is_a(|pin_type| matches!(pin_type, PinType::Texture { .. }));
        let is_audio = is_a(|pin_type| matches!(pin_type, PinType::Audio {}));
        let is_a_tensor = is_a(|pin_type| matches!(pin_type, PinType::Tensor {}));
        let stated_keys: [(&str, bool, bool); 5] = [
            ("formats", !self.formats.is_empty(), is_a_texture),
            (
                "frame_rate",
                self.annotations.frame_rate.is_some(),
                is_an_output && is_a_texture,
            ),
            (
                "rate",
                self.annotations.sample_rate.is_some(),
                is_an_output && is_audio,
            ),
            (
                "dtype",
                self.annotations.sample_type.is_some(),
                is_an_output && is_audio,
            ),
            (
                "residency",
                self.annotations.residency.is_some(),
                is_an_output && is_a_tensor,
            ),
        ];
        let direction_word = match direction {
            PortDirection::Input => "input",
            PortDirection::Output => "output",
        };
        let pin_type_words = pin_type.map_or_else(
            || "a type a setting chooses".to_owned(),
            |pin_type| format!("{pin_type}"),
        );
        stated_keys
            .iter()
            .find(|(_, stated, taken)| *stated && !*taken)
            .map(|(key, _, _)| {
                format!(
                    "{direction_word} port {port_name:?} states `{key}=`, which an \
                     {direction_word} of {pin_type_words} does not take"
                )
            })
    }
}

/// Parse one stream fact key's value into `stream_facts`; which pin type and direction take it
/// is checked once the whole port is read.
pub(super) fn parse_stream_fact_key(
    content: ParseStream<'_>,
    fact_key: &str,
    port_name: &str,
    stream_facts: &mut StreamFactsStated,
    key_span: proc_macro2::Span,
) -> syn::Result<()> {
    let StreamFactsStated {
        formats,
        annotations,
        read_from_value_inputs,
    } = stream_facts;
    match fact_key {
        "formats" => *formats = parse_string_literal_list(content, "formats")?,
        "rate" => {
            annotations.sample_rate = Some(match parse_the_input_a_fact_is_read_from(content)? {
                Some(read_from) => {
                    let setting = read_from.cast_name.clone();
                    read_from_value_inputs.push(StreamFactReadFromAValueInput {
                        fact_key: "rate",
                        read_from,
                    });
                    ChannelsOutputPortSampleRate::FromSetting { setting }
                }
                None => {
                    let rate: LitInt = content.parse()?;
                    ChannelsOutputPortSampleRate::Stated(rate.base10_parse()?)
                }
            });
        }
        "frame_rate" => {
            annotations.frame_rate = Some(match parse_the_input_a_fact_is_read_from(content)? {
                Some(read_from) => {
                    let setting = read_from.cast_name.clone();
                    read_from_value_inputs.push(StreamFactReadFromAValueInput {
                        fact_key: "frame_rate",
                        read_from,
                    });
                    ImageOutputPortFrameRate::FromSetting { setting }
                }
                None => {
                    let frames_per_second: LitInt = content.parse()?;
                    ImageOutputPortFrameRate::Stated(streamlib_node_schema::ImageStreamFrameRate {
                        numerator: frames_per_second.base10_parse()?,
                        denominator: 1,
                    })
                }
            });
        }
        "dtype" => {
            let dtype: LitStr = content.parse()?;
            annotations.sample_type = Some(match dtype.value().as_str() {
                "f32" => ChannelsOutputPortSampleType::F32,
                "f64" => ChannelsOutputPortSampleType::F64,
                other => {
                    return Err(syn::Error::new(
                        dtype.span(),
                        format!("port `{port_name}`: `dtype` is \"f32\" or \"f64\", not {other:?}"),
                    ));
                }
            });
        }
        "residency" => {
            let residency: LitStr = content.parse()?;
            annotations.residency = Some(match residency.value().as_str() {
                "gpu" => ArrayOutputPortResidency::Gpu,
                "cpu" => ArrayOutputPortResidency::Cpu,
                other => {
                    return Err(syn::Error::new(
                        residency.span(),
                        format!(
                            "port `{port_name}`: `residency` is \"gpu\" or \"cpu\", not {other:?}"
                        ),
                    ));
                }
            });
        }
        other => {
            return Err(syn::Error::new(
                key_span,
                format!("port `{port_name}`: `{other}` is not a stream fact key"),
            ));
        }
    }
    Ok(())
}
