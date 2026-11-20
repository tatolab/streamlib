// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `data` message: one `ViewModelValues`, a JSON object from view-model
//! property path to value, each entry assigned to the property of that path by
//! the property's own type, in the order the object names them.

use std::collections::HashSet;

use serde_json::Value as JsonValue;

/// A view-model property's type, as rive-runtime's `DataType` numbers it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewModelPropertyType {
    String,
    Number,
    Boolean,
    Color,
    Enum,
    Trigger,
    /// A type a `data` message cannot set: a list, a nested view model, an
    /// asset or an artboard.
    NotSettableFromData(i32),
}

impl ViewModelPropertyType {
    /// The type `rive::DataType` numbers `data_type`.
    pub fn from_rive_data_type(data_type: i32) -> Self {
        match data_type {
            1 => Self::String,
            2 => Self::Number,
            3 => Self::Boolean,
            4 => Self::Color,
            6 => Self::Enum,
            7 => Self::Trigger,
            other => Self::NotSettableFromData(other),
        }
    }
}

/// What one entry does to the property it names.
#[derive(Debug, Clone, PartialEq)]
pub enum ViewModelPropertyAssignment {
    Number(f32),
    String(String),
    Boolean(bool),
    /// `0xAARRGGBB`.
    Color(u32),
    Enum(String),
    FireTrigger,
}

impl ViewModelPropertyAssignment {
    /// The assignment `value` makes to a property of `property_type`, or why
    /// it makes none.
    pub fn for_property_of_type(
        property_type: ViewModelPropertyType,
        value: &JsonValue,
    ) -> Result<Option<Self>, String> {
        match (property_type, value) {
            (ViewModelPropertyType::Number, JsonValue::Number(number)) => Ok(Some(Self::Number(
                number.as_f64().unwrap_or_default() as f32,
            ))),
            (ViewModelPropertyType::String, JsonValue::String(text)) => {
                Ok(Some(Self::String(text.clone())))
            }
            (ViewModelPropertyType::Boolean, JsonValue::Bool(flag)) => {
                Ok(Some(Self::Boolean(*flag)))
            }
            (ViewModelPropertyType::Color, JsonValue::Number(argb)) => argb
                .as_u64()
                .and_then(|argb| u32::try_from(argb).ok())
                .map(|argb| Some(Self::Color(argb)))
                .ok_or_else(|| "a colour is an unsigned 0xAARRGGBB".to_owned()),
            (ViewModelPropertyType::Color, JsonValue::String(text)) => argb_of_a_hex_colour(text)
                .map(|argb| Some(Self::Color(argb)))
                .ok_or_else(|| "a colour is written #RRGGBB or #AARRGGBB".to_owned()),
            (ViewModelPropertyType::Enum, JsonValue::String(text)) => {
                Ok(Some(Self::Enum(text.clone())))
            }
            (ViewModelPropertyType::Trigger, JsonValue::Bool(false)) => Ok(None),
            (ViewModelPropertyType::Trigger, _) => Ok(Some(Self::FireTrigger)),
            (ViewModelPropertyType::NotSettableFromData(data_type), _) => Err(format!(
                "its type (rive DataType {data_type}) is not one a data message sets"
            )),
            (property_type, value) => Err(format!(
                "a {property_type:?} property cannot take the value {value}"
            )),
        }
    }
}

/// `#RRGGBB` (opaque) or `#AARRGGBB` as `0xAARRGGBB`.
fn argb_of_a_hex_colour(text: &str) -> Option<u32> {
    let hex_digits = text.strip_prefix('#')?;
    let value = u32::from_str_radix(hex_digits, 16).ok()?;
    match hex_digits.len() {
        6 => Some(0xFF00_0000 | value),
        8 => Some(value),
        _ => None,
    }
}

/// The view-model properties a `data` message is applied to.
pub trait ViewModelPropertyTarget {
    /// The type of the property at `property_path`, or `None` when the path
    /// names none.
    fn view_model_property_type(&self, property_path: &str) -> Option<ViewModelPropertyType>;

    /// Make `assignment` to the property at `property_path`; `false` when it
    /// was refused.
    fn assign_view_model_property(
        &mut self,
        property_path: &str,
        assignment: &ViewModelPropertyAssignment,
    ) -> bool;
}

/// The entries of one `data` message: its object's keys and values, in order.
pub fn entries_of_a_data_message(payload: &[u8]) -> Result<Vec<(String, JsonValue)>, String> {
    let message: JsonValue = serde_json::from_slice(payload)
        .map_err(|unreadable| format!("not one JSON object: {unreadable}"))?;
    let JsonValue::Object(entries) = message else {
        return Err(format!("a JSON {}, not an object", value_kind(&message)));
    };
    Ok(entries.into_iter().collect())
}

fn value_kind(value: &JsonValue) -> &'static str {
    match value {
        JsonValue::Null => "null",
        JsonValue::Bool(_) => "boolean",
        JsonValue::Number(_) => "number",
        JsonValue::String(_) => "string",
        JsonValue::Array(_) => "array",
        JsonValue::Object(_) => "object",
    }
}

/// Applies `data` messages to a scene's view model, reporting each path that
/// cannot be applied once and skipping it.
#[derive(Debug, Default)]
pub struct ViewModelDataMessageApplier {
    property_paths_already_reported: HashSet<String>,
}

impl ViewModelDataMessageApplier {
    /// Apply every entry of `payload` to `target`. Answers the reports to log:
    /// a malformed message, and the first failure of each path.
    pub fn apply(
        &mut self,
        payload: &[u8],
        target: &mut impl ViewModelPropertyTarget,
    ) -> Vec<String> {
        let entries = match entries_of_a_data_message(payload) {
            Ok(entries) => entries,
            Err(malformed) => {
                return vec![format!(
                    "RiveSource: a data message was skipped: it is {malformed}"
                )];
            }
        };
        let mut reports = Vec::new();
        for (property_path, value) in entries {
            if let Err(failure) = Self::apply_one_entry(target, &property_path, &value)
                && self
                    .property_paths_already_reported
                    .insert(property_path.clone())
            {
                reports.push(format!(
                    "RiveSource: view-model property {property_path:?} is skipped: {failure}; \
                     later messages naming it are skipped without a report"
                ));
            }
        }
        reports
    }

    fn apply_one_entry(
        target: &mut impl ViewModelPropertyTarget,
        property_path: &str,
        value: &JsonValue,
    ) -> Result<(), String> {
        let property_type = target
            .view_model_property_type(property_path)
            .ok_or_else(|| {
                "the bound view-model instance has no property at that path".to_owned()
            })?;
        let Some(assignment) =
            ViewModelPropertyAssignment::for_property_of_type(property_type, value)?
        else {
            return Ok(());
        };
        if target.assign_view_model_property(property_path, &assignment) {
            Ok(())
        } else {
            Err(format!("rive-runtime refused {assignment:?}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct RecordedViewModel {
        property_types: HashMap<&'static str, ViewModelPropertyType>,
        assignments: Vec<(String, ViewModelPropertyAssignment)>,
    }

    impl ViewModelPropertyTarget for RecordedViewModel {
        fn view_model_property_type(&self, property_path: &str) -> Option<ViewModelPropertyType> {
            self.property_types.get(property_path).copied()
        }

        fn assign_view_model_property(
            &mut self,
            property_path: &str,
            assignment: &ViewModelPropertyAssignment,
        ) -> bool {
            if matches!(assignment, ViewModelPropertyAssignment::Enum(value) if value == "unknown")
            {
                return false;
            }
            self.assignments
                .push((property_path.to_owned(), assignment.clone()));
            true
        }
    }

    fn lower_third_view_model() -> RecordedViewModel {
        RecordedViewModel {
            property_types: HashMap::from([
                ("title", ViewModelPropertyType::String),
                ("progress", ViewModelPropertyType::Number),
                ("visible", ViewModelPropertyType::Boolean),
                ("accent", ViewModelPropertyType::Color),
                ("variant", ViewModelPropertyType::Enum),
                ("reveal", ViewModelPropertyType::Trigger),
                ("speaker/name", ViewModelPropertyType::String),
                ("items", ViewModelPropertyType::NotSettableFromData(5)),
            ]),
            ..Default::default()
        }
    }

    fn data_message(values: JsonValue) -> Vec<u8> {
        values.to_string().into_bytes()
    }

    #[test]
    fn each_entry_sets_the_property_of_its_path_by_that_propertys_type() {
        let mut view_model = lower_third_view_model();
        let reports = ViewModelDataMessageApplier::default().apply(
            &data_message(serde_json::json!({
                "title": "Jane Doe",
                "progress": 0.5,
                "visible": true,
                "accent": "#FF8800",
                "variant": "breaking",
                "reveal": null,
                "speaker/name": "Ada",
            })),
            &mut view_model,
        );
        assert!(reports.is_empty(), "{reports:?}");
        assert_eq!(
            view_model.assignments,
            vec![
                (
                    "title".into(),
                    ViewModelPropertyAssignment::String("Jane Doe".into())
                ),
                ("progress".into(), ViewModelPropertyAssignment::Number(0.5)),
                ("visible".into(), ViewModelPropertyAssignment::Boolean(true)),
                (
                    "accent".into(),
                    ViewModelPropertyAssignment::Color(0xFFFF_8800)
                ),
                (
                    "variant".into(),
                    ViewModelPropertyAssignment::Enum("breaking".into())
                ),
                ("reveal".into(), ViewModelPropertyAssignment::FireTrigger),
                (
                    "speaker/name".into(),
                    ViewModelPropertyAssignment::String("Ada".into())
                ),
            ]
        );
    }

    #[test]
    fn an_integer_sets_a_number_and_a_colour_takes_argb() {
        let mut view_model = lower_third_view_model();
        ViewModelDataMessageApplier::default().apply(
            &data_message(serde_json::json!({
                "progress": 3,
                "accent": 0x80FF_0000_u32,
            })),
            &mut view_model,
        );
        assert_eq!(
            view_model.assignments,
            vec![
                ("progress".into(), ViewModelPropertyAssignment::Number(3.0)),
                (
                    "accent".into(),
                    ViewModelPropertyAssignment::Color(0x80FF_0000)
                ),
            ]
        );
    }

    #[test]
    fn a_missing_path_is_reported_once_and_skipped_while_the_rest_apply() {
        let mut view_model = lower_third_view_model();
        let mut applier = ViewModelDataMessageApplier::default();
        let message = data_message(serde_json::json!({"subtitle": "x", "title": "kept"}));
        let first_reports = applier.apply(&message, &mut view_model);
        let second_reports = applier.apply(&message, &mut view_model);
        assert_eq!(first_reports.len(), 1);
        assert!(
            first_reports[0].contains("\"subtitle\""),
            "{first_reports:?}"
        );
        assert!(second_reports.is_empty(), "{second_reports:?}");
        assert_eq!(view_model.assignments.len(), 2);
    }

    #[test]
    fn a_value_the_propertys_type_cannot_take_is_reported_and_skipped() {
        let mut view_model = lower_third_view_model();
        let reports = ViewModelDataMessageApplier::default().apply(
            &data_message(serde_json::json!({
                "progress": "half",
                "items": 1,
                "accent": "orange",
                "variant": "unknown",
            })),
            &mut view_model,
        );
        assert_eq!(reports.len(), 4, "{reports:?}");
        assert!(view_model.assignments.is_empty());
    }

    #[test]
    fn a_false_trigger_fires_nothing() {
        let mut view_model = lower_third_view_model();
        let reports = ViewModelDataMessageApplier::default().apply(
            &data_message(serde_json::json!({"reveal": false})),
            &mut view_model,
        );
        assert!(reports.is_empty());
        assert!(view_model.assignments.is_empty());
    }

    #[test]
    fn a_message_that_is_not_an_object_is_reported_whole() {
        let reports =
            ViewModelDataMessageApplier::default().apply(b"7", &mut lower_third_view_model());
        assert_eq!(reports.len(), 1);
        assert!(reports[0].contains("not an object"), "{reports:?}");
    }

    #[test]
    fn entries_apply_in_the_order_the_object_names_them() {
        let entries = entries_of_a_data_message(br#"{"z":1,"a":2,"m":3}"#).unwrap();
        let paths: Vec<&str> = entries.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(paths, ["z", "a", "m"]);
    }
}
