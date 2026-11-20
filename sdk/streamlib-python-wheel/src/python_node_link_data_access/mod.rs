// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The data plane a Python node reads and writes through.
//!
//! A read hands out a borrow of the sample a payload arrived in and a write
//! copies any contiguous buffer, or loans its sample to fill; nothing here
//! encodes or decodes it. This is where the GIL-release contract is kept for
//! the per-message path: the iceoryx2 call that can block runs detached, since
//! holding the GIL across it would stall the interpreter's other threads for
//! its duration.
//!
//! A helper process wires this object itself from the port wiring the parent
//! sends it, then reads and writes through the same methods the parent's
//! wiring path installs into. Every iceoryx2 port here is `!Send` under the
//! engine's discipline, so a child must create and drive all of them from one
//! thread.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use pyo3::exceptions::{PyKeyError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;

use crate::python_helper_process_pixel_exchange::HelperProcessGpuExchangeClient;
use streamlib::sdk::descriptors::AudioWindowContractDeclaredValues;
use streamlib::sdk::error::Error;
use streamlib::sdk::graph::cast_exposed_name_to_url_safe;
use streamlib::sdk::iceoryx2::{
    BundleMembersAtTheRead, ChannelEgressConfig, ChannelTrustTier, DataChannelPayloadDescriptor,
    HelperProcessLossCountBoardWriter, ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE, Iceoryx2Node,
    InboundLinkLossCountBoardSlotAndWiringGeneration, InboundLinkName, InputMailboxesInner,
    MessageGroupBoundary, OutputWriterInner, ReadMode, ResolvedAudioWindowContract,
    StreamClockBoardPublishListener, input_port_borrow_quota as resolve_input_port_borrow_quota,
};
use streamlib::sdk::node_interpreter::NODE_INTERPRETER_NODE_RUN_ID_ENVIRONMENT_VARIABLE;

use crate::python_node_declaration::read_a_channel_count_or_the_source_spelling;
use crate::python_port_payload::PythonPortPayloadLoan;

mod helper_wiring_and_port_lending;
mod input_port_reads;
mod output_port_writes_and_loans;

pub(crate) use input_port_reads::{
    PortFamiliesOfTheRead, PortFamilyOfTheRead, PortPayloadReadShape, image_frame_of_the_read,
};
pub(crate) use output_port_writes_and_loans::{
    ArrayOutputPortDeclaredResidency, ChannelsOutputPortDeclaredShape,
    image_frame_descriptor_of_the_image,
};

/// One node's links, as seen from Python.
///
/// Frozen because the engine hands the same object to the node's own
/// thread and reads it from the wiring path; the interior `OnceLock`s are
/// written once before the node's first callback.
#[pyclass(name = "NodeLinkDataAccess", module = "tatolab.stream", frozen)]
pub(crate) struct PythonNodeLinkDataAccess {
    input_mailboxes: OnceLock<Arc<InputMailboxesInner>>,
    output_writer: OnceLock<Arc<OutputWriterInner>>,
    /// The child's own iceoryx2 node, present only when this object wired
    /// itself. The parent's copy is wired by the compiler op and leaves this
    /// empty — the node it would name belongs to the engine.
    iceoryx2_node: OnceLock<Iceoryx2Node>,
    /// The board this helper's loss counts are mirrored onto for its parent,
    /// once its setup opened the one the parent named.
    loss_count_board_writer: OnceLock<Arc<HelperProcessLossCountBoardWriter>>,
    /// The state each stateful function node this helper calls resumes from,
    /// by breadcrumb number, as the setup command's board carries it; a node
    /// it names no state for starts from its state record's defaults.
    function_node_states_resumed_from: OnceLock<HashMap<u32, Vec<f64>>>,
    /// The wake on each publish of this helper's stream clock, once its setup
    /// found the stream stepped.
    stream_clock_board_publish_listener: OnceLock<StreamClockBoardPublishListener>,
    declared_input_ports: parking_lot::Mutex<HashSet<String>>,
    declared_output_ports: parking_lot::Mutex<HashSet<String>>,
    /// The ports Python lent to the node's native code through its module host
    /// table; Python's own reads and writes of them are refused by name.
    input_ports_lent_to_native_code: parking_lot::Mutex<HashSet<String>>,
    output_ports_lent_to_native_code: parking_lot::Mutex<HashSet<String>>,
    /// Whether the runtime drives `process()` from this node's inputs, which
    /// would race native code waiting on a lent input for the same wakeups.
    runtime_drives_process_from_inputs: std::sync::atomic::AtomicBool,
    values_last_read_off_links_into_its_function_node_islands:
        crate::python_function_node_island::ValuesLastReadOffLinksFromOutsideIslands,
    function_node_raises_in_this_helper:
        Arc<crate::python_function_node_island::FunctionNodeRaisesInThisHelper>,
    /// The client this helper claims pooled GPU frames through, once its
    /// runtime context opened one; the islands it feeds claim through it the
    /// frames the messages they hold name.
    gpu_exchange_client_of_this_helper: Arc<OnceLock<Arc<HelperProcessGpuExchangeClient>>>,
}

/// The data plane a module host table reaches a helper's lent ports through.
pub(crate) struct HelperProcessDataPlaneLentToNativeCode {
    pub(crate) input_mailboxes: Arc<InputMailboxesInner>,
    pub(crate) output_writer: Arc<OutputWriterInner>,
    /// Lent port names, each cast the way `@node.process` casts it.
    pub(crate) lent_input_port_names: Vec<String>,
    pub(crate) lent_output_port_names: Vec<String>,
}

/// Build a helper process's own data plane, with its own iceoryx2 node.
///
/// The parent's copy of this object is built in Rust and wired by the
/// compiler op; this is how a child builds the one it wires itself from the
/// port wiring the parent sent it. The node opens in the domain root the
/// parent handed over, and a process handed none is refused.
#[pyfunction]
pub(crate) fn open_node_link_data_access_for_helper_process(
    python: Python<'_>,
) -> PyResult<PythonNodeLinkDataAccess> {
    let iceoryx2_domain_root = std::env::var_os(ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE)
        .filter(|root| !root.is_empty())
        .ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "{ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE} is not set: a helper process \
                 opens its iceoryx2 node only in the domain its parent runtime hands it"
            ))
        })?;
    let node_name = std::env::var(NODE_INTERPRETER_NODE_RUN_ID_ENVIRONMENT_VARIABLE).map_or_else(
        |_| format!("streamlib-helper/pid{}", std::process::id()),
        |node_run_id| format!("streamlib-helper/{node_run_id}"),
    );
    let node = python
        .detach(|| Iceoryx2Node::new(std::path::Path::new(&iceoryx2_domain_root), &node_name))
        .map_err(|node_failure| PyRuntimeError::new_err(node_failure.to_string()))?;
    Ok(PythonNodeLinkDataAccess::over_helper_process_iceoryx2_node(
        node,
    ))
}

#[pymethods]
impl PythonNodeLinkDataAccess {
    /// Read the clock of this helper's stream off the board its parent named,
    /// so `ctx.time` and every write with no stamp follow the stream's clock.
    fn adopt_the_stream_clock_board(&self, python: Python<'_>, service_name: &str) -> PyResult<()> {
        let Some(node) = self.iceoryx2_node.get() else {
            return Err(not_a_helper_process_data_plane_error());
        };
        let board_reader = python
            .detach(|| node.open_stream_clock_board_reader(service_name))
            .map_err(|open_failure| PyRuntimeError::new_err(open_failure.to_string()))?;
        crate::python_helper_stream_clock::adopt_the_stream_clock_board(board_reader)
            .map_err(PyRuntimeError::new_err)
    }

    /// Wake on each publish of this helper's stream clock from here on: how a
    /// node in a stepped stream learns a step opened.
    fn listen_for_each_stream_clock_board_publish(
        &self,
        python: Python<'_>,
        service_name: &str,
    ) -> PyResult<()> {
        let Some(node) = self.iceoryx2_node.get() else {
            return Err(not_a_helper_process_data_plane_error());
        };
        let publish_listener = python
            .detach(|| node.listen_for_each_stream_clock_board_publish(service_name))
            .map_err(|open_failure| PyRuntimeError::new_err(open_failure.to_string()))?;
        self.stream_clock_board_publish_listener
            .set(publish_listener)
            .map_err(|_| {
                PyRuntimeError::new_err(format!(
                    "this helper already listens for its stream clock's publishes, so it cannot \
                     listen on {service_name:?} as well"
                ))
            })
    }

    /// The fd that turns readable once the stream clock is published; `None`
    /// before this helper listens for it.
    ///
    /// Owned by the listener, which lives as long as this object: the caller
    /// must not close it.
    fn stream_clock_board_publish_listener_fd(&self) -> Option<i32> {
        self.stream_clock_board_publish_listener
            .get()
            .map(StreamClockBoardPublishListener::file_descriptor)
    }

    /// Take every stream clock publish noted so far, so its fd waits for the
    /// next.
    fn take_every_stream_clock_board_publish_noted(&self) {
        if let Some(publish_listener) = self.stream_clock_board_publish_listener.get() {
            publish_listener.take_every_publish_noted();
        }
    }

    /// Note on the loss-count board that a callback of this node started; the
    /// stream's step cannot close until it returns.
    fn note_a_step_callback_started(&self) {
        if let Some(board_writer) = self.loss_count_board_writer.get() {
            board_writer.note_a_callback_started();
        }
    }

    /// Note on the loss-count board that a callback of this node returned,
    /// finishing step `finished_step_index` when one is named.
    #[pyo3(signature = (finished_step_index = None))]
    fn note_a_step_callback_returned(&self, finished_step_index: Option<u64>) {
        if let Some(board_writer) = self.loss_count_board_writer.get() {
            board_writer.note_a_callback_returned(finished_step_index);
        }
    }

    /// Open the loss-count board the parent created for this spawn, so every
    /// link wired from here on mirrors its counts onto it and every stateful
    /// function node writes its state on it.
    ///
    /// `output_port_names` is the board's output-port section in key order, as
    /// the setup command lists it; `function_node_states` names each stateful
    /// function node's entry by breadcrumb number, beside the state it resumes
    /// from as f64 bit patterns, `None` for its state record's defaults;
    /// `function_node_frame_interval_ns` is the interval each function-node call
    /// is timed against.
    #[pyo3(signature = (
        service_name,
        output_port_names,
        function_node_states = Vec::new(),
        function_node_frame_interval_ns = None,
    ))]
    fn open_loss_count_board(
        &self,
        python: Python<'_>,
        service_name: &str,
        output_port_names: Vec<String>,
        function_node_states: Vec<(u32, Option<Vec<u64>>)>,
        function_node_frame_interval_ns: Option<u64>,
    ) -> PyResult<()> {
        let Some(node) = self.iceoryx2_node.get() else {
            return Err(not_a_helper_process_data_plane_error());
        };
        let stateful_function_node_breadcrumb_numbers: Vec<u32> = function_node_states
            .iter()
            .map(|(breadcrumb_number, _)| *breadcrumb_number)
            .collect();
        let board_writer = python
            .detach(|| {
                node.open_helper_process_loss_count_board_writer(
                    service_name,
                    &output_port_names,
                    &stateful_function_node_breadcrumb_numbers,
                )
            })
            .map_err(|open_failure| PyRuntimeError::new_err(open_failure.to_string()))?;
        if let Some(frame_interval_ns) = function_node_frame_interval_ns {
            board_writer.time_each_function_node_call_against_the_frame_interval(
                std::time::Duration::from_nanos(frame_interval_ns),
            );
        }
        self.loss_count_board_writer
            .set(Arc::new(board_writer))
            .map_err(|_| {
                PyRuntimeError::new_err(format!(
                    "this helper already mirrors its loss counts onto a board, so the board \
                     {service_name:?} cannot be opened as a second"
                ))
            })?;
        let function_node_states_resumed_from = function_node_states
            .into_iter()
            .filter_map(|(breadcrumb_number, resumes_from_f64_bits)| {
                Some((
                    breadcrumb_number,
                    resumes_from_f64_bits?
                        .into_iter()
                        .map(f64::from_bits)
                        .collect(),
                ))
            })
            .collect();
        // Set once, beside the board the `set` above already admitted once.
        let _ = self
            .function_node_states_resumed_from
            .set(function_node_states_resumed_from);
        Ok(())
    }

    /// Open this node's publisher and one destination notifier for a link
    /// out of `port_name`.
    ///
    /// One call per link. The publisher is installed once — the first link out
    /// of a port creates it and every later link only appends itself — because
    /// iceoryx2 admits exactly one publisher per channel.
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
        output_port_wiring_generation = None,
    ))]
    #[allow(
        clippy::too_many_arguments,
        reason = "pyo3 maps each Python keyword argument to its own parameter"
    )]
    pub(crate) fn wire_output_link(
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
        output_port_wiring_generation: Option<u64>,
    ) -> PyResult<()> {
        let (node, output_writer) = self.helper_process_output_plane()?;
        // A helper mirroring onto its parent's board must be told which of the
        // port's channels this is; one with no board has nowhere to mirror.
        let board_writer_and_output_port_wiring_generation = self
            .loss_count_board_writer_with_where_a_link_mirrors_to(
                "output",
                link_id,
                output_port_wiring_generation,
                "an `output_port_wiring_generation`",
            )?;

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
                        &channel,
                        publisher,
                        ChannelEgressConfig {
                            service_name: channel_service_name.to_string(),
                            // A helper's egress crosses the subprocess boundary
                            // by construction, which is the tier the parent
                            // classified this channel under when it chose the
                            // ceiling riding the envelope beside it.
                            trust_tier: ChannelTrustTier::UntrustedSession,
                            expected_payload_bytes,
                            chunk_ceiling_bytes: max_payload_bytes_per_channel,
                        },
                    );
                    if let Some(bag_count_board_entry_mirror) =
                        board_writer_and_output_port_wiring_generation.and_then(
                            |(board_writer, output_port_wiring_generation)| {
                                board_writer.claim_output_port_entry(
                                    port_name,
                                    output_port_wiring_generation,
                                )
                            },
                        )
                    {
                        output_writer.mirror_an_output_ports_bag_counts_into(
                            port_name,
                            bag_count_board_entry_mirror,
                        )?;
                    }
                }
                let notify_service = node.open_or_create_notify_service(
                    dest_notify_service_name,
                    notify_max_notifiers,
                )?;
                output_writer.add_channel_link(
                    port_name,
                    link_id,
                    notify_service.create_notifier()?,
                );
                Ok(())
            })
            .map_err(|wiring_failure| {
                PyRuntimeError::new_err(format!(
                    "could not wire output port {port_name:?} onto channel \
                 {channel_service_name:?}: {wiring_failure}"
                ))
            })
    }

    /// Open this node's subscriber for a link into `port_name`, plus the
    /// one listener every input shares.
    ///
    /// `channel_service_name` is what this end subscribes to and
    /// `inbound_link_name` is what the link is known by in a read.
    ///
    /// A helper-hosted destination opens its listener whatever execution mode
    /// the class declares.
    ///
    /// One call per link. The mailbox and the destination-keyed listener are
    /// installed once — fan-in appends subscribers to the same port, and
    /// iceoryx2 admits exactly one listener per destination.
    #[pyo3(signature = (
        port_name,
        channel_service_name,
        inbound_link_name,
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
        input_port_borrow_quota = None,
        member_taken_at_the_read = None,
        member_put_at_the_read = None,
    ))]
    #[allow(
        clippy::too_many_arguments,
        reason = "pyo3 maps each Python keyword argument to its own parameter"
    )]
    pub(crate) fn wire_input_link(
        &self,
        python: Python<'_>,
        port_name: &str,
        channel_service_name: &str,
        inbound_link_name: &str,
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
        input_port_borrow_quota: Option<usize>,
        member_taken_at_the_read: Option<&Bound<'_, PyAny>>,
        member_put_at_the_read: Option<&Bound<'_, PyAny>>,
    ) -> PyResult<()> {
        let (node, input_mailboxes) = self.helper_process_input_plane()?;
        let bundle_members_at_the_read = BundleMembersAtTheRead {
            taken_from_its_output: member_taken_at_the_read
                .map(|wired| {
                    read_the_member_the_parent_wired(
                        python,
                        port_name,
                        "member_taken_at_the_read",
                        wired,
                    )
                })
                .transpose()?,
            put_into_its_input: member_put_at_the_read
                .map(|wired| {
                    read_the_member_the_parent_wired(
                        python,
                        port_name,
                        "member_put_at_the_read",
                        wired,
                    )
                })
                .transpose()?,
        };
        let input_port_borrow_quota =
            resolve_input_port_borrow_quota(port_name, input_port_borrow_quota)
                .map_err(|refusal| PyValueError::new_err(refusal.to_string()))?;
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
        let loss_count_slot_and_generation = self
            .loss_count_board_writer_with_where_a_link_mirrors_to(
                "input",
                link_id,
                loss_count_slot.zip(wiring_generation),
                "a `loss_count_slot` and a `wiring_generation`",
            )?;

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
                input_mailboxes.set_input_port_borrow_quota(port_name, input_port_borrow_quota)?;
                let channel = node.open_or_create_service(
                    channel_service_name,
                    max_subscribers,
                    channel_service_creation_depth,
                )?;
                input_mailboxes.add_channel_subscriber(
                    port_name,
                    link_id,
                    &InboundLinkName::from(inbound_link_name),
                    channel.create_subscriber(input_port_ring_depth)?,
                    bundle_members_at_the_read,
                );
                if let Some((board_writer, (loss_count_slot, wiring_generation))) =
                    loss_count_slot_and_generation
                {
                    input_mailboxes.mirror_an_inbound_links_loss_counts_into(
                        link_id,
                        board_writer.claim_inbound_link_slot(
                            InboundLinkLossCountBoardSlotAndWiringGeneration {
                                slot: loss_count_slot,
                                wiring_generation,
                            },
                        )?,
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

    fn unwire_input_link(&self, python: Python<'_>, link_id: &str) -> PyResult<()> {
        let (_, input_mailboxes) = self.helper_process_input_plane()?;
        python.detach(|| input_mailboxes.remove_channel_link(link_id));
        Ok(())
    }

    /// The fd that becomes readable when any upstream publishes.
    ///
    /// Owned by the listener: the caller must not close it, and must stop
    /// polling it before this object is dropped.
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

    /// Whether any input port has a message waiting.
    ///
    /// One wake does not mean one frame — iceoryx2 coalesces — so a reactive
    /// loop asks this rather than assuming.
    fn any_input_port_has_data(&self, python: Python<'_>) -> bool {
        match self.input_mailboxes.get() {
            Some(input_mailboxes) => python.detach(|| input_mailboxes.any_port_has_data()),
            None => false,
        }
    }

    #[pyo3(signature = (port_name, *, into = None))]
    fn read_from_input_port<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        into: Option<&Bound<'py, PyAny>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.read_from_input_port_offering_gpu_access(
            python,
            port_name,
            PortPayloadReadShape::by_name_cast_into(into),
            None,
        )
    }

    /// The next message's payload on `port_name` with its stamp, or
    /// `(None, None)` when the mailbox is empty.
    pub(crate) fn read_from_input_port_with_timestamp<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
    ) -> PyResult<(Option<Bound<'py, PyAny>>, Option<i64>)> {
        self.read_from_input_port_with_timestamp_offering_gpu_access(
            python,
            port_name,
            PortPayloadReadShape::RawBorrow,
            None,
        )
    }

    /// Whether a message is waiting on `port_name`, without consuming it.
    pub(crate) fn input_port_has_data(
        &self,
        python: Python<'_>,
        port_name: &str,
    ) -> PyResult<bool> {
        let port_name = &*declared_port_name_the_spelling_names(port_name)?;
        let Some(input_mailboxes) = self.input_mailboxes_reaching(port_name)? else {
            return Ok(false);
        };
        Ok(python.detach(|| input_mailboxes.has_data(port_name)))
    }

    pub(crate) fn declare_ports(
        &self,
        input_port_names: Vec<String>,
        output_port_names: Vec<String>,
    ) {
        self.declared_input_ports.lock().extend(input_port_names);
        if let Some(output_writer) = self.output_writer.get() {
            output_writer.declare_output_ports(output_port_names.iter().cloned());
        }
        self.declared_output_ports.lock().extend(output_port_names);
    }

    /// Have each of `output_port_names` keep the last message written to it, linked or not,
    /// and send it to a reader that connects after it, as the parent's setup command names
    /// them. A data plane with no writer of its own keeps nothing.
    pub(crate) fn keep_the_last_message_of_output_ports(&self, output_port_names: Vec<String>) {
        if let Some(output_writer) = self.output_writer.get() {
            output_writer.keep_the_last_message_of_output_ports(output_port_names);
        }
    }

    /// Publish one message carrying `payload` to every downstream link on
    /// `port_name`.
    ///
    /// `payload` is any contiguous buffer, copied; anything else is refused
    /// by name.
    /// Omitting `timestamp_ns` stamps stream time now; `starts_group` lands
    /// in the frame header's group flag. An over-ceiling message is refused
    /// and counted on the port, reaches `graph` through this helper's
    /// loss-count board, and is warned about through the helper's log — never
    /// raised here, the write path's never-die contract.
    #[pyo3(signature = (port_name, payload, timestamp_ns = None, starts_group = true))]
    pub(crate) fn write_to_output_port(
        &self,
        python: Python<'_>,
        port_name: &str,
        payload: &Bound<'_, PyAny>,
        timestamp_ns: Option<i64>,
        starts_group: bool,
    ) -> PyResult<()> {
        self.write_described_by(
            python,
            port_name,
            payload,
            timestamp_ns,
            starts_group,
            |_| Ok(DataChannelPayloadDescriptor::bytes()),
        )
    }

    /// Loan `payload_byte_len` bytes on `port_name` for its writer to fill in
    /// place, published when the loan's `with` block exits cleanly.
    ///
    /// Refused while the port's previous loan is open, so a port's order is
    /// its write order. A declared port no link has reached, or a payload past
    /// the port's ceiling, loans scratch bytes that publish nowhere.
    #[pyo3(signature = (port_name, payload_byte_len, timestamp_ns = None, starts_group = true))]
    pub(crate) fn loan_on_output_port(
        &self,
        python: Python<'_>,
        port_name: &str,
        payload_byte_len: usize,
        timestamp_ns: Option<i64>,
        starts_group: bool,
    ) -> PyResult<PythonPortPayloadLoan> {
        let port_name = &*declared_port_name_the_spelling_names(port_name)?;
        let output_writer = self.output_writer_reaching(port_name)?;
        let group_boundary = if starts_group {
            MessageGroupBoundary::StartsGroup
        } else {
            MessageGroupBoundary::ContinuesGroup
        };
        PythonPortPayloadLoan::loaning(
            python,
            output_writer,
            port_name,
            payload_byte_len,
            timestamp_ns,
            group_boundary,
        )
    }
}

/// Read the window contract the parent wired this input port with.
///
/// The parent sends the values resolved — a `match_device` sentinel resolves in
/// the process that opened the device stream — so a child reads one shape and
/// never a sentinel it could not settle. Field by field rather than through a
/// serde bridge, so a key the parent got wrong is named here rather than
/// surfacing as an anonymous decode failure.
/// The member of a bundle the parent wired a link into `port_name` to take from each message or
/// put each value into, under the envelope's key `key`.
fn read_the_member_the_parent_wired<Member: serde::de::DeserializeOwned>(
    python: Python<'_>,
    port_name: &str,
    key: &str,
    wired: &Bound<'_, PyAny>,
) -> PyResult<Member> {
    let rendered: String = python
        .import("json")?
        .call_method1("dumps", (wired,))?
        .extract()?;
    serde_json::from_str(&rendered).map_err(|read_failure| {
        PyValueError::new_err(format!(
            "input port {port_name:?} was wired with a `{key}` the helper could not read: \
             {read_failure}"
        ))
    })
}

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
    read_a_channel_count_or_the_source_spelling(
        &value,
        format!(
            "input port {port_name:?} was wired with an `audio_window` whose \"channels\" \
             the helper could not read: it"
        ),
    )
}

fn not_a_helper_process_data_plane_error() -> PyErr {
    PyRuntimeError::new_err(
        "this node's links belong to the engine that wired them, so they cannot be wired \
         again from Python — only a helper process wires its own",
    )
}

/// The declared port an author's spelling names: `@node.process` cast each declared
/// name, so a lookup casts its argument and `read("Video")` finds `video`.
fn declared_port_name_the_spelling_names(port_name: &str) -> PyResult<Cow<'_, str>> {
    cast_exposed_name_to_url_safe(port_name)
        .map_err(|names_no_port| PyValueError::new_err(names_no_port.to_string()))
}

fn unwired_port_error(direction: &str, port_name: &str) -> PyErr {
    PyRuntimeError::new_err(format!(
        "{direction} port {port_name:?} is not one this node declared, so no link \
         can reach it"
    ))
}

#[cfg(test)]
mod tests;
