// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A `set_node_input_value` that chooses another `pin_type` for a top-level `GraphInput` or a
//! `Constant` saving a value carries that value into the new type in the same transaction, as
//! three value sets it records: the saved value taken off, the type set, and the value the one
//! conversion fitting the two types casts, held within the Input's `range`, set again. With no such cast the value
//! gives way to the new type's default, which is no saved value, and the answer says why. One
//! undo puts back both the type and the value.
//!
//! A module's conversion casts by running its body in the stream's interpreter, so each cast a
//! transaction asks for is worked out before the transaction takes the project's lock or opens
//! a stream's edit batch, from the documents as they stand then; the transaction takes the cast
//! worked out for the value and types it finds. One they no longer match, as when an earlier
//! change in the same transaction touched the Input, runs no module's conversion: it is worked
//! out by the casts that start no process, and a module's says why it did not run.

use streamlib::sdk::descriptors::{
    PinConversion, PinConversionSavedValueCast, PinNumberRange, PinType, SavedValueRetyped,
    the_saved_value_retyped,
};
use streamlib::sdk::graph::graph_interface_resolution::{
    GRAPH_INPUT_VALUE_FORMER_NAME, GRAPH_INTERFACE_PIN_TYPE_SETTING,
    the_range_a_graph_input_config_declares,
};
use streamlib::sdk::nodes::the_pin_type_a_constant_holds_by_default;
use streamlib_project_repository::{
    CONSTANT_NODE_TYPE, GRAPH_INPUT_NODE_TYPE, GRAPH_INTERFACE_VALUE_PORT_NAME, StreamGraphDocument,
    StreamGraphDocumentNode,
};

use serde::Deserialize;

use super::*;

/// The saved value a retype of a `GraphInput` carries: the stream asset and node the request
/// names, the input the value is saved under, and what the answer names of it, among it the
/// value it saves next.
struct SavedValueARetypeCarries {
    stream: Uuid,
    node: String,
    held_under: &'static str,
    answered: GraphTransactionSavedValueRetyped,
}

/// What a retype of a `GraphInput` carries: the value it saves, the type it is of, the type
/// chosen and the Input's range. A retype worked out before the edit is taken for one that
/// carries the same.
#[derive(Clone, PartialEq)]
struct WhatARetypeCarries {
    saved: Value,
    from: PinType,
    into: PinType,
    range: Option<PinNumberRange>,
}

/// A retype of a `GraphInput` saving a value: the input the value is saved under, and what it
/// carries.
struct RetypeOfASavedValue {
    held_under: &'static str,
    carried: WhatARetypeCarries,
}

/// One retype worked out before the edit: what it carried then, and what became of its value.
struct ASavedValueRetypeWorkedOut {
    carried: WhatARetypeCarries,
    retyped: SavedValueRetyped,
}

/// What became of each saved value a transaction retypes, worked out before it took any lock,
/// by the stream asset and node: the first retype of each node the transaction names, found in
/// the documents as they stood then.
#[derive(Default)]
pub(super) struct SavedValueRetypesWorkedOutBeforeTheEdit {
    by_stream_and_node: HashMap<(Uuid, Uuid), ASavedValueRetypeWorkedOut>,
}

impl SavedValueRetypesWorkedOutBeforeTheEdit {
    /// What became of the retype of `node` in the stream asset `stream` carrying `carried`, if
    /// one carrying the same was worked out.
    fn of(
        &self,
        stream: Uuid,
        node: Uuid,
        carried: &WhatARetypeCarries,
    ) -> Option<SavedValueRetyped> {
        self.by_stream_and_node
            .get(&(stream, node))
            .filter(|worked_out| worked_out.carried == *carried)
            .map(|worked_out| worked_out.retyped.clone())
    }
}

/// Why a retype the transaction found carrying other than what was worked out before the edit
/// runs no module's conversion: the edit holds the project's lock.
const AN_EARLIER_CHANGE_IN_THE_TRANSACTION_TOUCHED_THE_INPUT: &str = "an earlier change in the same transaction touched this Input, so its cast was not worked \
     out before the transaction took the project's lock, and a module's conversion does not run \
     under it";

/// The stream asset, node and chosen type of `request` when it sets `pin_type` on a node of a
/// stream's own level; `None` for any other request.
fn what_a_retype_names(request: &StreamGraphAction) -> Option<(Uuid, &str, &Value)> {
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
    (input == GRAPH_INTERFACE_PIN_TYPE_SETTING && level.is_empty()).then_some((
        *stream,
        node.as_str(),
        chosen,
    ))
}

/// The retype of the value `retyped` saves into `chosen`: `None` unless `retyped` is a
/// `GraphInput` or a `Constant` saving a value of another type than `chosen`. A `Constant`
/// choosing no type holds a Float, and has no range.
fn the_retype_of_what_a_node_saves(
    retyped: &StreamGraphDocumentNode,
    chosen: &Value,
) -> Option<RetypeOfASavedValue> {
    let a_constant = retyped.node_type == CONSTANT_NODE_TYPE;
    if retyped.node_type != GRAPH_INPUT_NODE_TYPE && !a_constant {
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
    let pin_type_of = |value: &Value| PinType::deserialize(value).ok();
    let from = match retyped.input_values.get(GRAPH_INTERFACE_PIN_TYPE_SETTING) {
        Some(held) => pin_type_of(held)?,
        None if a_constant => the_pin_type_a_constant_holds_by_default(),
        None => return None,
    };
    let into = pin_type_of(chosen)?;
    if from == into {
        return None;
    }
    let range = (!a_constant)
        .then(|| {
            the_range_a_graph_input_config_declares(Some(&Value::Object(
                retyped.input_values.clone(),
            )))
        })
        .flatten();
    Some(RetypeOfASavedValue {
        held_under,
        carried: WhatARetypeCarries {
            saved,
            from,
            into,
            range,
        },
    })
}

/// `conversions` with each cast that starts a process, a module's, in place of one that does
/// not run, saying why: an earlier change in the same transaction touched the Input.
fn with_no_cast_that_starts_a_process(conversions: Vec<PinConversion>) -> Vec<PinConversion> {
    conversions
        .into_iter()
        .map(|mut conversion| {
            if conversion
                .saved_value_cast
                .as_ref()
                .is_some_and(PinConversionSavedValueCast::starts_a_process)
            {
                conversion.saved_value_cast = Some(PinConversionSavedValueCast::that_does_not_run(
                    AN_EARLIER_CHANGE_IN_THE_TRANSACTION_TOUCHED_THE_INPUT,
                ));
            }
            conversion
        })
        .collect()
}

/// What becomes of the value `carried` names by `conversions`.
fn what_becomes_of_the_saved_value(
    carried: &WhatARetypeCarries,
    conversions: &[PinConversion],
) -> SavedValueRetyped {
    the_saved_value_retyped(
        &carried.saved,
        &carried.from,
        &carried.into,
        carried.range.as_ref(),
        conversions,
    )
}

impl StreamGraphTransactionExecutor<'_> {
    /// What becomes of each saved value `requests_and_the_stream_id_each_names` retype, worked
    /// out against the documents as they stand: read under `project_repository`'s lock, taken
    /// only while they are read, or from each stream's live graph when there is none. A
    /// retype whose node or document cannot be read now is left for the transaction.
    pub(super) fn the_saved_value_retypes_worked_out_before_the_edit(
        &self,
        requests_and_the_stream_id_each_names: &[(StreamGraphAction, String)],
        stream_locations: &HashMap<String, StreamLocation>,
        project_repository: Option<
            &streamlib_project_repository::ProjectRepositoryOneWriterAtATime,
        >,
    ) -> SavedValueRetypesWorkedOutBeforeTheEdit {
        let retypes: Vec<(&str, Uuid, &str, &Value)> = requests_and_the_stream_id_each_names
            .iter()
            .filter_map(|(request, stream_id)| {
                let (stream, node, chosen) = what_a_retype_names(request)?;
                Some((stream_id.as_str(), stream, node, chosen))
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
                    let stream_operations = self
                        .operations_on_the_loaded_streams
                        .runtime_operations_of_the_stream_a_call_names(stream_id)
                        .ok()?;
                    self.the_stream_graph_document_as_it_stands(
                        project_repository_held.as_deref(),
                        stream_locations.get(*stream_id)?.stream_asset,
                        stream_operations.as_ref(),
                    )
                    .ok()
                });
            }
        }
        let mut worked_out = SavedValueRetypesWorkedOutBeforeTheEdit::default();
        for (stream_id, stream, node, chosen) in retypes {
            let Ok(node) = Uuid::parse_str(node) else {
                continue;
            };
            // Only a node's first retype is kept, so a later one runs no cast.
            if worked_out.by_stream_and_node.contains_key(&(stream, node)) {
                continue;
            }
            let Some(retype) = documents
                .get(stream_id)
                .and_then(Option::as_ref)
                .and_then(|document| document.node(node).ok())
                .and_then(|retyped| the_retype_of_what_a_node_saves(&retyped, chosen))
            else {
                continue;
            };
            let Ok(conversions) = self
                .operations_on_the_loaded_streams
                .pin_conversions_of_the_stream_a_call_names(stream_id)
            else {
                continue;
            };
            let retyped = what_becomes_of_the_saved_value(&retype.carried, &conversions);
            worked_out.by_stream_and_node.insert(
                (stream, node),
                ASavedValueRetypeWorkedOut {
                    carried: retype.carried,
                    retyped,
                },
            );
        }
        worked_out
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
        let mut sets: Vec<(&str, StreamGraphAction)> = vec![
            (carried.held_under, set(carried.held_under, None)),
            (GRAPH_INTERFACE_PIN_TYPE_SETTING, request),
        ];
        if let Some(value) = &carried.answered.value {
            sets.push((
                GRAPH_INTERFACE_VALUE_PORT_NAME,
                set(GRAPH_INTERFACE_VALUE_PORT_NAME, Some(value.clone())),
            ));
        }
        let mut applied_by_the_retype: Vec<OneGraphRequestApplied> = Vec::with_capacity(sets.len());
        for (input_set, one_set) in sets {
            match self.apply_one_request(one_set, stream_id, staged, InputValueCheck::MadeOnApply) {
                Ok(applied) => applied_by_the_retype.push(applied),
                Err(refusal) => {
                    tracing::warn!(
                        stream = %stream_id,
                        node = %carried.node,
                        input = %input_set,
                        refusal = %refusal.runtime_words(),
                        "a value set carrying a retyped Graph Input's saved value was refused, \
                         and the retype is reversed"
                    );
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
    /// the value is the one worked out before the edit for the retype found here; when none
    /// was, it is worked out now by every conversion that casts without starting a process.
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
        let what_became_of_it = match staged.saved_value_retypes_worked_out_before_the_edit.of(
            stream,
            node_uuid,
            &retype.carried,
        ) {
            Some(worked_out) => worked_out,
            None => what_becomes_of_the_saved_value(
                &retype.carried,
                &with_no_cast_that_starts_a_process(
                    self.operations_on_the_loaded_streams
                        .pin_conversions_of_the_stream_a_call_names(stream_id)
                        .ok()?
                        .to_vec(),
                ),
            ),
        };
        let answered = |value: Option<Value>| GraphTransactionSavedValueRetyped {
            stream: stream_id.to_owned(),
            node: node_uuid.to_string(),
            value_before: retype.carried.saved.clone(),
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
            node: node.to_owned(),
            held_under: retype.held_under,
            answered,
        })
    }
}
