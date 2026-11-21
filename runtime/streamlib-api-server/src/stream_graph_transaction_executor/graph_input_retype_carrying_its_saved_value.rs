// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `set_node_input_value` that chooses another `pin_type` for a top-level `GraphInput` saving a
//! value carries that value into the new type in the same transaction, as three value sets it
//! records: the saved value taken off, the type set, and the value the one conversion fitting
//! the two types casts, held within the Input's `range`, set again. With no such cast the value
//! gives way to the new type's default, which is no saved value, and the answer says why. One
//! undo puts back both the type and the value.

use streamlib::sdk::descriptors::{PinType, SavedValueRetyped, the_saved_value_retyped};
use streamlib::sdk::graph::graph_interface_resolution::{
    GRAPH_INPUT_VALUE_FORMER_NAME, GRAPH_INTERFACE_PIN_TYPE_SETTING,
    the_range_a_graph_input_config_declares,
};
use streamlib_project_repository::{GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_VALUE_PORT_NAME};

use super::*;

/// The saved value a retype of a `GraphInput` carries: the stream asset and node the request
/// names, the input the value is saved under, and what the answer names of it, among it the
/// value it saves next.
struct SavedValueARetypeCarries {
    stream: Uuid,
    node: String,
    held_under: String,
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
        let Some(carried) = self.the_saved_value_a_retype_carries(&request, stream_id, staged)
        else {
            return self
                .apply_one_request(request, stream_id, staged, InputValueCheck::MadeOnApply)
                .map(|applied| vec![applied]);
        };
        let set = |input: &str, value: Option<Value>| StreamGraphAction::SetNodeInputValue {
            stream: carried.stream,
            level: Vec::new(),
            node: carried.node.clone(),
            input: input.to_owned(),
            member: None,
            value,
        };
        let mut sets = vec![set(&carried.held_under, None), request];
        if let Some(value) = &carried.answered.value {
            sets.push(set(GRAPH_INTERFACE_VALUE_PORT_NAME, Some(value.clone())));
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

    /// The saved value `request` carries into another type: `None` unless it sets a `pin_type`
    /// other than the one held on a top-level `GraphInput` the transaction has staged, which
    /// saves a value its new type is not to keep as it is, and the stream's conversions are read,
    /// so a request whose carry cannot be worked out is applied as it is sent.
    fn the_saved_value_a_retype_carries(
        &self,
        request: &StreamGraphAction,
        stream_id: &str,
        staged: &WhatTheTransactionHasStaged<'_>,
    ) -> Option<SavedValueARetypeCarries> {
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
            .ok()?;
        let answered = |value: Option<Value>| GraphTransactionSavedValueRetyped {
            stream: stream_id.to_owned(),
            node: node_uuid.to_string(),
            value_before: saved.clone(),
            value,
            cast_by: None,
            default_because: None,
            conversions_that_fit: Vec::new(),
        };
        let range = the_range_a_graph_input_config_declares(Some(&Value::Object(
            retyped.input_values.clone(),
        )));
        let answered =
            match the_saved_value_retyped(saved, &from, &into, range.as_ref(), &conversions) {
                SavedValueRetyped::TakenAsItIs => return None,
                SavedValueRetyped::CastBy { conversion, value } => {
                    GraphTransactionSavedValueRetyped {
                        cast_by: Some(conversion.to_string()),
                        ..answered(Some(value))
                    }
                }
                SavedValueRetyped::TakesTheDefault(why) => GraphTransactionSavedValueRetyped {
                    default_because: Some(why.to_string()),
                    conversions_that_fit: why
                        .conversions_that_fit()
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    ..answered(None)
                },
            };
        Some(SavedValueARetypeCarries {
            stream: *stream,
            node: node.clone(),
            held_under: held_under.to_owned(),
            answered,
        })
    }
}
