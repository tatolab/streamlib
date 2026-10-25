// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use pyo3::exceptions::{PyRuntimeError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

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
    /// The bridge's blocking round trip to the parent, which holds the
    /// catalog entries a read asks after.
    pub(super) escalate_request_to_parent: Option<Py<PyAny>>,
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

    /// Count the message last read from `port_name` as lost on the link that
    /// delivered it, once — for a message whose frame its producer recycled
    /// before this node's claim.
    fn count_the_message_last_read_as_lost(&self, port_name: &str) -> PyResult<()> {
        self.link_data_access
            .get()
            .count_the_message_last_read_from_input_port_as_lost(port_name)
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

    /// The catalog entry the output port feeding `port_name` carries now, as
    /// `{"section": ..., "entry": {...}}`, or `None`.
    ///
    /// A port fed by more than one link names the one to read with
    /// `inbound_link`.
    #[pyo3(signature = (port_name, *, inbound_link = None))]
    fn inbound_link_catalog_entry<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        inbound_link: Option<&str>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let op = PyDict::new(python);
        op.set_item("op", "read_inbound_link_catalog_entry")?;
        op.set_item("port", port_name)?;
        if let Some(inbound_link) = inbound_link {
            op.set_item("inbound_link", inbound_link)?;
        }
        let response = catalog_entry_round_trip_to_parent(
            python,
            self.escalate_request_to_parent.as_ref(),
            &op,
        )?;
        let catalog_entry = response.call_method1("get", ("catalog_entry",))?;
        Ok((!catalog_entry.is_none()).then_some(catalog_entry))
    }
}

/// A processor's output ports, as `ctx.outputs`.
#[pyclass(name = "LinkOutputDataWriter", module = "tatolab.stream", frozen)]
pub(crate) struct PythonLinkOutputDataWriter {
    pub(super) link_data_access: Py<PythonProcessorLinkDataAccess>,
    /// The bridge's blocking round trip to the parent, which holds the
    /// catalog entries this processor's ports carry.
    pub(super) escalate_request_to_parent: Option<Py<PyAny>>,
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

    /// Replace the catalog entry `port_name` carries with `entry`, one of
    /// `tatolab.stream.catalog`'s entries. The parent parses it with hang's
    /// catalog types and refuses one they do not read.
    fn set_catalog_entry(
        &self,
        python: Python<'_>,
        port_name: &str,
        entry: &Bound<'_, PyAny>,
    ) -> PyResult<()> {
        let not_a_catalog_entry = |_| {
            PyTypeError::new_err(format!(
                "output port {port_name:?}: set_catalog_entry takes one of tatolab.stream.catalog's \
                 entries, got {}",
                entry
                    .get_type()
                    .name()
                    .map(|type_name| type_name.to_string())
                    .unwrap_or_default()
            ))
        };
        let section = entry.getattr("section").map_err(not_a_catalog_entry)?;
        let fields = entry.call_method0("to_dict").map_err(not_a_catalog_entry)?;
        let op = PyDict::new(python);
        op.set_item("op", "set_output_port_catalog_entry")?;
        op.set_item("port", port_name)?;
        op.set_item("section", section)?;
        op.set_item("entry", fields)?;
        catalog_entry_round_trip_to_parent(python, self.escalate_request_to_parent.as_ref(), &op)?;
        Ok(())
    }
}

/// One catalog-entry escalate round trip to the parent, whose refusal raises
/// as the bridge raises it.
fn catalog_entry_round_trip_to_parent<'py>(
    python: Python<'py>,
    escalate_request_to_parent: Option<&Py<PyAny>>,
    op: &Bound<'py, PyDict>,
) -> PyResult<Bound<'py, PyAny>> {
    escalate_request_to_parent
        .ok_or_else(|| {
            PyRuntimeError::new_err(
                "a catalog entry lives on the parent's graph, and this context has no route to \
                 the parent",
            )
        })?
        .bind(python)
        .call1((op,))
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
