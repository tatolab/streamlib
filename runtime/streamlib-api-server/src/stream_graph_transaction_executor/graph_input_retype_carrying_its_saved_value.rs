// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `set_node_input_value` that chooses another `pin_type` for a top-level `GraphInput` saving a
//! value carries that value into the new type in the same transaction, as three value sets it
//! records: the saved value taken off, the type set, and the value the one conversion fitting
//! the two types casts, held within the Input's `range`, set again. With no such cast the value
//! gives way to the new type's default, which is no saved value, and the answer says why. One
//! undo puts back both the type and the value.

use streamlib::sdk::descriptors::{
    PinType, SavedValueRetyped, the_saved_value_retyped, the_value_held_within_the_range,
};
use streamlib::sdk::graph::graph_interface_resolution::{
    GRAPH_INPUT_RANGE_CONFIG_KEY, GRAPH_INPUT_VALUE_FORMER_NAME, GRAPH_INTERFACE_PIN_TYPE_SETTING,
};
use streamlib_project_repository::{GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_VALUE_PORT_NAME};

use super::*;

/// The saved value a retype of a `GraphInput` carries: the input it is saved under, and what
/// the answer names of it.
struct SavedValueARetypeCarries {
    held_under: String,
    value: Option<Value>,
    answered: GraphTransactionSavedValueRetyped,
}

impl StreamGraphTransactionExecutor<'_> {
    /// Apply `request` as [`Self::apply_one_request`] does; when it chooses another `pin_type`
    /// for a top-level `GraphInput` saving a value its new type is not to keep as it is, carry
    /// that value into the new type first, as the value sets this module names. One refused
    /// reverses those made live before it.
    pub(super) fn apply_a_set_carrying_a_retyped_saved_value(
        &self,
        request: StreamGraphAction,
        stream_id: &str,
        staged: &mut WhatTheTransactionHasStaged<'_>,
    ) -> Result<Vec<OneGraphRequestApplied>, GraphRequestRefusal> {
        let Some((carried, stream, node)) =
            self.the_saved_value_a_retype_carries(&request, stream_id, staged)
        else {
            return self
                .apply_one_request(request, stream_id, staged, InputValueCheck::MadeOnApply)
                .map(|applied| vec![applied]);
        };
        let set = |input: &str, value: Option<Value>| StreamGraphAction::SetNodeInputValue {
            stream,
            level: Vec::new(),
            node: node.clone(),
            input: input.to_owned(),
            member: None,
            value,
        };
        let mut sets = vec![set(&carried.held_under, None), request];
        if let Some(value) = carried.value {
            sets.push(set(GRAPH_INTERFACE_VALUE_PORT_NAME, Some(value)));
        }
        let mut applied_by_the_retype: Vec<OneGraphRequestApplied> = Vec::with_capacity(sets.len());
        for one_set in sets {
            match self.apply_one_request(one_set, stream_id, staged, InputValueCheck::MadeOnApply) {
                Ok(applied) => applied_by_the_retype.push(applied),
                Err(refusal) => {
                    let applied_live = applied_by_the_retype
                        .into_iter()
                        .filter_map(|applied| Some((stream_id.to_owned(), applied.applied_live?)))
                        .collect();
                    self.reverse_what_was_applied_live(&staged.streams, applied_live);
                    return Err(refusal);
                }
            }
        }
        if let Some(last) = applied_by_the_retype.last_mut() {
            last.saved_value_retyped = Some(carried.answered);
        }
        Ok(applied_by_the_retype)
    }

    /// The saved value `request` carries into another type, with its stream and node as the
    /// request names them: `None` unless it sets a `pin_type` other than the one held on a
    /// top-level `GraphInput` the transaction has staged, which saves a value its new type is
    /// not to keep as it is.
    fn the_saved_value_a_retype_carries(
        &self,
        request: &StreamGraphAction,
        stream_id: &str,
        staged: &WhatTheTransactionHasStaged<'_>,
    ) -> Option<(SavedValueARetypeCarries, Uuid, String)> {
        let StreamGraphAction::SetNodeInputValue {
            stream,
            level,
            node,
            input,
            member: None,
            value: Some(chosen),
        } = request
        else {
            return None;
        };
        if input != GRAPH_INTERFACE_PIN_TYPE_SETTING || !level.is_empty() {
            return None;
        }
        let node_uuid = staged
            .node_by_label
            .get(node)
            .copied()
            .or_else(|| Uuid::parse_str(node).ok())?;
        let retyped = self
            .the_level_a_request_reaches(
                stream_id,
                level,
                &staged.streams,
                &staged.staged_graph_of_each_stream_not_running,
                staged.project_repository,
            )
            .ok()?
            .the_document_as_drawn()
            .ok()?
            .node(node_uuid)
            .ok()?;
        if retyped.node_type != GRAPH_INPUT_NODE_TYPE {
            return None;
        }
        let (held_under, saved) = [
            GRAPH_INTERFACE_VALUE_PORT_NAME,
            GRAPH_INPUT_VALUE_FORMER_NAME,
        ]
        .into_iter()
        .find_map(|key| {
            retyped
                .input_values
                .get(key)
                .filter(|saved| !saved.is_null())
                .map(|saved| (key, saved))
        })?;
        let pin_type_of = |value: &Value| serde_json::from_value::<PinType>(value.clone()).ok();
        let from = pin_type_of(retyped.input_values.get(GRAPH_INTERFACE_PIN_TYPE_SETTING)?)?;
        let into = pin_type_of(chosen)?;
        if from == into {
            return None;
        }
        let conversions = self
            .operations_on_the_loaded_streams
            .pin_conversions_of_the_stream_a_call_names(stream_id)
            .unwrap_or_else(|_| Arc::from([]));
        let answered = |value: Option<Value>| GraphTransactionSavedValueRetyped {
            stream: stream_id.to_owned(),
            node: node_uuid.to_string(),
            value_before: saved.clone(),
            value,
            cast_by: None,
            default_because: None,
            conversions_that_fit: Vec::new(),
        };
        let carried = match the_saved_value_retyped(saved, &from, &into, &conversions) {
            SavedValueRetyped::TakenAsItIs => return None,
            SavedValueRetyped::CastBy { conversion, value } => {
                let value = match the_range_held_on(&retyped.input_values) {
                    Some(range) => the_value_held_within_the_range(value, &into, range),
                    None => value,
                };
                SavedValueARetypeCarries {
                    held_under: held_under.to_owned(),
                    value: Some(value.clone()),
                    answered: GraphTransactionSavedValueRetyped {
                        cast_by: Some(conversion.to_string()),
                        ..answered(Some(value))
                    },
                }
            }
            SavedValueRetyped::TakesTheDefault(why) => SavedValueARetypeCarries {
                held_under: held_under.to_owned(),
                value: None,
                answered: GraphTransactionSavedValueRetyped {
                    default_because: Some(why.to_string()),
                    conversions_that_fit: why
                        .conversions_that_fit()
                        .into_iter()
                        .map(ToString::to_string)
                        .collect(),
                    ..answered(None)
                },
            },
        };
        Some((carried, *stream, node.clone()))
    }
}

/// The `[lowest, highest]` a `GraphInput` holding `input_values` declares under `range`; `None`
/// when it declares none, or no two numbers.
fn the_range_held_on(input_values: &Map<String, Value>) -> Option<(f64, f64)> {
    match input_values
        .get(GRAPH_INPUT_RANGE_CONFIG_KEY)?
        .as_array()?
        .as_slice()
    {
        [lowest, highest] => Some((lowest.as_f64()?, highest.as_f64()?)),
        _ => None,
    }
}
