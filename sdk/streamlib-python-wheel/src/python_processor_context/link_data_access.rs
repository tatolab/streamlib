// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use pyo3::prelude::*;
use streamlib::sdk::iceoryx2::WhatIsKnownOfAnInboundLinksStampClock;

use crate::python_processor_link_data_access::PythonProcessorLinkDataAccess;

use super::gpu_context::PythonGpuContextLimitedAccess;

/// A processor's input ports, as `ctx.inputs`.
///
/// It carries the same GPU capability the context exposes as
/// `ctx.gpu_limited_access` because this is where the two knowledges meet: the
/// consumer names the type it is reading into, and the context holds the route
/// to the engine's surfaces.
#[pyclass(name = "LinkInputDataReader", module = "streamlib", frozen)]
pub(crate) struct PythonLinkInputDataReader {
    pub(super) link_data_access: Py<PythonProcessorLinkDataAccess>,
    pub(super) gpu_limited_access_context: Py<PythonGpuContextLimitedAccess>,
    /// The bridge's blocking round trip to the parent, for the one question
    /// this process cannot answer for itself: which machine's clock a link
    /// carrying from another runtime takes its stamps on. `None` in a process
    /// with no parent to ask, where no such link can exist either.
    ///
    /// Held here rather than reached through the GPU exchange client: that one
    /// is built only where the surface socket is too, and this question has
    /// nothing to do with surfaces.
    pub(super) ask_the_parent: Option<Py<PyAny>>,
}

#[pymethods]
impl PythonLinkInputDataReader {
    /// The next bag on `port_name`, or `None` when the mailbox is empty.
    ///
    /// `into` is the opt-in strictness dial: a TypedDict casts for free, a
    /// dataclass or pydantic model constructs and validates, and a bag that
    /// does not fit raises here rather than travelling on.
    ///
    /// A constructing target is offered this processor's GPU capability while
    /// it builds — see `gpu_limited_access_of_the_typed_read_in_progress`.
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

    /// The next bag with its stamp, or `(None, None)` when empty.
    fn read_with_timestamp<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
    ) -> PyResult<(Option<Bound<'py, PyAny>>, Option<i64>)> {
        self.link_data_access
            .get()
            .read_from_input_port_with_timestamp(python, port_name)
    }

    /// The next bag on `port_name` with the link it arrived on, or `None`.
    ///
    /// Any number of links may enter one input port, and each is one producer.
    /// This is how a many-input processor tells them apart: the name is the
    /// source channel the link subscribed to — or, for a link carrying from
    /// another runtime, that port's mesh address — which the engine knows and
    /// a producer cannot misstate.
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

    /// The next bag on `port_name` with its link and its timestamp, or `None`.
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

    /// Every link feeding `port_name`, in wiring order.
    ///
    /// Readable in `setup()`, which is how a sink learns how many producers it
    /// owes before the first bag arrives. A port nothing is connected to lists
    /// none.
    fn inbound_link_names(&self, port_name: &str) -> PyResult<Vec<String>> {
        self.link_data_access
            .get()
            .inbound_links_of_input_port(port_name)
    }

    /// Which machine's monotonic clock the bags arriving on one link of
    /// `port_name` were stamped on, as that machine's boot-session UUID text.
    ///
    /// Two stamps taken on two machines are readings of two unrelated clocks,
    /// so a processor fanning several links in asks this before it compares one
    /// link's stamps against another's. A link from this runtime always answers
    /// this machine; one carrying from another runtime answers `None` until its
    /// first bag lands, and answers a different machine after its peer comes
    /// back on a fresh boot.
    ///
    /// Costs a round trip to the runtime for a link carrying from another
    /// runtime, which this process holds no mesh session to answer for itself.
    /// Read it when a link wires or a track opens, not once per bag.
    fn inbound_link_stamp_clock_identity(
        &self,
        python: Python<'_>,
        port_name: &str,
        inbound_link_name: &str,
    ) -> PyResult<Option<String>> {
        match self
            .link_data_access
            .get()
            .inbound_link_stamp_clock_of_input_port(port_name, inbound_link_name)?
        {
            WhatIsKnownOfAnInboundLinksStampClock::TheMachine(machine) => {
                Ok(Some(machine.to_string()))
            }
            WhatIsKnownOfAnInboundLinksStampClock::NothingHasCrossedItYet
            | WhatIsKnownOfAnInboundLinksStampClock::ItsMachineNamesNoClockOfItsOwn
            | WhatIsKnownOfAnInboundLinksStampClock::NoSuchLinkFeedsThatPort => Ok(None),
            WhatIsKnownOfAnInboundLinksStampClock::OnlyTheAppProcessCanSay => {
                self.ask_the_parent_which_machine_stamped(python, inbound_link_name)
            }
        }
    }

    /// Whether a bag is waiting on `port_name`, without consuming it.
    fn has_data(&self, python: Python<'_>, port_name: &str) -> PyResult<bool> {
        self.link_data_access
            .get()
            .input_port_has_data(python, port_name)
    }
}

impl PythonLinkInputDataReader {
    /// Ask the runtime which machine stamped the bags arriving on a link this
    /// process holds no mesh session to answer for.
    ///
    /// A refusal is `None` rather than a raise: the caller asked which clock a
    /// link is on, and "the runtime could not say" is an answer to that — one
    /// that stops a stamp being compared, which is the safe direction. The
    /// reason is logged rather than swallowed.
    fn ask_the_parent_which_machine_stamped(
        &self,
        python: Python<'_>,
        inbound_link_name: &str,
    ) -> PyResult<Option<String>> {
        let Some(ask_the_parent) = self.ask_the_parent.as_ref() else {
            return Ok(None);
        };
        let request = pyo3::types::PyDict::new(python);
        request.set_item("op", "inbound_link_stamp_clock_identity")?;
        request.set_item("inbound_link_name", inbound_link_name)?;
        let answer = match ask_the_parent.bind(python).call1((request,)) {
            Ok(answer) => answer,
            Err(the_parent_did_not_answer) => {
                tracing::warn!(
                    "the runtime did not say which machine stamps the bags on \
                     `{inbound_link_name}`, so nothing here may be compared against them: \
                     {the_parent_did_not_answer}"
                );
                return Ok(None);
            }
        };
        // Absent is what the runtime answers for a link nothing has crossed
        // yet and for an address it carries nothing from; an answer that is not
        // a mapping at all lands here too, and must not read as the same thing
        // silently.
        match answer.get_item("stamp_clock_identity") {
            Ok(machine) => machine.extract::<Option<String>>(),
            Err(not_a_mapping) => {
                if !answer.is_instance_of::<pyo3::types::PyDict>() {
                    tracing::warn!(
                        "the runtime's answer about `{inbound_link_name}` was not a mapping this \
                         build can read, so nothing here may be compared against its stamps: \
                         {not_a_mapping}"
                    );
                }
                Ok(None)
            }
        }
    }
}

/// A processor's output ports, as `ctx.outputs`.
#[pyclass(name = "LinkOutputDataWriter", module = "streamlib", frozen)]
pub(crate) struct PythonLinkOutputDataWriter {
    pub(super) link_data_access: Py<PythonProcessorLinkDataAccess>,
}

#[pymethods]
impl PythonLinkOutputDataWriter {
    /// Publish one bag to every downstream link on `port_name`.
    #[pyo3(signature = (port_name, bag, timestamp_ns = None))]
    fn write(
        &self,
        python: Python<'_>,
        port_name: &str,
        bag: &Bound<'_, PyAny>,
        timestamp_ns: Option<i64>,
    ) -> PyResult<()> {
        self.link_data_access
            .get()
            .write_to_output_port(python, port_name, bag, timestamp_ns)
    }
}

/// The typed cast's claim, over a real link and a real surface-share service.
///
/// What is proven here is the seam, not a type: a bag crosses a wired link, the
/// read constructs a frame class **the wheel does not ship**, and that class
/// pins its surface for exactly as long as it lives. If this only worked for
/// `VideoFrame` the pattern would be a private handshake, so the target here is
/// deliberately somebody else's.
#[cfg(all(test, target_os = "linux"))]
mod typed_read_claim_tests;
