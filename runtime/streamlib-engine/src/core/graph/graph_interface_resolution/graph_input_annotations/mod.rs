// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The annotations of a top-level `GraphInput`'s `value` input, each held by a setting of its
//! own: read from its settings, so `graph` serves them on that input, and checked against the
//! pin type it chooses.

use super::{
    GRAPH_INPUT_CONTROL_SETTING, GRAPH_INPUT_MEANING_SETTING, GRAPH_INPUT_RANGE_CONFIG_KEY,
    GRAPH_INPUT_STEP_SETTING, GRAPH_INPUT_UNIT_SETTING, GRAPH_INTERFACE_PIN_TYPE_SETTING,
    GRAPH_INTERFACE_VALUE_PORT_NAME,
};
use crate::core::descriptors::{
    DeclaredPinType, PinAnnotationName, PinAnnotations, PinNumberRange, PinType, built_in_pin_type,
};
use crate::core::graph::graph_snapshot::GraphSnapshotNode;
use crate::core::{Error, Result};

/// Each setting of a top-level `GraphInput` stating one annotation of its `value` input, by the
/// annotation's name, which is the setting's name too.
const THE_ANNOTATION_SETTINGS_OF_A_GRAPH_INPUT: [(&str, PinAnnotationName); 5] = [
    (GRAPH_INPUT_RANGE_CONFIG_KEY, PinAnnotationName::Range),
    (GRAPH_INPUT_STEP_SETTING, PinAnnotationName::Step),
    (GRAPH_INPUT_UNIT_SETTING, PinAnnotationName::Unit),
    (GRAPH_INPUT_MEANING_SETTING, PinAnnotationName::Meaning),
    (GRAPH_INPUT_CONTROL_SETTING, PinAnnotationName::Control),
];

/// The name of each setting of a top-level `GraphInput` stating an annotation of its `value`.
pub(super) fn the_annotation_settings_of_a_graph_input() -> impl Iterator<Item = &'static str> {
    THE_ANNOTATION_SETTINGS_OF_A_GRAPH_INPUT
        .into_iter()
        .map(|(setting, _)| setting)
}

/// Whether `setting` is a setting of a top-level `GraphInput` stating an annotation of its
/// `value`.
pub(crate) fn is_an_annotation_setting_of_a_graph_input(setting: &str) -> bool {
    the_annotation_settings_of_a_graph_input()
        .any(|annotation_setting| annotation_setting == setting)
}

/// The range a `GraphInput`'s `config` declares a number of its `value` takes, as
/// `[lowest, highest]`; `None` when it declares none.
pub fn the_range_a_graph_input_config_declares(
    config: Option<&serde_json::Value>,
) -> Option<PinNumberRange> {
    match config?
        .get(GRAPH_INPUT_RANGE_CONFIG_KEY)?
        .as_array()?
        .as_slice()
    {
        [lowest, highest] => Some(PinNumberRange {
            minimum: lowest.as_f64(),
            maximum: highest.as_f64(),
        }),
        _ => None,
    }
}

/// The annotations a `GraphInput`'s `config` states of its `value` input, each from the setting
/// [`THE_ANNOTATION_SETTINGS_OF_A_GRAPH_INPUT`] names for it; a setting it holds no value of,
/// or one it cannot read, states none, and so does each annotation no setting of it holds.
pub(super) fn the_annotations_a_graph_input_config_states(
    config: Option<&serde_json::Value>,
) -> PinAnnotations {
    let held = |setting: &str| config.and_then(|config| config.get(setting));
    let held_text = |setting: &str| {
        held(setting)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    // Every field is named, so an annotation added to a pin is decided on here.
    PinAnnotations {
        range: the_range_a_graph_input_config_declares(config),
        step: held(GRAPH_INPUT_STEP_SETTING).and_then(serde_json::Value::as_f64),
        unit: held_text(GRAPH_INPUT_UNIT_SETTING),
        meaning: held_text(GRAPH_INPUT_MEANING_SETTING),
        control: held_text(GRAPH_INPUT_CONTROL_SETTING),
        frame_rate: None,
        sample_rate: None,
        sample_type: None,
        residency: None,
        project_asset: None,
    }
}

/// The pin type the `GraphInput` node type declares for its setting `setting`.
fn the_pin_type_the_setting_declares(setting: &str) -> Option<PinType> {
    crate::core::nodes::the_settings_of_a_graph_input()
        .iter()
        .find(|declared| declared.name == setting)
        .and_then(|declared| declared.declared_pin())
        .and_then(|declared| match declared.pin_type {
            DeclaredPinType::Is { pin_type } => Some(pin_type),
            DeclaredPinType::OneOf { .. }
            | DeclaredPinType::Any
            | DeclaredPinType::SameAsInput { .. }
            | DeclaredPinType::SetByInput { .. } => None,
        })
}

/// What a value `held` by the setting stating `annotation`, of the type `declared`, must be
/// when it is not that: what `declared` takes, or for a step, a number above 0; `None` when it
/// is of its form.
fn the_form_a_held_annotation_is_not_of(
    annotation: PinAnnotationName,
    declared: Option<&PinType>,
    held: &serde_json::Value,
) -> Option<String> {
    if let Some(Err(not_taken)) = declared.map(|declared| declared.check_value(held)) {
        return Some(not_taken.takes);
    }
    match annotation {
        PinAnnotationName::Step => {
            (!held.as_f64().is_some_and(|step| step > 0.0)).then(|| "a number above 0".to_string())
        }
        // Its declared type alone judges each of these; the order of a range's bounds is
        // judged with its family.
        PinAnnotationName::Range
        | PinAnnotationName::Unit
        | PinAnnotationName::Meaning
        | PinAnnotationName::Control => None,
        // No setting of a `GraphInput` holds one of these: the first four only an output
        // states, and an asset is picked by the Asset type.
        PinAnnotationName::FrameRate
        | PinAnnotationName::SampleRate
        | PinAnnotationName::SampleType
        | PinAnnotationName::Residency
        | PinAnnotationName::ProjectAsset => None,
    }
}

/// Refuse a setting of the `GraphInput` `node` stating an annotation of its `value` that the
/// `pin_type` it chooses does not take, naming the annotation and those the type takes, or one
/// that holds a value of another form; and one stating any annotation but `range` while it
/// chooses no pin type. A `range` on an Input that chooses no pin type is checked against its
/// family.
pub(super) fn refuse_an_annotation_the_chosen_type_does_not_take(
    node: &GraphSnapshotNode,
    pin_type: Option<&PinType>,
) -> Result<()> {
    for (setting, annotation) in THE_ANNOTATION_SETTINGS_OF_A_GRAPH_INPUT {
        let Some(held) = node
            .input_values
            .get(setting)
            .filter(|held| !held.is_null())
        else {
            continue;
        };
        let Some(pin_type) = pin_type else {
            if annotation == PinAnnotationName::Range {
                continue;
            }
            return Err(Error::GraphError(format!(
                "`GraphInput` node `{}` states `{setting}`, an annotation of its \
                 `{GRAPH_INTERFACE_VALUE_PORT_NAME}`, and chooses no \
                 `{GRAPH_INTERFACE_PIN_TYPE_SETTING}` that takes it. To fix: choose a \
                 `{GRAPH_INTERFACE_PIN_TYPE_SETTING}`, or clear `{setting}`",
                node.name
            )));
        };
        let row = built_in_pin_type(pin_type.built_in_name());
        if !row.annotations.contains(&annotation) {
            let taken: Vec<String> = THE_ANNOTATION_SETTINGS_OF_A_GRAPH_INPUT
                .iter()
                .filter(|(_, taken)| row.annotations.contains(taken))
                .map(|(taken, _)| format!("`{taken}`"))
                .collect();
            let it_takes = if taken.is_empty() {
                "none of them".to_string()
            } else {
                taken.join(", ")
            };
            return Err(Error::GraphError(format!(
                "`GraphInput` node `{}` carries {pin_type} and states `{setting}`, an annotation \
                 a {} pin does not take; of the annotations a `GraphInput` states, it takes \
                 {it_takes}. To fix: clear `{setting}`",
                node.name, row.label
            )));
        }
        if let Some(form) = the_form_a_held_annotation_is_not_of(
            annotation,
            the_pin_type_the_setting_declares(setting).as_ref(),
            held,
        ) {
            return Err(Error::GraphError(format!(
                "`GraphInput` node `{}` states its `{setting}` as {held}, and it takes {form}",
                node.name
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
