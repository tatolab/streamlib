// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use pyo3::prelude::*;

use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;

use super::gpu_context::PythonGpuContextLimitedAccess;

/// A processor's input ports, as `ctx.inputs`.
///
/// It carries the same GPU capability the context exposes as
/// `ctx.gpu_limited_access` because this is where the two knowledges meet: the
/// consumer names the type it is reading into, and the context holds the route
/// to the engine's surfaces.
#[pyclass(name = "LinkInputDataReader", module = "tatolab.stream", frozen)]
pub(crate) struct PythonLinkInputDataReader {
    pub(super) link_data_access: Py<PythonProcessorLinkDataAccess>,
    pub(super) gpu_limited_access_context: Py<PythonGpuContextLimitedAccess>,
}

#[pymethods]
impl PythonLinkInputDataReader {
    /// The next message's payload on `port_name` as `bytes`, or `None` when the
    /// mailbox is empty.
    ///
    /// With `into`, the read returns `into.from_bytes(payload)`, so a payload
    /// that does not fit raises here rather than travelling on. `from_bytes` is
    /// offered this processor's GPU capability while it runs — see
    /// `gpu_limited_access_of_the_typed_read_in_progress`.
    #[pyo3(signature = (port_name, *, into = None))]
    fn read<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        into: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.link_data_access
            .get()
            .read_from_input_port_offering_gpu_access(
                python,
                port_name,
                into,
                Some(self.gpu_limited_access_context.bind(python)),
            )
    }

    /// The next message's payload with its stamp, or `(None, None)` when empty.
    fn read_with_timestamp<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
    ) -> PyResult<(Option<Bound<'py, PyAny>>, Option<i64>)> {
        self.link_data_access
            .get()
            .read_from_input_port_with_timestamp(python, port_name)
    }

    /// The next message's payload on `port_name` with the link it arrived on,
    /// or `None`.
    ///
    /// Any number of links may enter one input port, and each is one producer.
    /// This is how a many-input processor tells them apart: the name is the
    /// source channel the link subscribed to, which the engine knows and a
    /// producer cannot misstate.
    #[pyo3(signature = (port_name, *, into = None))]
    fn read_from_inbound_link<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        into: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Option<(Bound<'py, PyAny>, String)>> {
        self.link_data_access
            .get()
            .read_from_input_port_naming_its_inbound_link(
                python,
                port_name,
                into,
                Some(self.gpu_limited_access_context.bind(python)),
            )
    }

    /// The next message's payload on `port_name` with its link and its
    /// timestamp, or `None`.
    ///
    /// What a many-track sink needs to restate a producer's own timing: the
    /// link names the producer and the stamp is the one that producer wrote,
    /// which is the source frame's instant rather than the moment of the read.
    #[pyo3(signature = (port_name, *, into = None))]
    fn read_from_inbound_link_with_timestamp<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        into: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Option<(Bound<'py, PyAny>, String, i64)>> {
        self.link_data_access
            .get()
            .read_from_input_port_naming_its_inbound_link_and_timestamp(
                python,
                port_name,
                into,
                Some(self.gpu_limited_access_context.bind(python)),
            )
    }

    /// The next message on `port_name` as a `Message` — payload, stamp, group
    /// flag, the messages its link lost since the previous read from it, and
    /// the link — or `None`.
    ///
    /// `into` casts the payload as it does for `read`.
    #[pyo3(signature = (port_name, *, into = None))]
    fn read_message<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        into: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.link_data_access.get().read_message_from_input_port(
            python,
            port_name,
            into,
            Some(self.gpu_limited_access_context.bind(python)),
        )
    }

    /// Every link feeding `port_name`, in wiring order.
    ///
    /// Readable in `setup()`, which is how a sink learns how many producers it
    /// owes before the first message arrives. A port nothing is connected to lists
    /// none.
    fn inbound_link_names(&self, port_name: &str) -> PyResult<Vec<String>> {
        self.link_data_access
            .get()
            .inbound_links_of_input_port(port_name)
    }

    /// Whether a message is waiting on `port_name`, without consuming it.
    fn has_data(&self, python: Python<'_>, port_name: &str) -> PyResult<bool> {
        self.link_data_access
            .get()
            .input_port_has_data(python, port_name)
    }
}

/// A processor's output ports, as `ctx.outputs`.
#[pyclass(name = "LinkOutputDataWriter", module = "tatolab.stream", frozen)]
pub(crate) struct PythonLinkOutputDataWriter {
    pub(super) link_data_access: Py<PythonProcessorLinkDataAccess>,
}

#[pymethods]
impl PythonLinkOutputDataWriter {
    /// Publish one message carrying `payload`, which must be `bytes`, to every
    /// downstream link on `port_name`.
    #[pyo3(signature = (port_name, payload, timestamp_ns = None, starts_group = true))]
    fn write(
        &self,
        python: Python<'_>,
        port_name: &str,
        payload: &Bound<'_, PyAny>,
        timestamp_ns: Option<i64>,
        starts_group: bool,
    ) -> PyResult<()> {
        self.link_data_access.get().write_to_output_port(
            python,
            port_name,
            payload,
            timestamp_ns,
            starts_group,
        )
    }
}

/// The typed cast's claim, over a real link and a real surface-share service.
///
/// What is proven here is the seam, not a type: a message crosses a wired link,
/// the read builds a frame class **the wheel does not ship** through its
/// `from_bytes`, and that class pins its surface for exactly as long as it
/// lives. If this only worked for
/// `VideoFrame` the pattern would be a private handshake, so the target here is
/// deliberately somebody else's.
#[cfg(all(test, target_os = "linux"))]
mod typed_read_claim_tests;
