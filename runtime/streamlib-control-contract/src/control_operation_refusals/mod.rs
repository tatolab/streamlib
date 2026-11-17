// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! What the contract itself knows about a refused call: where params that do not match their
//! schema went wrong, the refusal of a call that names no operation or sends such params, and the
//! one constructor of a suggested call.

use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use streamlib_runtime_client_contract::local_api_wire_contract::{
    ControlOperationRefusalCode, ProjectActionRefusalOnTheWire, SuggestedOperationCall,
};

use crate::agent_surface::{
    DESCRIBE_OPERATIONS_TOOL_NAME, DescribeOperationsRequest, LIST_OPERATION_GROUPS_TOOL_NAME,
    ListOperationGroupsRequest, the_operation_served_to_agents_as,
};
use crate::control_operations::{ControlOperationCall, ControlOperationCallParseRefusal};

/// Params that do not read as the type they were sent for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamsThatDoNotMatchTheirSchema {
    /// The JSON pointer of the value the read stopped at; absent when it stopped at the params
    /// as a whole.
    pub pointer: Option<String>,
    /// What the read said.
    pub why: String,
}

/// `params` read as `Params`, or where and why they do not match its schema.
pub fn the_params_read_as<Params: DeserializeOwned>(
    params: Value,
) -> Result<Params, ParamsThatDoNotMatchTheirSchema> {
    serde_path_to_error::deserialize(params).map_err(|refusal| ParamsThatDoNotMatchTheirSchema {
        pointer: the_json_pointer_of(refusal.path()),
        why: refusal.inner().to_string(),
    })
}

/// `path` as a JSON pointer, up to the first step the read could not name.
fn the_json_pointer_of(path: &serde_path_to_error::Path) -> Option<String> {
    let mut pointer = String::new();
    for segment in path.iter() {
        let token = match segment {
            serde_path_to_error::Segment::Seq { index } => index.to_string(),
            serde_path_to_error::Segment::Map { key } => key.clone(),
            serde_path_to_error::Segment::Enum { variant } => variant.clone(),
            serde_path_to_error::Segment::Unknown => break,
        };
        pointer.push('/');
        pointer.push_str(&the_json_pointer_token_of(&token));
    }
    (!pointer.is_empty()).then_some(pointer)
}

/// `key` as one token of a JSON pointer: `~` written `~0` and `/` written `~1`.
pub fn the_json_pointer_token_of(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// The call `operation` with `params`, when an agent can make it: a discovery tool whose
/// arguments `params` are, or an operation served to agents whose request `params` reads as.
pub fn the_suggested_call(
    operation: &str,
    params: Map<String, Value>,
) -> Option<SuggestedOperationCall> {
    let params_value = Value::Object(params.clone());
    let can_be_called = match operation {
        LIST_OPERATION_GROUPS_TOOL_NAME => {
            the_params_read_as::<ListOperationGroupsRequest>(params_value).is_ok()
        }
        DESCRIBE_OPERATIONS_TOOL_NAME => {
            the_params_read_as::<DescribeOperationsRequest>(params_value).is_ok()
        }
        operation => {
            the_operation_served_to_agents_as(operation).is_some()
                && ControlOperationCall::parse(operation, Some(params_value)).is_ok()
        }
    };
    can_be_called.then(|| SuggestedOperationCall {
        operation: operation.to_owned(),
        params,
    })
}

/// The call that lists every operation, which a call naming none suggests.
pub fn the_call_that_lists_every_operation() -> Option<SuggestedOperationCall> {
    the_suggested_call(LIST_OPERATION_GROUPS_TOOL_NAME, Map::new())
}

/// The call that describes `operation`'s params, which params that do not match suggest.
pub fn the_call_that_describes(operation: &str) -> Option<SuggestedOperationCall> {
    the_suggested_call(
        DESCRIBE_OPERATIONS_TOOL_NAME,
        Map::from_iter([(
            "operations".to_owned(),
            Value::Array(vec![Value::String(operation.to_owned())]),
        )]),
    )
}

/// The refusal of a call naming `operation`, which the runtime serves under no name, sent
/// through the discovery tool `through_tool` or, with none, straight to `operation`. Its
/// `action` is the tool the call came through, else the operation it named, and it suggests the
/// call that lists every operation.
pub fn the_refusal_of_an_unknown_operation(
    through_tool: Option<&str>,
    operation: &str,
) -> ProjectActionRefusalOnTheWire {
    let no_such_operation = format!("the runtime serves no operation named `{operation}`");
    let (action, runtime_words) = match through_tool {
        Some(tool) => (tool, format!("{tool} failed: {no_such_operation}")),
        None => (operation, no_such_operation),
    };
    ProjectActionRefusalOnTheWire {
        suggested_call: the_call_that_lists_every_operation(),
        ..ProjectActionRefusalOnTheWire::of_the_action(
            action,
            ControlOperationRefusalCode::OperationNotFound,
            runtime_words,
        )
    }
}

impl ControlOperationCallParseRefusal {
    /// This refusal as every door carries it.
    pub fn the_refusal_on_the_wire(&self) -> ProjectActionRefusalOnTheWire {
        match self {
            Self::NoSuchMethod { method_name } => {
                the_refusal_of_an_unknown_operation(None, method_name)
            }
            Self::InvalidParams {
                method_name,
                params_pointer,
                ..
            } => ProjectActionRefusalOnTheWire {
                refused_params_pointer: params_pointer.clone(),
                suggested_call: the_call_that_describes(method_name),
                ..ProjectActionRefusalOnTheWire::of_the_action(
                    *method_name,
                    ControlOperationRefusalCode::ParamsDoNotMatchSchema,
                    self.to_string(),
                )
            },
        }
    }
}

#[cfg(test)]
mod tests;
