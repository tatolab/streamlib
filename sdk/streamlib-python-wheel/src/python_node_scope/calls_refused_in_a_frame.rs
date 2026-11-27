// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A Python node's calls too slow to run every frame, refused while its frame runs on the calling
//! thread, each with its own exception and its fix written in Python.

use pyo3::create_exception;
use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib_scope::{CallRefusedInAFrame, CallTooSlowForAFrame};

create_exception!(
    tatolab.runtime._engine,
    KernelCreatedInAFrame,
    PyRuntimeError,
    "A kernel was created in the per-frame function; create it in a computed value instead."
);
create_exception!(
    tatolab.runtime._engine,
    DeviceOpenedInAFrame,
    PyRuntimeError,
    "A device was opened in the per-frame function; open it in a computed value instead."
);
create_exception!(
    tatolab.runtime._engine,
    SessionStartedInAFrame,
    PyRuntimeError,
    "A session was started in the per-frame function; start it in a computed value instead."
);

/// The refusal with its fix written in Python.
fn python_words_of(refused: &CallRefusedInAFrame) -> String {
    refused.words_with_the_fix(&format!(
        "{} = computed(lambda: … {}(…))",
        refused.does.what_it_makes(),
        refused.call
    ))
}

/// `call`, which does what `does` names, refused with the exception of its kind while a frame
/// runs on this thread.
pub(crate) fn raised_while_a_frame_runs(does: CallTooSlowForAFrame, call: &str) -> PyResult<()> {
    does.refused_while_a_frame_runs(call).map_err(|refused| {
        let words = python_words_of(&refused);
        match refused.does {
            CallTooSlowForAFrame::CreatesAKernel => KernelCreatedInAFrame::new_err(words),
            CallTooSlowForAFrame::OpensADevice => DeviceOpenedInAFrame::new_err(words),
            CallTooSlowForAFrame::StartsASession => SessionStartedInAFrame::new_err(words),
        }
    })
}

/// `call`, which does what `does` names, refused in the words a module host table verb answers
/// while a frame runs on this thread: led by the refusal's code, since the words are all a module
/// hands on.
pub(crate) fn worded_while_a_frame_runs(
    does: CallTooSlowForAFrame,
    call: &str,
) -> Result<(), String> {
    does.refused_while_a_frame_runs(call)
        .map_err(|refused| format!("{}: {}", does.code(), python_words_of(&refused)))
}

pub(super) fn add_the_calls_refused_in_a_frame_to(module: &Bound<'_, PyModule>) -> PyResult<()> {
    let python = module.py();
    module.add(
        "KernelCreatedInAFrame",
        python.get_type::<KernelCreatedInAFrame>(),
    )?;
    module.add(
        "DeviceOpenedInAFrame",
        python.get_type::<DeviceOpenedInAFrame>(),
    )?;
    module.add(
        "SessionStartedInAFrame",
        python.get_type::<SessionStartedInAFrame>(),
    )?;
    Ok(())
}
