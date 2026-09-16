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

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use pyo3::exceptions::{PyKeyError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use streamlib::sdk::descriptors::AudioWindowContractDeclaredValues;
use streamlib::sdk::error::Error;
use streamlib::sdk::iceoryx2::{
    ChannelEgressConfig, ChannelTrustTier, HelperProcessLossCountBoardWriter,
    ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE, Iceoryx2Node, InboundLinkName, InputMailboxesInner,
    OutputWriterInner, ReadMode, ResolvedAudioWindowContract,
};

use crate::python_bag_conversion::{
    cast_decoded_bag_into_read_target, decode_msgpack_to_python_object, encode_bag_to_msgpack,
};
use crate::python_helper_process_pixel_exchange::warn_through_the_childs_log_module;
use crate::python_helper_process_spawn_host::HELPER_PROCESS_PROCESSOR_ID_ENVIRONMENT_VARIABLE;
use crate::python_logging::monotonic_clock_now_ns;
use crate::python_processor_context::PythonGpuContextLimitedAccess;
use crate::python_processor_declaration::read_a_channel_count_or_the_source_spelling;

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
    /// The board this helper's loss counts are mirrored onto for its parent,
    /// once its setup opened the one the parent named.
    loss_count_board_writer: OnceLock<Arc<HelperProcessLossCountBoardWriter>>,
    /// The ports the processor class declared. A declared port no link has
    /// reached reads as empty and swallows writes; an undeclared name is
    /// refused.
    declared_input_ports: parking_lot::Mutex<HashSet<String>>,
    declared_output_ports: parking_lot::Mutex<HashSet<String>>,
}

impl PythonProcessorLinkDataAccess {
    pub(crate) fn new() -> Self {
        Self {
            input_mailboxes: OnceLock::new(),
            output_writer: OnceLock::new(),
            iceoryx2_node: OnceLock::new(),
            loss_count_board_writer: OnceLock::new(),
            declared_input_ports: parking_lot::Mutex::new(HashSet::new()),
            declared_output_ports: parking_lot::Mutex::new(HashSet::new()),
        }
    }

    /// A helper process's own data plane over an iceoryx2 node it already opened.
    pub(crate) fn over_helper_process_iceoryx2_node(node: Iceoryx2Node) -> Self {
        let wiring = Self::new();
        let _ = wiring.iceoryx2_node.set(node);
        let _ = wiring
            .input_mailboxes
            .set(Arc::new(InputMailboxesInner::new()));
        let _ = wiring.output_writer.set(Arc::new(OutputWriterInner::new()));
        wiring
    }

    /// The mailboxes `port_name` reads from; `None` for a declared input port
    /// no link has reached yet, which reads as empty rather than as an error.
    fn input_mailboxes_reaching(
        &self,
        port_name: &str,
    ) -> PyResult<Option<&Arc<InputMailboxesInner>>> {
        match self.input_mailboxes.get() {
            Some(input_mailboxes) if input_mailboxes.has_port(port_name) => {
                Ok(Some(input_mailboxes))
            }
            _ if self.declared_input_ports.lock().contains(port_name) => Ok(None),
            _ => Err(unwired_port_error("input", port_name)),
        }
    }

    /// The writer `port_name` publishes through; `None` for a declared output
    /// port no link has reached yet, whose bags go nowhere rather than raising.
    fn output_writer_reaching(&self, port_name: &str) -> PyResult<Option<&Arc<OutputWriterInner>>> {
        match self.output_writer.get() {
            Some(output_writer) if output_writer.has_port(port_name) => Ok(Some(output_writer)),
            _ if self.declared_output_ports.lock().contains(port_name) => Ok(None),
            _ => Err(unwired_port_error("output", port_name)),
        }
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

    /// The next bag on `port_name`, offering `offered_gpu_limited_access` to
    /// whatever `into` constructs.
    ///
    /// The one read body; the context-carrying reader passes its capability
    /// and the bare data plane passes nothing.
    pub(crate) fn read_from_input_port_offering_gpu_access<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        into: Option<&Bound<'py, PyAny>>,
        offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let Some(input_mailboxes) = self.input_mailboxes_reaching(port_name)? else {
            return Ok(None);
        };
        let read = python
            .detach(|| input_mailboxes.read_raw(port_name))
            .map_err(|read_failure| PyRuntimeError::new_err(read_failure.to_string()))?;
        match read {
            Some((encoded, _timestamp_ns)) => decode_one_bag_into(
                python,
                port_name,
                &encoded,
                into,
                offered_gpu_limited_access,
            )
            .map(Some),
            None => Ok(None),
        }
    }

    /// The next bag on `port_name` with the inbound link it arrived on, or
    /// `None` when the mailbox is empty.
    ///
    /// The read a destination taking many links on one port uses: each inbound
    /// link is one producer, named by the source channel name it subscribed to.
    pub(crate) fn read_from_input_port_naming_its_inbound_link<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        into: Option<&Bound<'py, PyAny>>,
        offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
    ) -> PyResult<Option<(Bound<'py, PyAny>, String)>> {
        Ok(self
            .read_from_input_port_naming_its_inbound_link_and_timestamp(
                python,
                port_name,
                into,
                offered_gpu_limited_access,
            )?
            .map(|(bag, inbound_link_name, _timestamp_ns)| (bag, inbound_link_name)))
    }

    /// The same read, keeping the frame header's timestamp.
    ///
    /// A many-track sink needs both halves at once: the link says which
    /// producer sent the bag, and the stamp is the producer's own — the source
    /// frame's instant, not the moment the bag was read.
    pub(crate) fn read_from_input_port_naming_its_inbound_link_and_timestamp<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        into: Option<&Bound<'py, PyAny>>,
        offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
    ) -> PyResult<Option<(Bound<'py, PyAny>, String, i64)>> {
        let Some(input_mailboxes) = self.input_mailboxes_reaching(port_name)? else {
            return Ok(None);
        };
        let read = python
            .detach(|| input_mailboxes.read_raw_from_inbound_link(port_name))
            .map_err(|read_failure| PyRuntimeError::new_err(read_failure.to_string()))?;
        let Some((encoded, timestamp_ns, inbound_link_name)) = read else {
            return Ok(None);
        };
        let bag = decode_one_bag_into(
            python,
            port_name,
            &encoded,
            into,
            offered_gpu_limited_access,
        )?;
        Ok(Some((
            bag,
            inbound_link_name.as_str().to_string(),
            timestamp_ns,
        )))
    }

    /// Every inbound link feeding `port_name`, in wiring order.
    pub(crate) fn inbound_links_of_input_port(&self, port_name: &str) -> PyResult<Vec<String>> {
        let Some(input_mailboxes) = self.input_mailboxes.get() else {
            return Err(unwired_port_error("input", port_name));
        };
        Ok(input_mailboxes
            .inbound_link_names(port_name)
            .iter()
            .map(|inbound_link_name| inbound_link_name.as_str().to_string())
            .collect())
    }
}

/// Decode one bag's msgpack and, where `into` names a target, cast into it —
/// the half every read shares once the bytes are in hand.
fn decode_one_bag_into<'py>(
    python: Python<'py>,
    port_name: &str,
    encoded: &[u8],
    into: Option<&Bound<'py, PyAny>>,
    offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
) -> PyResult<Bound<'py, PyAny>> {
    let bag = decode_msgpack_to_python_object(python, encoded)?;
    match into {
        Some(read_target_type) => cast_decoded_bag_into_read_target(
            port_name,
            bag,
            read_target_type,
            offered_gpu_limited_access,
        ),
        None => Ok(bag),
    }
}

#[pymethods]
impl PythonProcessorLinkDataAccess {
    /// Build a helper process's own data plane, with its own iceoryx2 node.
    ///
    /// The parent's copy of this object is built in Rust and wired by the
    /// compiler op; this is the constructor a child uses to wire itself from
    /// the port wiring the parent sent it. The node opens in the domain root
    /// the parent handed over, and a process handed none is refused.
    #[new]
    fn open_for_helper_process(python: Python<'_>) -> PyResult<Self> {
        let iceoryx2_domain_root = std::env::var_os(ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE)
            .filter(|root| !root.is_empty())
            .ok_or_else(|| {
                PyRuntimeError::new_err(format!(
                    "{ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE} is not set: a helper process \
                     opens its iceoryx2 node only in the domain its parent runtime hands it"
                ))
            })?;
        let node_name = std::env::var(HELPER_PROCESS_PROCESSOR_ID_ENVIRONMENT_VARIABLE)
            .map_or_else(
                |_| format!("streamlib-helper/pid{}", std::process::id()),
                |processor_id| format!("streamlib-helper/{processor_id}"),
            );
        let node = python
            .detach(|| Iceoryx2Node::new(std::path::Path::new(&iceoryx2_domain_root), &node_name))
            .map_err(|node_failure| PyRuntimeError::new_err(node_failure.to_string()))?;
        Ok(Self::over_helper_process_iceoryx2_node(node))
    }

    /// Open the loss-count board the parent created for this spawn, so every
    /// link wired from here on mirrors its counts onto it.
    ///
    /// `output_port_names` is the board's output-port section in key order, as
    /// the setup command lists it.
    fn open_loss_count_board(
        &self,
        python: Python<'_>,
        service_name: &str,
        output_port_names: Vec<String>,
    ) -> PyResult<()> {
        let Some(node) = self.iceoryx2_node.get() else {
            return Err(not_a_helper_process_data_plane_error());
        };
        let board_writer = python
            .detach(|| {
                node.open_helper_process_loss_count_board_writer(service_name, &output_port_names)
            })
            .map_err(|open_failure| PyRuntimeError::new_err(open_failure.to_string()))?;
        self.loss_count_board_writer
            .set(Arc::new(board_writer))
            .map_err(|_| {
                PyRuntimeError::new_err(format!(
                    "this helper already mirrors its loss counts onto a board, so the board \
                     {service_name:?} cannot be opened as a second"
                ))
            })
    }

    /// Open this processor's publisher and one destination notifier for a link
    /// out of `port_name`.
    ///
    /// One call per link. The publisher is installed once — the first link out
    /// of a port creates it and every later link only appends itself — because
    /// iceoryx2 admits exactly one publisher per channel. An empty
    /// `dest_notify_service_name` means the destination never drains a
    /// listener, so the link opens no notifier and carries data only.
    #[pyo3(signature = (
        port_name,
        channel_service_name,
        dest_notify_service_name,
        expected_payload_bytes,
        max_payload_bytes_per_channel,
        channel_service_creation_depth,
        max_subscribers,
        notify_max_notifiers,
        link_id,
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
        channel_service_creation_depth: usize,
        max_subscribers: usize,
        notify_max_notifiers: usize,
        link_id: &str,
    ) -> PyResult<()> {
        let (node, output_writer) = self.helper_process_output_plane()?;

        python
            .detach(|| -> Result<(), Error> {
                if !output_writer.has_channel_publisher(port_name) {
                    let channel = node.open_or_create_service(
                        channel_service_name,
                        max_subscribers,
                        channel_service_creation_depth,
                    )?;
                    let publisher = channel.create_publisher(expected_payload_bytes)?;
                    output_writer.set_channel_publisher(
                        port_name,
                        publisher,
                        ChannelEgressConfig {
                            service_name: channel_service_name.to_string(),
                            trust_tier: ChannelTrustTier::Trusted,
                            expected_payload_bytes,
                            ceiling_bytes: max_payload_bytes_per_channel,
                        },
                    );
                    if let Some(refused_bag_entry) = self
                        .loss_count_board_writer
                        .get()
                        .and_then(|board_writer| board_writer.claim_output_port_entry(port_name))
                    {
                        output_writer.mirror_an_output_ports_refused_bag_count_into(
                            port_name,
                            refused_bag_entry,
                        )?;
                    }
                }
                // An empty name is the engine saying this destination never
                // drains a listener, so there is nothing to wake and the link
                // is wired for data only.
                let notifier = if dest_notify_service_name.is_empty() {
                    None
                } else {
                    let notify_service = node.open_or_create_notify_service(
                        dest_notify_service_name,
                        notify_max_notifiers,
                    )?;
                    Some(notify_service.create_notifier()?)
                };
                output_writer.add_channel_link(port_name, link_id, notifier);
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
    /// `notify_service_name` is always a real name here, unlike the output
    /// side's: a helper-hosted destination drains its own listener whatever
    /// execution mode the class declares, so the engine never tells one to
    /// skip it.
    ///
    /// One call per link. The mailbox and the destination-keyed listener are
    /// installed once — fan-in appends subscribers to the same port, and
    /// iceoryx2 admits exactly one listener per destination.
    #[pyo3(signature = (
        port_name,
        channel_service_name,
        notify_service_name,
        read_mode,
        channel_service_creation_depth,
        input_port_ring_depth,
        max_subscribers,
        notify_max_notifiers,
        link_id,
        audio_window = None,
        loss_count_slot = None,
        wiring_generation = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn wire_input_link(
        &self,
        python: Python<'_>,
        port_name: &str,
        channel_service_name: &str,
        notify_service_name: &str,
        read_mode: &str,
        channel_service_creation_depth: usize,
        input_port_ring_depth: usize,
        max_subscribers: usize,
        notify_max_notifiers: usize,
        link_id: &str,
        audio_window: Option<&Bound<'_, PyAny>>,
        loss_count_slot: Option<usize>,
        wiring_generation: Option<u64>,
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
        let audio_window = audio_window
            .map(|declared| read_the_window_contract_the_parent_wired(port_name, declared))
            .transpose()?;
        // A helper mirroring onto its parent's board must be told where this
        // link's counts go; one with no board has nowhere to mirror them.
        let loss_count_slot_and_generation = match (
            self.loss_count_board_writer.get(),
            loss_count_slot,
            wiring_generation,
        ) {
            (None, _, _) => None,
            (Some(board_writer), Some(loss_count_slot), Some(wiring_generation)) => {
                Some((board_writer, loss_count_slot, wiring_generation))
            }
            (Some(_), _, _) => {
                return Err(PyValueError::new_err(format!(
                    "input link {link_id:?} was wired without a `loss_count_slot` and a \
                     `wiring_generation`, so this helper has no slot on its loss-count board to \
                     mirror the link's losses onto"
                )));
            }
        };

        python
            .detach(|| -> Result<(), Error> {
                if !input_mailboxes.has_port(port_name) {
                    match audio_window {
                        // The window contract sizes the mailbox itself, and the
                        // envelope's ring depth is the windowed ring the
                        // subscriber below takes — the same split the parent
                        // makes for an app-process destination.
                        Some(contract) => {
                            input_mailboxes.add_windowed_port(port_name, read_mode, contract)
                        }
                        None => {
                            input_mailboxes.add_port(port_name, input_port_ring_depth, read_mode)
                        }
                    }
                }
                let channel = node.open_or_create_service(
                    channel_service_name,
                    max_subscribers,
                    channel_service_creation_depth,
                )?;
                input_mailboxes.add_channel_subscriber(
                    port_name,
                    link_id,
                    &InboundLinkName::from(channel_service_name),
                    channel.create_subscriber(input_port_ring_depth)?,
                );
                if let Some((board_writer, loss_count_slot, wiring_generation)) =
                    loss_count_slot_and_generation
                {
                    input_mailboxes.mirror_an_inbound_links_loss_counts_into(
                        link_id,
                        board_writer.claim_inbound_link_slot(loss_count_slot, wiring_generation)?,
                    )?;
                }
                if !input_mailboxes.has_listener() {
                    let notify_service = node
                        .open_or_create_notify_service(notify_service_name, notify_max_notifiers)?;
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

    /// Release this processor's egress for one disconnected link out of
    /// `port_name`, dropping the channel publisher with the port's last link.
    ///
    /// The mirror of [`wire_output_link`]. The engine cannot do this from the
    /// parent — the publisher and notifier are this process's — so it asks, and
    /// a link left behind here is one this child re-opens a second port for on
    /// reconnect.
    ///
    /// [`wire_output_link`]: PythonProcessorLinkDataAccess::wire_output_link
    fn unwire_output_link(
        &self,
        python: Python<'_>,
        port_name: &str,
        link_id: &str,
    ) -> PyResult<()> {
        let (_, output_writer) = self.helper_process_output_plane()?;
        python.detach(|| output_writer.remove_channel_link(port_name, link_id));
        Ok(())
    }

    /// Release this processor's subscriber for one disconnected link, dropping
    /// its input port's mailbox and the shared listener with the last one.
    ///
    /// The mirror of [`wire_input_link`]. Keyed on the link alone: a subscriber
    /// already knows which local input port it was bound to.
    ///
    /// [`wire_input_link`]: PythonProcessorLinkDataAccess::wire_input_link
    fn unwire_input_link(&self, python: Python<'_>, link_id: &str) -> PyResult<()> {
        let (_, input_mailboxes) = self.helper_process_input_plane()?;
        python.detach(|| input_mailboxes.remove_channel_link(link_id));
        Ok(())
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
    ///
    /// `into` is the opt-in strictness dial: without it the bag arrives as a
    /// mapping, and with it the bag is cast or constructed into the type named
    /// — so a mismatch surfaces here, at the consuming read, and nowhere else.
    ///
    /// This is the plumbing a helper process wires by hand; it holds no
    /// context, so a type it constructs is offered no GPU capability. The read
    /// a processor writes is `ctx.inputs.read`, which does.
    #[pyo3(signature = (port_name, *, into = None))]
    fn read_from_input_port<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        into: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.read_from_input_port_offering_gpu_access(python, port_name, into, None)
    }

    /// The next bag on `port_name` with its stamp, or `(None, None)` when the
    /// mailbox is empty.
    pub(crate) fn read_from_input_port_with_timestamp<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
    ) -> PyResult<(Option<Bound<'py, PyAny>>, Option<i64>)> {
        let Some(input_mailboxes) = self.input_mailboxes_reaching(port_name)? else {
            return Ok((None, None));
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
        let Some(input_mailboxes) = self.input_mailboxes_reaching(port_name)? else {
            return Ok(false);
        };
        Ok(python.detach(|| input_mailboxes.has_data(port_name)))
    }

    /// Name the ports the processor class declared, ahead of any wiring.
    ///
    /// A declared port that no link has reached — a processor added to a
    /// running graph before its connects, or one whose last link was taken
    /// away — reads as empty and drops what is written to it; only a name the
    /// class never declared is refused.
    fn declare_ports(&self, input_port_names: Vec<String>, output_port_names: Vec<String>) {
        self.declared_input_ports.lock().extend(input_port_names);
        self.declared_output_ports.lock().extend(output_port_names);
    }

    /// Publish one bag to every downstream link on `port_name`.
    ///
    /// An over-ceiling bag is refused and counted on the port, reaches `graph`
    /// through this helper's loss-count board, and is warned about through the
    /// helper's log — never raised here, the write path's never-die contract.
    #[pyo3(signature = (port_name, bag, timestamp_ns = None))]
    pub(crate) fn write_to_output_port(
        &self,
        python: Python<'_>,
        port_name: &str,
        bag: &Bound<'_, PyAny>,
        timestamp_ns: Option<i64>,
    ) -> PyResult<()> {
        let Some(output_writer) = self.output_writer_reaching(port_name)? else {
            return Ok(());
        };
        let encoded = encode_bag_to_msgpack(bag)?;
        let timestamp_ns = timestamp_ns.unwrap_or_else(|| monotonic_clock_now_ns() as i64);
        match python.detach(|| output_writer.write_raw(port_name, &encoded, timestamp_ns)) {
            Ok(()) => Ok(()),
            Err(Error::PayloadExceedsChannelCeiling {
                channel,
                payload_bytes,
                ceiling_bytes,
                ..
            }) => {
                let refused_bags = output_writer
                    .refused_bag_counts_by_output_port()
                    .refused_bag_count_snapshot_by_output_port()
                    .get(port_name)
                    .copied()
                    .unwrap_or(0);
                warn_through_the_childs_log_module(
                    python,
                    format!(
                        "output port {port_name:?} refused a {payload_bytes}-byte bag over the \
                         {ceiling_bytes}-byte ceiling of channel {channel:?}; the bag was \
                         dropped, and the port has refused {refused_bags} so far"
                    ),
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

    /// Unique per test: every test in this process shares one iceoryx2 domain,
    /// so a fixed name would let one test's channel meet the next one's.
    fn unique_channel_names(label: &str) -> (String, String) {
        let run = std::process::id();
        (
            format!("wiring{run}_{label}/frames_to_downstream"),
            format!("wiring{run}_{label}_dest/notify"),
        )
    }

    fn helper_plane() -> PythonProcessorLinkDataAccess {
        PythonProcessorLinkDataAccess::over_helper_process_iceoryx2_node(
            Iceoryx2Node::for_this_test_process(),
        )
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
            let source = helper_plane();
            let destination = helper_plane();

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
                    8,
                    2,
                    1,
                    "link-1",
                    // No window contract: this port reads the bags it is sent.
                    None,
                    None,
                    None,
                )
                .unwrap();
            source
                .wire_output_link(
                    python,
                    "frames_to_downstream",
                    &channel,
                    &notify,
                    1024,
                    1 << 20,
                    8,
                    2,
                    1,
                    "link-1",
                )
                .unwrap();

            let bag = PyDict::new(python);
            bag.set_item("frame_index", 7i64).unwrap();
            source
                .write_to_output_port(python, "frames_to_downstream", bag.as_any(), None)
                .unwrap();

            let received = destination
                .read_from_input_port(python, "frames_from_upstream", None)
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

    /// A helper that opened its parent's board mirrors an overrun `ordered`
    /// port's losses onto its link's slot and a refusal at the ceiling onto its
    /// output port's entry, with nothing but the reads and writes the processor
    /// already makes — no loop flush and no timer.
    ///
    /// Fail-without-fix: open the board and wire nothing onto it, and the slot
    /// and the entry both still read zero after the losses.
    #[test]
    fn a_helper_that_opened_its_board_mirrors_its_losses_as_its_reads_and_writes_count_them() {
        Python::initialize();
        Python::attach(|python| {
            let (channel, notify) = unique_channel_names("board");
            let board_name = format!("wiring{}_board/loss-counts", std::process::id());
            let output_ports = vec!["frames_to_downstream".to_string()];
            let parent_board = Iceoryx2Node::for_this_test_process()
                .create_helper_process_loss_count_board(&board_name, output_ports.clone())
                .unwrap();
            let source = helper_plane();
            let destination = helper_plane();
            destination
                .open_loss_count_board(python, &board_name, output_ports)
                .expect("the helper opens the board its parent named");

            destination
                .wire_input_link(
                    python,
                    "frames_from_upstream",
                    &channel,
                    &notify,
                    "read_next_in_order",
                    8,
                    8,
                    2,
                    1,
                    "link-1",
                    None,
                    Some(4),
                    Some(21),
                )
                .unwrap();
            destination
                .wire_output_link(
                    python,
                    "frames_to_downstream",
                    &format!("{channel}_onward"),
                    "",
                    64,
                    1024,
                    8,
                    2,
                    1,
                    "link-onward",
                )
                .unwrap();
            source
                .wire_output_link(
                    python,
                    "frames_to_downstream",
                    &channel,
                    &notify,
                    1024,
                    1 << 20,
                    8,
                    2,
                    1,
                    "link-1",
                )
                .unwrap();

            let write_frame = |frame_index: i64| {
                let bag = PyDict::new(python);
                bag.set_item("frame_index", frame_index).unwrap();
                source
                    .write_to_output_port(python, "frames_to_downstream", bag.as_any(), None)
                    .unwrap();
            };
            let read_frame_index = || {
                destination
                    .read_from_input_port(python, "frames_from_upstream", None)
                    .unwrap()
                    .expect("the port holds a bag")
                    .get_item("frame_index")
                    .unwrap()
                    .extract::<i64>()
                    .unwrap()
            };
            let slot_counts = || {
                parent_board.inbound_link_slot(4).map(|slot| {
                    (
                        slot.wiring_generation,
                        slot.dropped_bags,
                        slot.discarded_samples,
                    )
                })
            };

            write_frame(0);
            assert_eq!(read_frame_index(), 0);
            assert_eq!(slot_counts(), Some((21, 0, 0)));
            for frame_index in 1..=12 {
                write_frame(frame_index);
            }
            assert_eq!(read_frame_index(), 5);
            assert_eq!(
                slot_counts(),
                Some((21, 4, 0)),
                "the four bags the eight-deep ring overwrote land on the link's slot at the read"
            );

            let bag_over_the_ceiling = PyDict::new(python);
            bag_over_the_ceiling
                .set_item("padding", pyo3::types::PyBytes::new(python, &[0u8; 4096]))
                .unwrap();
            destination
                .write_to_output_port(
                    python,
                    "frames_to_downstream",
                    bag_over_the_ceiling.as_any(),
                    None,
                )
                .expect("a refusal at the ceiling is never raised");
            assert_eq!(
                parent_board.output_port_refused_bags("frames_to_downstream"),
                Some(1)
            );
        });
    }

    /// The window contract crosses the parent→child envelope and the child's
    /// own stage honours it: a 48 kHz stereo source published on the wired
    /// output reaches the wired input as exactly-512-sample 16 kHz mono
    /// windows, in this process, through the same engine code the parent's
    /// mailboxes run.
    ///
    /// The wheel crate's own test target is the only place this is provable
    /// without a device: everything above it is either a Python-level refusal
    /// or a rig-gated graph.
    #[test]
    fn a_windowed_input_link_hands_the_child_windows_rather_than_the_bags_it_was_sent() {
        Python::initialize();
        Python::attach(|python| {
            let (channel, notify) = unique_channel_names("windowed");
            let source = helper_plane();
            let destination = helper_plane();

            let contract = PyDict::new(python);
            contract.set_item("sample_rate", 16_000i64).unwrap();
            contract.set_item("channels", 1i64).unwrap();
            contract.set_item("dtype", "f32").unwrap();
            contract.set_item("window_size", 512i64).unwrap();
            contract.set_item("hop", 512i64).unwrap();

            destination
                .wire_input_link(
                    python,
                    "audio_from_upstream",
                    &channel,
                    &notify,
                    "read_next_in_order",
                    8,
                    8,
                    2,
                    1,
                    "link-1",
                    Some(contract.as_any()),
                    None,
                    None,
                )
                .unwrap();
            source
                .wire_output_link(
                    python,
                    "audio",
                    &channel,
                    &notify,
                    16_384,
                    1 << 20,
                    8,
                    2,
                    1,
                    "link-1",
                )
                .unwrap();

            const SOURCE_FRAMES_PER_BLOCK: usize = 512;
            const SOURCE_RATE: i64 = 48_000;
            for block in 0..8i64 {
                let payload: Vec<u8> = (0..SOURCE_FRAMES_PER_BLOCK * 2)
                    .flat_map(|scalar| (scalar as f32 / 1024.0).to_le_bytes())
                    .collect();
                let bag = PyDict::new(python);
                bag.set_item("samples", pyo3::types::PyBytes::new(python, &payload))
                    .unwrap();
                bag.set_item("sample_rate", SOURCE_RATE).unwrap();
                bag.set_item("channels", 2i64).unwrap();
                bag.set_item("sample_count", SOURCE_FRAMES_PER_BLOCK as i64)
                    .unwrap();
                bag.set_item("dtype", "f32").unwrap();
                bag.set_item(
                    "first_sample_timestamp_ns",
                    block * SOURCE_FRAMES_PER_BLOCK as i64 * 1_000_000_000 / SOURCE_RATE,
                )
                .unwrap();
                source
                    .write_to_output_port(python, "audio", bag.as_any(), None)
                    .unwrap();
            }

            let window = destination
                .read_from_input_port(python, "audio_from_upstream", None)
                .unwrap()
                .expect("the windowed input received nothing");
            assert_eq!(
                window
                    .get_item("sample_count")
                    .unwrap()
                    .extract::<i64>()
                    .unwrap(),
                512,
                "the child's stage owes exactly what the contract declared"
            );
            assert_eq!(
                window
                    .get_item("sample_rate")
                    .unwrap()
                    .extract::<i64>()
                    .unwrap(),
                16_000
            );
            assert_eq!(
                window
                    .get_item("channels")
                    .unwrap()
                    .extract::<i64>()
                    .unwrap(),
                1
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
            let source = helper_plane();

            source
                .wire_output_link(
                    python,
                    "frames_to_downstream",
                    &channel,
                    &notify,
                    1024,
                    1 << 20,
                    8,
                    2,
                    1,
                    "link-1",
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
                    "link-2",
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
                    python,
                    "frames_to_downstream",
                    &channel,
                    &notify,
                    1024,
                    1 << 20,
                    8,
                    2,
                    1,
                    "link-1",
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
            let destination = helper_plane();
            let refusal = destination
                .wire_input_link(
                    python,
                    "frames_from_upstream",
                    &channel,
                    &notify,
                    "whenever",
                    8,
                    8,
                    2,
                    1,
                    "link-1",
                    None,
                    None,
                    None,
                )
                .unwrap_err();
            assert!(
                refusal.to_string().contains("whenever"),
                "the refusal must name what it got: {refusal}"
            );
        });
    }
}

/// Read the window contract the parent wired this input port with.
///
/// The parent sends the values resolved — a `match_device` sentinel resolves in
/// the process that opened the device stream — so a child reads one shape and
/// never a sentinel it could not settle. Field by field rather than through a
/// serde bridge, so a key the parent got wrong is named here rather than
/// surfacing as an anonymous decode failure.
fn read_the_window_contract_the_parent_wired(
    port_name: &str,
    declared: &Bound<'_, PyAny>,
) -> PyResult<ResolvedAudioWindowContract> {
    fn field<'py, T: for<'a> FromPyObject<'a, 'py, Error = PyErr>>(
        port_name: &str,
        declared: &Bound<'py, PyAny>,
        key: &str,
    ) -> PyResult<T> {
        declared
            .get_item(key)
            .and_then(|value| value.extract())
            .map_err(|read_failure| {
                PyValueError::new_err(format!(
                    "input port {port_name:?} was wired with an `audio_window` whose \
                     {key:?} the helper could not read: {read_failure}"
                ))
            })
    }

    let values = AudioWindowContractDeclaredValues {
        sample_rate: field(port_name, declared, "sample_rate")?,
        channels: channel_count_the_parent_wired(port_name, declared)?,
        dtype: field(port_name, declared, "dtype")?,
        window_size: field(port_name, declared, "window_size")?,
        hop: field(port_name, declared, "hop")?,
    };
    ResolvedAudioWindowContract::from_declared_values(&values).map_err(|refusal| {
        PyValueError::new_err(format!(
            "input port {port_name:?} was wired with an `audio_window` the stage cannot \
             honour: {refusal}"
        ))
    })
}

/// Read the channel count off the contract the parent wired, or `None` where
/// the port follows whatever count its source sends.
///
/// The one field of the envelope that may be absent, and the one that may
/// arrive as a word rather than a number.
fn channel_count_the_parent_wired(
    port_name: &str,
    declared: &Bound<'_, PyAny>,
) -> PyResult<Option<u32>> {
    let value = match declared.get_item("channels") {
        Ok(value) => value,
        // Only a missing key means "follows its source". Anything else is an
        // envelope the helper could not read, and swallowing it here would
        // turn a wiring bug into a silently different contract.
        Err(lookup_failure) if lookup_failure.is_instance_of::<PyKeyError>(declared.py()) => {
            return Ok(None);
        }
        Err(lookup_failure) => {
            return Err(PyValueError::new_err(format!(
                "input port {port_name:?} was wired with an `audio_window` the helper could \
                 not read `channels` out of: {lookup_failure}"
            )));
        }
    };
    read_a_channel_count_or_the_source_spelling(&value).map_err(|refusal| {
        refusal.framed_as(format!(
            "input port {port_name:?} was wired with an `audio_window` whose \"channels\" \
             the helper could not read: it"
        ))
    })
}

fn not_a_helper_process_data_plane_error() -> PyErr {
    PyRuntimeError::new_err(
        "this processor's links belong to the engine that wired them, so they cannot be wired \
         again from Python — only a helper process wires its own",
    )
}

fn unwired_port_error(direction: &str, port_name: &str) -> PyErr {
    PyRuntimeError::new_err(format!(
        "{direction} port {port_name:?} is not one this processor declared, so no link \
         can reach it"
    ))
}
