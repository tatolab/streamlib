// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A saved value cast by a module's function node conversion out of stream: one start of the
//! stream's node interpreter runs the conversion's body once, as a reader runs it, with the
//! saved value on its one wired input, and prints the value its output gives.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use super::node_interpreter_describe::{
    NodeInterpreterRunOutOfStream, node_interpreter_describe_command_with_arguments,
    run_a_describe_reading_its_document,
};
use super::node_interpreter_spawn_host::LoadedStreamAHelperProcessBelongsTo;
use crate::core::descriptors::{NodeClassImportPath, PinConversionSavedValueCast};
use crate::core::error::Result;
use crate::core::runtime::StreamEnvironment;

/// The bootstrap argument that asks for one saved value cast by a function node conversion.
const CAST_A_SAVED_VALUE_ARGUMENT: &str = "--cast-a-saved-value";

/// How long one cast may run before its process group is killed and the cast refused: an
/// edit waits on it.
pub(crate) const SAVED_VALUE_CAST_BOUND: Duration = Duration::from_secs(20);

/// A cast, in the words its refusals name it by.
const A_SAVED_VALUE_CAST: NodeInterpreterRunOutOfStream = NodeInterpreterRunOutOfStream {
    doing: "casting the saved value",
    run: "cast",
    document: "cast document",
};

/// What a cast prints on its standard output: the value cast, or why it was not.
#[derive(serde::Deserialize)]
struct SavedValueCastDocument {
    #[serde(default)]
    cast: Option<Value>,
    #[serde(default)]
    not_cast_because: Option<String>,
}

/// Where a function node conversion's body runs out of stream: the stream's interpreter, the
/// runtime it lends, and the stream its run belongs to.
#[derive(Clone)]
pub(crate) struct WhereAConversionRunsOutOfStream {
    pub(crate) stream_environment: StreamEnvironment,
    pub(crate) lend_directory: Arc<PathBuf>,
    pub(crate) the_stream_it_belongs_to: LoadedStreamAHelperProcessBelongsTo,
}

/// The command that casts `saved_value` through `conversion`'s body, its input `input_port`
/// holding it, in `stream_environment`.
pub(crate) fn saved_value_cast_command(
    conversion: &NodeClassImportPath,
    input_port: &str,
    saved_value: &Value,
    stream_environment: &StreamEnvironment,
    lend_directory: &Path,
) -> Result<Command> {
    let request = serde_json::json!({ "input": input_port, "value": saved_value }).to_string();
    node_interpreter_describe_command_with_arguments(
        [CAST_A_SAVED_VALUE_ARGUMENT, conversion.as_str(), &request],
        stream_environment,
        lend_directory,
    )
}

/// The cast `conversion`, a function node conversion whose wired input is `input_port`, makes
/// of a saved value: its body run once in the stream's node interpreter `where_it_runs` names;
/// refused with why when the body cannot run out of stream, refuses the value, or the run
/// fails.
pub(crate) fn the_saved_value_cast_of_a_function_node_conversion(
    conversion: NodeClassImportPath,
    input_port: String,
    where_it_runs: WhereAConversionRunsOutOfStream,
) -> PinConversionSavedValueCast {
    PinConversionSavedValueCast::new(move |saved_value, _into| {
        cast_a_saved_value_in_the_streams_node_interpreter(
            &conversion,
            &input_port,
            saved_value,
            &where_it_runs,
        )
    })
}

fn cast_a_saved_value_in_the_streams_node_interpreter(
    conversion: &NodeClassImportPath,
    input_port: &str,
    saved_value: &Value,
    where_it_runs: &WhereAConversionRunsOutOfStream,
) -> std::result::Result<Value, String> {
    let WhereAConversionRunsOutOfStream {
        stream_environment,
        lend_directory,
        the_stream_it_belongs_to,
    } = where_it_runs;
    let (document, quoted_standard_error): (SavedValueCastDocument, String) =
        run_a_describe_reading_its_document(
            || {
                saved_value_cast_command(
                    conversion,
                    input_port,
                    saved_value,
                    stream_environment,
                    lend_directory,
                )
            },
            stream_environment,
            SAVED_VALUE_CAST_BOUND,
            the_stream_it_belongs_to,
            &|| false,
            &crate::core::error::Error::Runtime,
            &A_SAVED_VALUE_CAST,
        )
        .map_err(|refused| format!("its body did not run out of stream: {refused}"))?;
    match document {
        SavedValueCastDocument {
            cast: Some(cast), ..
        } => Ok(cast),
        SavedValueCastDocument {
            not_cast_because: Some(why),
            ..
        } => Err(why),
        SavedValueCastDocument { .. } => Err(format!(
            "the stream's interpreter `{}` printed a cast document naming neither the value nor \
             why. {quoted_standard_error}",
            stream_environment.interpreter.display()
        )),
    }
}

#[cfg(test)]
mod tests;
