// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A saved value cast by a module's function node conversion out of stream: one start of the
//! stream's node interpreter runs the conversion's body once, as a reader runs it, with the
//! saved value on its one wired input, and prints the value its output gives.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::Value;

use super::node_interpreter_describe::{
    NodeInterpreterRunOutOfStream, node_interpreter_describe_command_with_arguments,
    run_the_node_interpreter_out_of_stream_reading_its_document,
};
use super::node_interpreter_spawn_host::LoadedStreamAHelperProcessBelongsTo;
use crate::core::descriptors::{
    NodeClassImportPath, PinConversionSavedValueCast, SavedValueNotCast,
};
use crate::core::error::Result;
use crate::core::runtime::StreamEnvironment;

/// The bootstrap argument that asks for one saved value cast by a function node conversion.
const CAST_A_SAVED_VALUE_ARGUMENT: &str = "--cast-a-saved-value";

/// How long one cast may run before its process group is killed and the cast refused: the
/// edit retyping the Graph Input waits on it, so it is far shorter than a describe's bound.
pub(crate) const SAVED_VALUE_CAST_BOUND: Duration = Duration::from_secs(5);

/// A cast, in the words its refusals name it by.
const A_SAVED_VALUE_CAST: NodeInterpreterRunOutOfStream = NodeInterpreterRunOutOfStream {
    activity: "casting the saved value",
    run_name: "cast",
    document_name: "cast document",
};

/// What a cast is asked: the conversion's wired input and the saved value it holds. The
/// bootstrap reads these keys as `SAVED_VALUE_CAST_REQUEST_*_KEY` names them.
#[derive(serde::Serialize)]
struct SavedValueCastRequest<'a> {
    input: &'a str,
    value: &'a Value,
}

/// What a cast prints on its standard output: the value cast, why the body gave none of the
/// value, or why the body did not run. The bootstrap writes these keys as
/// `SAVED_VALUE_CAST_DOCUMENT_*_KEY` names them.
#[derive(serde::Deserialize)]
struct SavedValueCastDocument {
    #[serde(default)]
    cast: Option<Value>,
    #[serde(default)]
    not_cast_because: Option<String>,
    #[serde(default)]
    not_run_because: Option<String>,
}

/// Where a function node conversion's body runs out of stream: the stream's interpreter, the
/// runtime it lends, the stream its run belongs to, and the host's interrupt, which kills a
/// cast as it kills a describe.
#[derive(Clone)]
pub(crate) struct WhereAConversionRunsOutOfStream {
    pub(crate) stream_environment: StreamEnvironment,
    pub(crate) lend_directory: Arc<PathBuf>,
    pub(crate) the_stream_it_belongs_to: LoadedStreamAHelperProcessBelongsTo,
    pub(crate) interrupted_by_the_host: Arc<AtomicBool>,
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
    let request = serde_json::to_string(&SavedValueCastRequest {
        input: input_port,
        value: saved_value,
    })
    .map_err(|not_written| {
        crate::core::error::Error::Runtime(format!(
            "the request to cast a saved value through `{conversion}` was not written: \
             {not_written}"
        ))
    })?;
    node_interpreter_describe_command_with_arguments(
        [CAST_A_SAVED_VALUE_ARGUMENT, conversion.as_str(), &request],
        stream_environment,
        lend_directory,
    )
}

/// The cast `conversion`, a Python function node conversion whose wired input is
/// `input_port`, makes of a saved value: its body run once in the stream's node interpreter
/// `where_it_runs` names. Refused as not run when the body cannot run out of stream or the
/// run fails, and as refusing the value when the body ran and gave no cast of it.
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
) -> std::result::Result<Value, SavedValueNotCast> {
    let WhereAConversionRunsOutOfStream {
        stream_environment,
        lend_directory,
        the_stream_it_belongs_to,
        interrupted_by_the_host,
    } = where_it_runs;
    let (document, quoted_standard_error): (SavedValueCastDocument, String) =
        run_the_node_interpreter_out_of_stream_reading_its_document(
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
            &|| interrupted_by_the_host.load(Ordering::SeqCst),
            &crate::core::error::Error::Runtime,
            &A_SAVED_VALUE_CAST,
        )
        .map_err(|refused| {
            SavedValueNotCast::DidNotRun(format!("its body did not run out of stream: {refused}"))
        })?;
    match document {
        SavedValueCastDocument {
            cast: Some(cast), ..
        } => Ok(cast),
        SavedValueCastDocument {
            not_cast_because: Some(why),
            ..
        } => Err(SavedValueNotCast::RefusedTheValue(why)),
        SavedValueCastDocument {
            not_run_because: Some(why),
            ..
        } => Err(SavedValueNotCast::DidNotRun(why)),
        SavedValueCastDocument { .. } => Err(SavedValueNotCast::DidNotRun(format!(
            "the stream's interpreter `{}` printed a cast document naming neither the value nor \
             why. {quoted_standard_error}",
            stream_environment.interpreter.display()
        ))),
    }
}

#[cfg(test)]
mod tests;
