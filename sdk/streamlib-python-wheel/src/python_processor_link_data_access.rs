// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The data plane a Python processor reads and writes through.
//!
//! This is where the GIL-release contract is kept for the per-bag path: the
//! conversion between Python objects and msgpack needs the GIL and holds it;
//! the iceoryx2 call that can block does not, and runs detached. Holding the
//! GIL across that call would stall every other Python processor in the
//! process for its duration.

use std::sync::{Arc, OnceLock};

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use streamlib::sdk::error::Error;
use streamlib::sdk::iceoryx2::{InputMailboxesInner, OutputWriterInner};

use crate::python_bag_conversion::{decode_msgpack_to_python_object, encode_bag_to_msgpack};
use crate::python_logging::monotonic_clock_now_ns;

/// One processor's links, as seen from Python.
///
/// Frozen because the engine hands the same object to the processor's own
/// thread and reads it from the wiring path; the interior `OnceLock`s are
/// written once by the host before the processor's first callback.
#[pyclass(name = "ProcessorLinkDataAccess", module = "streamlib", frozen)]
pub(crate) struct PythonProcessorLinkDataAccess {
    input_mailboxes: OnceLock<Arc<InputMailboxesInner>>,
    output_writer: OnceLock<Arc<OutputWriterInner>>,
}

impl PythonProcessorLinkDataAccess {
    pub(crate) fn new() -> Self {
        Self {
            input_mailboxes: OnceLock::new(),
            output_writer: OnceLock::new(),
        }
    }

    pub(crate) fn install_input_mailboxes(&self, input_mailboxes: Arc<InputMailboxesInner>) {
        let _ = self.input_mailboxes.set(input_mailboxes);
    }

    pub(crate) fn install_output_writer(&self, output_writer: Arc<OutputWriterInner>) {
        let _ = self.output_writer.set(output_writer);
    }

    /// The wiring path's reach into this processor's outputs — how the compiler
    /// attaches a link's publisher after the processor exists.
    pub(crate) fn output_writer_inner(&self) -> Option<Arc<OutputWriterInner>> {
        self.output_writer.get().cloned()
    }

    /// The wiring path's reach into this processor's inputs.
    pub(crate) fn input_mailboxes_inner(&self) -> Option<Arc<InputMailboxesInner>> {
        self.input_mailboxes.get().cloned()
    }
}

#[pymethods]
impl PythonProcessorLinkDataAccess {
    /// The next bag on `port_name`, or `None` when the mailbox is empty.
    pub(crate) fn read_from_input_port<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let Some(input_mailboxes) = self.input_mailboxes.get() else {
            return Err(unwired_port_error("input", port_name));
        };
        let read = python
            .detach(|| input_mailboxes.read_raw(port_name))
            .map_err(|read_failure| PyRuntimeError::new_err(read_failure.to_string()))?;
        match read {
            Some((encoded, _timestamp_ns)) => {
                decode_msgpack_to_python_object(python, &encoded).map(Some)
            }
            None => Ok(None),
        }
    }

    /// The next bag on `port_name` with its stamp, or `(None, None)` when the
    /// mailbox is empty.
    pub(crate) fn read_from_input_port_with_timestamp<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
    ) -> PyResult<(Option<Bound<'py, PyAny>>, Option<i64>)> {
        let Some(input_mailboxes) = self.input_mailboxes.get() else {
            return Err(unwired_port_error("input", port_name));
        };
        let read = python
            .detach(|| input_mailboxes.read_raw(port_name))
            .map_err(|read_failure| PyRuntimeError::new_err(read_failure.to_string()))?;
        match read {
            Some((encoded, timestamp_ns)) => Ok((
                Some(decode_msgpack_to_python_object(python, &encoded)?),
                Some(timestamp_ns),
            )),
            None => Ok((None, None)),
        }
    }

    /// Whether a bag is waiting on `port_name`, without consuming it.
    pub(crate) fn input_port_has_data(
        &self,
        python: Python<'_>,
        port_name: &str,
    ) -> PyResult<bool> {
        let Some(input_mailboxes) = self.input_mailboxes.get() else {
            return Err(unwired_port_error("input", port_name));
        };
        Ok(python.detach(|| input_mailboxes.has_data(port_name)))
    }

    /// Publish one bag to every downstream link on `port_name`.
    ///
    /// An over-ceiling bag is refused-and-counted by the engine, never raised
    /// here — the old SDK's never-die contract for the write path.
    #[pyo3(signature = (port_name, bag, timestamp_ns = None))]
    pub(crate) fn write_to_output_port(
        &self,
        python: Python<'_>,
        port_name: &str,
        bag: &Bound<'_, PyAny>,
        timestamp_ns: Option<i64>,
    ) -> PyResult<()> {
        let Some(output_writer) = self.output_writer.get() else {
            return Err(unwired_port_error("output", port_name));
        };
        let encoded = encode_bag_to_msgpack(bag)?;
        // Default stamp is raw CLOCK_MONOTONIC, bug-compatible with the old
        // SDK's NativeOutputs.write — NOT the MediaClock epoch the engine's
        // Rust processors stamp with. Unifying the two epochs is a flagged
        // owner decision.
        let timestamp_ns = timestamp_ns.unwrap_or_else(|| monotonic_clock_now_ns() as i64);
        match python.detach(|| output_writer.write_raw(port_name, &encoded, timestamp_ns)) {
            Ok(()) => Ok(()),
            Err(Error::PayloadExceedsChannelCeiling { .. }) => {
                tracing::debug!(
                    port_name,
                    "bag refused over the channel ceiling; dropped without raising"
                );
                Ok(())
            }
            Err(write_failure) => Err(PyRuntimeError::new_err(write_failure.to_string())),
        }
    }
}

fn unwired_port_error(direction: &str, port_name: &str) -> PyErr {
    PyRuntimeError::new_err(format!(
        "{direction} port {port_name:?} is not wired: this processor declared no {direction} \
         ports, so the engine allocated no links for it"
    ))
}
