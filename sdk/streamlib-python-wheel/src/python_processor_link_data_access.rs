// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The data plane a Python processor reads and writes through.
//!
//! This is where the GIL-release contract is kept for the per-bag path: the
//! conversion between Python objects and msgpack needs the GIL and holds it;
//! the iceoryx2 call that can block does not, and runs detached. Holding the
//! GIL across that call would stall the interpreter's other threads for its
//! duration.
//!
//! A helper process wires this object itself from the port wiring the parent
//! sends it, then reads and writes through the same methods the parent's
//! wiring path installs into. Every iceoryx2 port here is `!Send` under the
//! engine's discipline, so a child must create and drive all of them from one
//! thread.

use std::sync::{Arc, OnceLock};

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use streamlib::sdk::error::Error;
use streamlib::sdk::iceoryx2::{
    ChannelEgressConfig, ChannelTrustTier, Iceoryx2Node, InputMailboxesInner, OutputWriterInner,
    ReadMode, SchemaIdentWire,
};

use crate::python_bag_conversion::{decode_msgpack_to_python_object, encode_bag_to_msgpack};
use crate::python_logging::monotonic_clock_now_ns;

/// One processor's links, as seen from Python.
///
/// Frozen because the engine hands the same object to the processor's own
/// thread and reads it from the wiring path; the interior `OnceLock`s are
/// written once before the processor's first callback.
#[pyclass(name = "ProcessorLinkDataAccess", module = "streamlib", frozen)]
pub(crate) struct PythonProcessorLinkDataAccess {
    input_mailboxes: OnceLock<Arc<InputMailboxesInner>>,
    output_writer: OnceLock<Arc<OutputWriterInner>>,
    /// The child's own iceoryx2 node, present only when this object wired
    /// itself. The parent's copy is wired by the compiler op and leaves this
    /// empty — the node it would name belongs to the engine.
    iceoryx2_node: OnceLock<Iceoryx2Node>,
}

impl PythonProcessorLinkDataAccess {
    pub(crate) fn new() -> Self {
        Self {
            input_mailboxes: OnceLock::new(),
            output_writer: OnceLock::new(),
            iceoryx2_node: OnceLock::new(),
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

    fn helper_process_output_plane(&self) -> PyResult<(&Iceoryx2Node, &Arc<OutputWriterInner>)> {
        match (self.iceoryx2_node.get(), self.output_writer.get()) {
            (Some(node), Some(output_writer)) => Ok((node, output_writer)),
            _ => Err(not_a_helper_process_data_plane_error()),
        }
    }

    fn helper_process_input_plane(&self) -> PyResult<(&Iceoryx2Node, &Arc<InputMailboxesInner>)> {
        match (self.iceoryx2_node.get(), self.input_mailboxes.get()) {
            (Some(node), Some(input_mailboxes)) => Ok((node, input_mailboxes)),
            _ => Err(not_a_helper_process_data_plane_error()),
        }
    }
}

#[pymethods]
impl PythonProcessorLinkDataAccess {
    /// Build a helper process's own data plane, with its own iceoryx2 node.
    ///
    /// The parent's copy of this object is built in Rust and wired by the
    /// compiler op; this is the constructor a child uses to wire itself from
    /// the port wiring the parent sent it.
    #[new]
    fn open_for_helper_process(python: Python<'_>) -> PyResult<Self> {
        let node = python
            .detach(Iceoryx2Node::new)
            .map_err(|node_failure| PyRuntimeError::new_err(node_failure.to_string()))?;
        let wiring = Self::new();
        let _ = wiring.iceoryx2_node.set(node);
        let _ = wiring
            .input_mailboxes
            .set(Arc::new(InputMailboxesInner::new()));
        let _ = wiring.output_writer.set(Arc::new(OutputWriterInner::new()));
        Ok(wiring)
    }

    /// Open this processor's publisher and one destination notifier for a link
    /// out of `port_name`.
    ///
    /// One call per link. The publisher is installed once — the first link out
    /// of a port creates it and every later link only appends its notifier —
    /// because iceoryx2 admits exactly one publisher per channel.
    #[pyo3(signature = (
        port_name,
        channel_service_name,
        dest_notify_service_name,
        expected_payload_bytes,
        max_payload_bytes_per_channel,
        max_queued_messages,
        max_subscribers,
        notify_max_notifiers,
        enable_safe_overflow,
        link_id,
        schema = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn wire_output_link(
        &self,
        python: Python<'_>,
        port_name: &str,
        channel_service_name: &str,
        dest_notify_service_name: &str,
        expected_payload_bytes: usize,
        max_payload_bytes_per_channel: usize,
        max_queued_messages: usize,
        max_subscribers: usize,
        notify_max_notifiers: usize,
        enable_safe_overflow: bool,
        link_id: &str,
        schema: Option<(String, String, String, u32, u32, u32)>,
    ) -> PyResult<()> {
        let (node, output_writer) = self.helper_process_output_plane()?;
        let schema_ident = match schema {
            Some((org, package, type_name, major, minor, patch)) => SchemaIdentWire::from_segments(
                &org, &package, &type_name, major, minor, patch,
            )
            .map_err(|schema_failure| {
                PyValueError::new_err(format!(
                    "output port {port_name:?} declared a schema the wire cannot carry: \
                     {schema_failure:?}"
                ))
            })?,
            None => SchemaIdentWire::default(),
        };

        python.detach(|| -> Result<(), Error> {
            if !output_writer.has_channel_publisher(port_name) {
                let channel = node.open_or_create_service(
                    channel_service_name,
                    max_subscribers,
                    max_queued_messages,
                    enable_safe_overflow,
                )?;
                let publisher = channel.create_publisher(expected_payload_bytes)?;
                output_writer.set_channel_publisher(
                    port_name,
                    schema_ident,
                    publisher,
                    ChannelEgressConfig {
                        service_name: channel_service_name.to_string(),
                        trust_tier: ChannelTrustTier::Trusted,
                        expected_payload_bytes,
                        ceiling_bytes: max_payload_bytes_per_channel,
                    },
                );
            }
            let notify_service =
                node.open_or_create_notify_service(dest_notify_service_name, notify_max_notifiers)?;
            output_writer.add_channel_notifier(port_name, link_id, notify_service.create_notifier()?);
            Ok(())
        })
        .map_err(|wiring_failure| {
            PyRuntimeError::new_err(format!(
                "could not wire output port {port_name:?} onto channel \
                 {channel_service_name:?}: {wiring_failure}"
            ))
        })
    }

    /// Open this processor's subscriber for a link into `port_name`, plus the
    /// one listener every input shares.
    ///
    /// One call per link. The mailbox and the destination-keyed listener are
    /// installed once — fan-in appends subscribers to the same port, and
    /// iceoryx2 admits exactly one listener per destination.
    #[pyo3(signature = (
        port_name,
        channel_service_name,
        notify_service_name,
        read_mode,
        max_queued_messages,
        max_subscribers,
        notify_max_notifiers,
        enable_safe_overflow,
        link_id,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn wire_input_link(
        &self,
        python: Python<'_>,
        port_name: &str,
        channel_service_name: &str,
        notify_service_name: &str,
        read_mode: &str,
        max_queued_messages: usize,
        max_subscribers: usize,
        notify_max_notifiers: usize,
        enable_safe_overflow: bool,
        link_id: &str,
    ) -> PyResult<()> {
        let (node, input_mailboxes) = self.helper_process_input_plane()?;
        let read_mode = match read_mode {
            "skip_to_latest" => ReadMode::SkipToLatest,
            "read_next_in_order" => ReadMode::ReadNextInOrder,
            unknown => {
                return Err(PyValueError::new_err(format!(
                    "input port {port_name:?} was wired with read mode {unknown:?}; the engine \
                     sends only \"skip_to_latest\" or \"read_next_in_order\""
                )));
            }
        };

        python.detach(|| -> Result<(), Error> {
            if !input_mailboxes.has_port(port_name) {
                input_mailboxes.add_port(port_name, max_queued_messages, read_mode);
            }
            let channel = node.open_or_create_service(
                channel_service_name,
                max_subscribers,
                max_queued_messages,
                enable_safe_overflow,
            )?;
            input_mailboxes.add_channel_subscriber(port_name, link_id, channel.create_subscriber()?);
            if !input_mailboxes.has_listener() {
                let notify_service =
                    node.open_or_create_notify_service(notify_service_name, notify_max_notifiers)?;
                input_mailboxes.set_listener(notify_service.create_listener()?);
            }
            Ok(())
        })
        .map_err(|wiring_failure| {
            PyRuntimeError::new_err(format!(
                "could not wire input port {port_name:?} onto channel \
                 {channel_service_name:?}: {wiring_failure}"
            ))
        })
    }

    /// The fd that becomes readable when any upstream publishes.
    ///
    /// Owned by the listener: the caller must not close it, and must stop
    /// selecting on it before this object is dropped.
    fn input_listener_fd(&self) -> Option<i32> {
        self.input_mailboxes.get()?.listener_fd()
    }

    /// Clear the listener's pending events so its fd goes not-readable again.
    ///
    /// iceoryx2 coalesces notifications into one fd transition, so a wait that
    /// is not followed by a drain returns immediately on the same event.
    fn drain_input_listener(&self, python: Python<'_>) {
        if let Some(input_mailboxes) = self.input_mailboxes.get() {
            python.detach(|| input_mailboxes.drain_listener());
        }
    }

    /// Whether any input port has a bag waiting.
    ///
    /// One wake does not mean one frame — iceoryx2 coalesces — so a reactive
    /// loop asks this rather than assuming.
    fn any_input_port_has_data(&self, python: Python<'_>) -> bool {
        match self.input_mailboxes.get() {
            Some(input_mailboxes) => python.detach(|| input_mailboxes.any_port_has_data()),
            None => false,
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyDict;

    /// Unique per run: iceoryx2 service state is machine-global and outlives a
    /// crashed process, so a fixed name makes one bad run poison every later
    /// one.
    fn unique_channel_names(label: &str) -> (String, String) {
        let run = std::process::id();
        (
            format!("wiring{run}_{label}/frames_to_downstream"),
            format!("wiring{run}_{label}_dest/notify"),
        )
    }

    fn helper_plane(python: Python<'_>) -> PythonProcessorLinkDataAccess {
        PythonProcessorLinkDataAccess::open_for_helper_process(python).unwrap()
    }

    /// The whole point of the wiring surface: a helper process opens its own
    /// publisher and its own subscriber on the services the parent named, and a
    /// bag written on one shows up on the other. Both planes live in this test's
    /// thread because iceoryx2's ports are `!Send`.
    #[test]
    fn a_bag_written_on_a_wired_output_arrives_on_a_wired_input() {
        Python::initialize();
        Python::attach(|python| {
            let (channel, notify) = unique_channel_names("roundtrip");
            let source = helper_plane(python);
            let destination = helper_plane(python);

            // The destination subscribes first: iceoryx2 drops a send with no
            // subscriber attached, so wiring the publisher first would race.
            destination
                .wire_input_link(
                    python,
                    "frames_from_upstream",
                    &channel,
                    &notify,
                    "read_next_in_order",
                    8,
                    2,
                    1,
                    true,
                    "link-1",
                )
                .unwrap();
            source
                .wire_output_link(
                    python, "frames_to_downstream", &channel, &notify, 1024, 1 << 20, 8, 2, 1,
                    true, "link-1", None,
                )
                .unwrap();

            let bag = PyDict::new(python);
            bag.set_item("frame_index", 7i64).unwrap();
            source
                .write_to_output_port(python, "frames_to_downstream", bag.as_any(), None)
                .unwrap();

            let received = destination
                .read_from_input_port(python, "frames_from_upstream")
                .unwrap()
                .expect("the wired input received nothing");
            assert_eq!(
                received
                    .get_item("frame_index")
                    .unwrap()
                    .extract::<i64>()
                    .unwrap(),
                7
            );
        });
    }

    /// Fan-out sends one entry per link, and iceoryx2 admits exactly one
    /// publisher per channel — so the second link must append its notifier
    /// rather than open a second publisher.
    #[test]
    fn a_second_link_out_of_one_port_reuses_the_publisher() {
        Python::initialize();
        Python::attach(|python| {
            let (channel, notify) = unique_channel_names("fanout");
            let (_, second_notify) = unique_channel_names("fanout_second");
            let source = helper_plane(python);

            source
                .wire_output_link(
                    python, "frames_to_downstream", &channel, &notify, 1024, 1 << 20, 8, 2, 1,
                    true, "link-1", None,
                )
                .unwrap();
            source
                .wire_output_link(
                    python,
                    "frames_to_downstream",
                    &channel,
                    &second_notify,
                    1024,
                    1 << 20,
                    8,
                    2,
                    1,
                    true,
                    "link-2",
                    None,
                )
                .expect("a second link out of one port must not reopen the publisher");
        });
    }

    /// The parent's copy is wired by the compiler op and owns no node of its
    /// own; asking it to wire itself must say so rather than panic on a missing
    /// node.
    #[test]
    fn an_engine_wired_plane_refuses_to_wire_itself() {
        Python::initialize();
        Python::attach(|python| {
            let engine_wired = PythonProcessorLinkDataAccess::new();
            let (channel, notify) = unique_channel_names("engineowned");
            let refusal = engine_wired
                .wire_output_link(
                    python, "frames_to_downstream", &channel, &notify, 1024, 1 << 20, 8, 2, 1,
                    true, "link-1", None,
                )
                .unwrap_err();
            assert!(
                refusal.to_string().contains("helper process wires its own"),
                "unexpected refusal: {refusal}"
            );
        });
    }

    #[test]
    fn an_unknown_read_mode_is_refused_by_name() {
        Python::initialize();
        Python::attach(|python| {
            let (channel, notify) = unique_channel_names("readmode");
            let destination = helper_plane(python);
            let refusal = destination
                .wire_input_link(
                    python,
                    "frames_from_upstream",
                    &channel,
                    &notify,
                    "whenever",
                    8,
                    2,
                    1,
                    true,
                    "link-1",
                )
                .unwrap_err();
            assert!(
                refusal.to_string().contains("whenever"),
                "the refusal must name what it got: {refusal}"
            );
        });
    }
}

fn not_a_helper_process_data_plane_error() -> PyErr {
    PyRuntimeError::new_err(
        "this processor's links belong to the engine that wired them, so they cannot be wired \
         again from Python — only a helper process wires its own",
    )
}

fn unwired_port_error(direction: &str, port_name: &str) -> PyErr {
    PyRuntimeError::new_err(format!(
        "{direction} port {port_name:?} is not wired: this processor declared no {direction} \
         ports, so the engine allocated no links for it"
    ))
}
