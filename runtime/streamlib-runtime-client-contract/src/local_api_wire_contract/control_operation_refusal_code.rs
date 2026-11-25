// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The code of each kind of refusal a control operation answers.

use serde::{Deserialize, Serialize};

/// What kind of refusal a refused call is. The set grows: a code a client does not know reads
/// as `refused`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "json-schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "json-schema", schemars(extend("x-may-grow" = true)))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ControlOperationRefusalCode {
    /// The params do not match the operation's params schema; `refused_params_pointer` names the
    /// value, when one is.
    ParamsDoNotMatchSchema,
    /// The runtime serves no operation under the name sent.
    OperationNotFound,
    /// The node the call reached for does not run; `not_running_because` says why.
    NodeNotRunning,
    /// The runtime could not carry the request out, through no doing of the caller.
    RuntimeFailed,
    /// The node has no input or setting of the name sent; the runtime's words list the ones it
    /// has.
    PortNotFound,
    /// The input takes only a link, so it holds no value.
    InputTakesNoValue,
    /// The value sent is not one the input takes: of another type, or outside its range. The
    /// runtime's words say what it takes.
    InputValueNotValid,
    /// No stream the runtime holds, or no stream asset the project holds, is the one named.
    StreamNotFound,
    /// The stream holds no node of the UUID or `as` label sent; `refused_node` names it, when the
    /// call named one.
    NodeNotFound,
    /// No node type of the name sent is one the stream can add.
    NodeTypeNotFound,
    /// Another node of the graph already has the name sent.
    NodeNameTaken,
    /// The link sent is already in the graph.
    LinkAlreadyExists,
    /// The two ends of the link sent carry pin types that do not connect, or connect only
    /// through a convert node `connect` does not place; the runtime's words name both ends, both
    /// types and each node type that converts between them.
    LinkPinTypesDoNotConnect,
    /// The two ends of the link sent connect only through a conversion a module or the project's
    /// own node declares, which `connect` never places; the runtime's words name each such node
    /// type. With exactly one, `suggested_call` is the `transact` that places it between them,
    /// only when both ends are UUID-named nodes the edited level holds.
    LinkNeedsAModuleConversion,
    /// The link sent ends at an input that takes no link: a setting read each run, a setting of
    /// a type no message carries, or a setting of a `GraphInput`, `GraphOutput`, `SetVariable`
    /// or `GetVariable`; `refused_input` names it.
    InputTakesNoWire,
    /// The link sent carries a Stream or a Trigger into a setting read at setup, which takes a
    /// Value wire only, since each change of what it carries rebuilds the setting's node;
    /// `refused_input` names the setting, and the runtime's words the node to place between them.
    LinkStreamsIntoASetting,
    /// The project holds no file or folder at the path sent; `refused_file` names it.
    FileNotFound,
    /// The file changed, or was taken out, since it was read, so the write was not made;
    /// `refused_file` names it. Read it again to see what it holds now.
    FileChangedSinceItWasRead,
    /// A check of the project's code was still running at its bound and was killed; the
    /// runtime's words give the bound, and `refused_file` the file it was checking.
    CheckRanOutOfTime,
    /// A check of the project's views and controls found no extension SDK kit to check them
    /// against; the runtime's words name each place it looked.
    ExtensionSdkNotFound,
    /// The request was understood and not done; changing it can help. The runtime's words say
    /// what to change.
    // `#[serde(other)]` holds only on the last variant, so a new code goes above this one.
    #[default]
    #[serde(other)]
    Refused,
}
