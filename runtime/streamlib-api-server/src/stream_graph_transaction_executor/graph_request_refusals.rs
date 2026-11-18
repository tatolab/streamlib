// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Why a graph action or one of its operations was refused, and what the refused operation
//! names, as the wire carries the refusal.

use serde_json::{Value, json};
use streamlib::sdk::descriptors::PinConversion;
use streamlib_project_repository::{ProjectRepositoryRefusal, StreamGraphActionLinkEnd};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, NodeNotRunningBecauseOnTheWire, ProjectActionRefusalOnTheWire,
    RefusedGraphLinkOnTheWire, SuggestedOperationCall,
};
use uuid::Uuid;

use super::what_a_link_action_names;
use crate::refusal_code_and_place::RefusalCodeAndPlace;

/// The node, link and field one operation of a graph action names, as its caller named them.
/// An operation on one node names it as `node`, which a refusal names over any node of its own;
/// an operation on many nodes names the node it makes of them as
/// `node_unless_the_refusal_names_one`, since a refusal naming one of the many is the more exact.
///
/// It also names the stream and the level the operation edits, by the stream id and the path of
/// placed streams, when its caller knows them, so a refusal can suggest a call into them.
#[derive(Default)]
pub(crate) struct WhatAGraphOperationNames {
    pub(super) node: Option<String>,
    pub(super) node_unless_the_refusal_names_one: Option<String>,
    pub(super) link: Option<RefusedGraphLinkOnTheWire>,
    pub(super) field: Option<String>,
    pub(super) stream: Option<String>,
    pub(super) level: Vec<Uuid>,
}

impl WhatAGraphOperationNames {
    /// What the operation names, made in the stream `stream_id`, at `level`.
    pub(super) fn in_the_stream(self, stream_id: &str, level: &[Uuid]) -> Self {
        Self {
            stream: Some(stream_id.to_owned()),
            level: level.to_vec(),
            ..self
        }
    }

    /// The `transact` that places a node of `conversion` between the two ends of the link this
    /// operation names, in its stream and level, as the caller named them; `None` when it names
    /// no link or no stream.
    pub(super) fn the_transact_placing_the_conversion_on_its_link(
        &self,
        conversion: &PinConversion,
    ) -> Option<SuggestedOperationCall> {
        const CONVERSION_LABEL: &str = "conversion";
        let (stream, link) = (self.stream.as_ref()?, self.link.as_ref()?);
        let at_the_level = |mut operation: Value| {
            if !self.level.is_empty() {
                operation["level"] = json!(self.level);
            }
            operation
        };
        let params = json!({
            "operations": [
                at_the_level(json!({
                    "operation": "add_node", "stream": stream,
                    "type": conversion.node_type.as_str(), "as": CONVERSION_LABEL,
                })),
                at_the_level(json!({
                    "operation": "connect", "stream": stream,
                    "source": { "node": link.source.node, "port": link.source.port },
                    "target": { "node": CONVERSION_LABEL, "port": conversion.input_port_name },
                })),
                at_the_level(json!({
                    "operation": "connect", "stream": stream,
                    "source": { "node": CONVERSION_LABEL, "port": conversion.output_port_name },
                    "target": { "node": link.target.node, "port": link.target.port },
                })),
            ],
        });
        let Value::Object(params) = params else {
            return None;
        };
        streamlib_control_contract::the_suggested_call(
            <streamlib_control_contract::Transact as streamlib_control_contract::ControlOperation>::METHOD_NAME,
            params,
        )
    }
}

/// Why one graph action was refused: its code and the place it names, the runtime's own words,
/// and the JSON pointer, into the action's own params, of the value refused.
#[derive(Debug)]
pub(crate) struct GraphRequestRefusal {
    pub(super) code_and_place: Box<RefusalCodeAndPlace>,
    pub(super) runtime_words: String,
    pub(super) refused_params_pointer: Option<String>,
}

impl GraphRequestRefusal {
    /// A refusal coded and placed by `code_and_place`, in `runtime_words`.
    pub(crate) fn coded(code_and_place: RefusalCodeAndPlace, runtime_words: String) -> Self {
        Self {
            code_and_place: Box::new(code_and_place),
            runtime_words,
            refused_params_pointer: None,
        }
    }

    /// A refusal of a value set on an input, coded by `refusal`; the value is at
    /// `refused_params_pointer` in the action's params, when a caller sent it there.
    pub(super) fn of_an_input_value(
        refusal: streamlib::sdk::graph::input_values::InputValueRefusal,
        refused_params_pointer: Option<String>,
    ) -> Self {
        use streamlib::sdk::graph::input_values::InputValueRefusalCode;
        Self {
            refused_params_pointer,
            ..Self::coded(
                RefusalCodeAndPlace::coded(match refusal.code {
                    InputValueRefusalCode::PortNotFound => {
                        ControlOperationRefusalCode::PortNotFound
                    }
                    InputValueRefusalCode::InputTakesNoValue => {
                        ControlOperationRefusalCode::InputTakesNoValue
                    }
                    InputValueRefusalCode::InputValueNotValid => {
                        ControlOperationRefusalCode::InputValueNotValid
                    }
                }),
                refusal.words,
            )
        }
    }

    /// A refusal of a value set live on a `reset` input while its stream's playback is stopped,
    /// in the runtime's words: coded `node_not_running`, because the playback is stopped.
    pub(super) fn of_a_reset_input_while_stopped(runtime_words: String) -> Self {
        Self::coded(
            RefusalCodeAndPlace {
                not_running_because: Some(NodeNotRunningBecauseOnTheWire::PlaybackStopped),
                ..RefusalCodeAndPlace::coded(ControlOperationRefusalCode::NodeNotRunning)
            },
            runtime_words,
        )
    }

    /// The refusal of a node `node` the action names that its stream does not hold, in the
    /// runtime's words `why_no_node_is_it`.
    pub(super) fn of_a_node_not_found(node: &str, why_no_node_is_it: String) -> Self {
        Self::coded(
            RefusalCodeAndPlace {
                refused_node: Some(node.to_owned()),
                ..RefusalCodeAndPlace::coded(ControlOperationRefusalCode::NodeNotFound)
            },
            why_no_node_is_it,
        )
    }

    /// This refusal's words, for a project repository call that takes a reader answering words;
    /// the refusal itself is kept in `kept`, so the call's refusal reads back typed.
    pub(super) fn words_kept_in(self, kept: &std::cell::RefCell<Option<Self>>) -> String {
        let runtime_words = self.runtime_words.clone();
        *kept.borrow_mut() = Some(self);
        runtime_words
    }

    /// The refusal of a project repository call that answered `why`: the one its reader kept in
    /// `kept`, when it kept one.
    pub(super) fn kept_in_or(kept: &std::cell::RefCell<Option<Self>>, why: String) -> Self {
        kept.take().unwrap_or_else(|| why.into())
    }

    /// The refusal naming `input` as the field refused.
    pub(super) fn of_the_field(mut self, input: &str) -> Self {
        self.code_and_place.refused_field = Some(input.to_owned());
        self
    }
}

impl From<String> for GraphRequestRefusal {
    fn from(runtime_words: String) -> Self {
        Self::coded(
            RefusalCodeAndPlace::coded(ControlOperationRefusalCode::Refused),
            runtime_words,
        )
    }
}

impl From<streamlib_project_repository::NodeNotRead> for GraphRequestRefusal {
    fn from(not_read: streamlib_project_repository::NodeNotRead) -> Self {
        match not_read {
            streamlib_project_repository::NodeNotRead::NoSuchNode(why) => Self::coded(
                RefusalCodeAndPlace::coded(ControlOperationRefusalCode::NodeNotFound),
                why,
            ),
            streamlib_project_repository::NodeNotRead::DoesNotRead(why) => why.into(),
        }
    }
}

impl From<&str> for GraphRequestRefusal {
    fn from(runtime_words: &str) -> Self {
        runtime_words.to_owned().into()
    }
}

impl From<streamlib::sdk::error::Error> for GraphRequestRefusal {
    fn from(refusal: streamlib::sdk::error::Error) -> Self {
        Self::coded(
            RefusalCodeAndPlace::of_an_engine_error(&refusal),
            refusal.to_string(),
        )
    }
}

impl From<ProjectRepositoryRefusal> for GraphRequestRefusal {
    fn from(refusal: ProjectRepositoryRefusal) -> Self {
        Self::coded(
            RefusalCodeAndPlace::of_a_project_repository_refusal(&refusal),
            refusal.to_string(),
        )
    }
}

/// Why a graph action was refused, in the runtime's own words, and which of its operations
/// was refused when one was.
pub(crate) struct GraphTransactionRefusal {
    pub(super) refused_operation_index: Option<usize>,
    pub(super) what_the_refused_operation_names: Box<WhatAGraphOperationNames>,
    pub(super) refusal: GraphRequestRefusal,
}

impl GraphTransactionRefusal {
    pub(super) fn of_the_operation(
        refused_operation_index: usize,
        what_the_refused_operation_names: WhatAGraphOperationNames,
        refusal: impl Into<GraphRequestRefusal>,
    ) -> Self {
        Self {
            refused_operation_index: Some(refused_operation_index),
            what_the_refused_operation_names: Box::new(what_the_refused_operation_names),
            refusal: refusal.into(),
        }
    }

    /// The refusal of a `connect` from `source` to `target`, its one operation, as its caller
    /// named the link, at the own level of the stream `stream_id` when it is known.
    pub(crate) fn of_a_connect(
        stream_id: Option<&str>,
        source: &StreamGraphActionLinkEnd,
        target: &StreamGraphActionLinkEnd,
        refusal: GraphRequestRefusal,
    ) -> Self {
        let what_the_connect_names = what_a_link_action_names(source, target);
        let what_the_connect_names = match stream_id {
            Some(stream_id) => what_the_connect_names.in_the_stream(stream_id, &[]),
            None => what_the_connect_names,
        };
        Self::of_the_operation(0, what_the_connect_names, refusal)
    }

    pub(crate) fn of_the_whole_transaction(refusal: impl Into<GraphRequestRefusal>) -> Self {
        Self {
            refused_operation_index: None,
            what_the_refused_operation_names: Box::default(),
            refusal: refusal.into(),
        }
    }

    /// The refusal of the action the tool `tool_name` was sent, as the wire carries it: the
    /// node as the refused operation's caller named it, else as the refusal names it, else the
    /// node an operation on many nodes makes of them; the input the refusal names, else the one
    /// the operation names; and the link the operation names.
    pub(crate) fn into_the_refusal_on_the_wire(
        self,
        tool_name: &str,
    ) -> ProjectActionRefusalOnTheWire {
        let what_the_refused_operation_names = *self.what_the_refused_operation_names;
        let GraphRequestRefusal {
            code_and_place,
            runtime_words,
            refused_params_pointer,
        } = self.refusal;
        let mut code_and_place = *code_and_place;
        let suggested_call = code_and_place
            .conversion_to_place
            .take()
            .and_then(|conversion| {
                what_the_refused_operation_names
                    .the_transact_placing_the_conversion_on_its_link(&conversion)
            });
        let refused_params_pointer = match (refused_params_pointer, self.refused_operation_index) {
            (Some(pointer), Some(index)) if tool_name == <streamlib_control_contract::Transact as streamlib_control_contract::ControlOperation>::METHOD_NAME => {
                Some(format!("/operations/{index}{pointer}"))
            }
            (pointer, _) => pointer,
        };
        let the_links_target_as_its_caller_named_it = what_the_refused_operation_names
            .link
            .as_ref()
            .filter(|_| {
                code_and_place.refusal_code == ControlOperationRefusalCode::InputTakesNoWire
            })
            .map(|link| link.target.node.clone());
        code_and_place.refused_node = what_the_refused_operation_names
            .node
            .or(the_links_target_as_its_caller_named_it)
            .or(code_and_place.refused_node)
            .or(what_the_refused_operation_names.node_unless_the_refusal_names_one);
        code_and_place.refused_field = code_and_place
            .refused_field
            .or(what_the_refused_operation_names.field);
        ProjectActionRefusalOnTheWire {
            refused_operation_index: self.refused_operation_index,
            refused_link: what_the_refused_operation_names.link,
            refused_params_pointer,
            suggested_call,
            ..code_and_place.on_the_refusal_of(tool_name, runtime_words)
        }
    }
}

impl From<ProjectRepositoryRefusal> for GraphTransactionRefusal {
    fn from(refusal: ProjectRepositoryRefusal) -> Self {
        Self::of_the_whole_transaction(refusal)
    }
}
