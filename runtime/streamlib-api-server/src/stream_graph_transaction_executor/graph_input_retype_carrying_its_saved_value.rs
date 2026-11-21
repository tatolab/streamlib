// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `set_node_input_value` that chooses another `pin_type` for a top-level `GraphInput` saving a
//! value carries that value into the new type in the same transaction, as three value sets it
//! records: the saved value taken off, the type set, and the value the one conversion fitting
//! the two types casts, held within the Input's `range`, set again. With no such cast the value
//! gives way to the new type's default, which is no saved value, and the answer says why. One
//! undo puts back both the type and the value.
//!
//! A module's conversion casts by running its body in the stream's interpreter, so each cast a
//! transaction asks for is worked out before the transaction takes the project's lock or opens
//! a stream's edit batch, from the documents as they stand then; the transaction takes the cast
//! worked out for the value and types it finds, and works out only one they no longer match.

use streamlib::sdk::descriptors::{
    PinNumberRange, PinType, SavedValueRetyped, the_saved_value_retyped,
};
use streamlib::sdk::graph::graph_interface_resolution::{
    GRAPH_INPUT_VALUE_FORMER_NAME, GRAPH_INTERFACE_PIN_TYPE_SETTING,
    the_range_a_graph_input_config_declares,
};
use streamlib_project_repository::{
    GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_VALUE_PORT_NAME, StreamGraphDocument,
    StreamGraphDocumentNode,
};

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

/// A retype of a `GraphInput` saving a value: the input the value is saved under, the value,
/// the type it is of, the type chosen and the Input's range.
#[derive(Clone, PartialEq)]
struct RetypeOfASavedValue {
    held_under: &'static str,
    saved: Value,
    from: PinType,
    into: PinType,
    range: Option<PinNumberRange>,
}

/// What became of each saved value a transaction retypes, worked out before it took any lock:
/// by the stream and node, the retype found then and what became of its value.
#[derive(Default)]
pub(super) struct SavedValueRetypesWorkedOutBeforeTheEdit {
    each: Vec<(String, Uuid, RetypeOfASavedValue, SavedValueRetyped)>,
}

impl SavedValueRetypesWorkedOutBeforeTheEdit {
    /// What became of `retype` of `node` in `stream_id`, if it was worked out for that retype.
    fn of(
        &self,
        stream_id: &str,
        node: Uuid,
        retype: &RetypeOfASavedValue,
    ) -> Option<SavedValueRetyped> {
        self.each
            .iter()
            .find(|(each_stream, each_node, each_retype, _)| {
                each_stream == stream_id && *each_node == node && each_retype == retype
            })
            .map(|(.., retyped)| retyped.clone())
    }
}

/// The stream asset, node and chosen type of `request` when it sets `pin_type` on a node of a
/// stream's own level; `None` for any other request.
fn what_a_retype_names(request: &StreamGraphAction) -> Option<(Uuid, &String, &Value)> {
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
    (input == GRAPH_INTERFACE_PIN_TYPE_SETTING && level.is_empty())
        .then_some((*stream, node, chosen))
}

/// The retype of the value `retyped` saves into `chosen`: `None` unless `retyped` is a
/// `GraphInput` saving a value of another type than `chosen`.
fn the_retype_of_what_a_node_saves(
    retyped: &StreamGraphDocumentNode,
    chosen: &Value,
) -> Option<RetypeOfASavedValue> {
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
            .map(|saved| (key, saved.clone()))
    })?;
    let pin_type_of = |value: &Value| serde_json::from_value::<PinType>(value.clone()).ok();
    let from = pin_type_of(retyped.input_values.get(GRAPH_INTERFACE_PIN_TYPE_SETTING)?)?;
    let into = pin_type_of(chosen)?;
    if from == into {
        return None;
    }
    let range =
        the_range_a_graph_input_config_declares(Some(&Value::Object(retyped.input_values.clone())));
    Some(RetypeOfASavedValue {
        held_under,
        saved,
        from,
        into,
        range,
    })
}

impl StreamGraphTransactionExecutor<'_> {
    /// What becomes of each saved value `requests` retype, each on the stream of the same place
    /// in `stream_id_of_each_request`, worked out against the documents as they stand: read
    /// under `project_repository`'s lock, taken only while they are read, or from each stream's
    /// live graph when there is none. A retype whose node or document cannot be read now is left
    /// for the transaction to work out.
    pub(super) fn the_saved_value_retypes_worked_out_before_the_edit(
        &self,
        requests: &[StreamGraphAction],
        stream_id_of_each_request: &[String],
        stream_locations: &HashMap<String, StreamLocation>,
        project_repository: Option<
            &streamlib_project_repository::ProjectRepositoryOneWriterAtATime,
        >,
    ) -> SavedValueRetypesWorkedOutBeforeTheEdit {
        let retypes: Vec<(&str, &String, &Value)> = requests
            .iter()
            .zip(stream_id_of_each_request)
            .filter_map(|(request, stream_id)| {
                let (_, node, chosen) = what_a_retype_names(request)?;
                Some((stream_id.as_str(), node, chosen))
            })
            .collect();
        if retypes.is_empty() {
            return SavedValueRetypesWorkedOutBeforeTheEdit::default();
        }
        let mut documents: HashMap<&str, Option<StreamGraphDocument>> = HashMap::new();
        {
            let project_repository_held = project_repository.map(|held| held.lock());
            for (stream_id, ..) in &retypes {
                documents.entry(stream_id).or_insert_with(|| {
                    match project_repository_held.as_deref() {
                        Some(project_repository) => project_repository
                            .stream_graph_document(stream_locations.get(*stream_id)?.stream_asset)
                            .ok(),
                        None => StreamGraphDocument::from_value(
                            self.async_runtime
                                .block_on(
                                    self.operations_on_the_loaded_streams
                                        .runtime_operations_of_the_stream_a_call_names(stream_id)
                                        .ok()?
                                        .to_json_async(),
                                )
                                .ok()?,
                        )
                        .ok(),
                    }
                });
            }
        }
        let mut worked_out = SavedValueRetypesWorkedOutBeforeTheEdit::default();
        for (stream_id, node, chosen) in retypes {
            let Some(node) = Uuid::parse_str(node).ok() else {
                continue;
            };
            let Some(retype) = documents
                .get(stream_id)
                .and_then(Option::as_ref)
                .and_then(|document| document.node(node).ok())
                .and_then(|retyped| the_retype_of_what_a_node_saves(&retyped, chosen))
            else {
                continue;
            };
            if let Some(retyped) = self.what_becomes_of_the_saved_value(stream_id, &retype) {
                worked_out
                    .each
                    .push((stream_id.to_owned(), node, retype, retyped));
            }
        }
        worked_out
    }

    /// What becomes of the value `retype` carries in the stream `stream_id`, by its conversions;
    /// `None` when they cannot be read.
    fn what_becomes_of_the_saved_value(
        &self,
        stream_id: &str,
        retype: &RetypeOfASavedValue,
    ) -> Option<SavedValueRetyped> {
        let conversions = self
            .operations_on_the_loaded_streams
            .pin_conversions_of_the_stream_a_call_names(stream_id)
            .ok()?;
        Some(the_saved_value_retyped(
            &retype.saved,
            &retype.from,
            &retype.into,
            retype.range.as_ref(),
            &conversions,
        ))
    }

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
    /// so a request whose carry cannot be worked out is applied as it is sent. What becomes of
    /// the value is the one worked out before the edit for the retype found here, and is worked
    /// out now only when none was.
    fn the_saved_value_a_retype_carries(
        &self,
        request: &StreamGraphAction,
        stream_id: &str,
        staged: &WhatTheTransactionHasStaged<'_>,
    ) -> Option<SavedValueARetypeCarries> {
        let (stream, node, chosen) = what_a_retype_names(request)?;
        let node_uuid = staged
            .node_by_label
            .get(node)
            .copied()
            .or_else(|| Uuid::parse_str(node).ok())?;
        let retyped = self
            .the_level_a_request_reaches(
                stream_id,
                &[],
                &staged.streams,
                &staged.staged_graph_of_each_stream_not_running,
                staged.project_repository,
            )
            .ok()?
            .the_document_as_drawn()
            .ok()?
            .node(node_uuid)
            .ok()?;
        let retype = the_retype_of_what_a_node_saves(&retyped, chosen)?;
        let what_became_of_it = match staged
            .saved_value_retypes_worked_out_before_the_edit
            .of(stream_id, node_uuid, &retype)
        {
            Some(worked_out) => worked_out,
            None => self.what_becomes_of_the_saved_value(stream_id, &retype)?,
        };
        let answered = |value: Option<Value>| GraphTransactionSavedValueRetyped {
            stream: stream_id.to_owned(),
            node: node_uuid.to_string(),
            value_before: retype.saved.clone(),
            value,
            cast_by: None,
            default_because: None,
            conversions_that_fit: Vec::new(),
        };
        let answered = match what_became_of_it {
            SavedValueRetyped::TakenAsItIs => return None,
            SavedValueRetyped::CastBy { conversion, value } => GraphTransactionSavedValueRetyped {
                cast_by: Some(conversion.to_string()),
                ..answered(Some(value))
            },
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
            stream,
            node: node.clone(),
            held_under: retype.held_under.to_owned(),
            answered,
        })
    }
}
