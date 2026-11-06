// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

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
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use pyo3::exceptions::{PyKeyError, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBytes, PyDict, PyMapping, PyTuple, PyType};
use streamlib::sdk::color::h273_color_vui::{
    H273_UNSPECIFIED, H273ColorVui, color_info_axis_names_of_h273_color_vui,
    h273_color_vui_from_color_info_axis_names,
};
use streamlib::sdk::descriptors::AudioWindowContractDeclaredValues;
use streamlib::sdk::error::Error;
use streamlib::sdk::graph::cast_exposed_name_to_url_safe;
use streamlib::sdk::iceoryx2::{
    ARRAY_FRAME_ATTRIBUTE_ROW_BYTES, ArrayFrameAttributeResidency, ArrayFrameAttributeRow,
    ArrayFrameDescriptorBody, ChannelEgressConfig, ChannelTrustTier, ChannelsBlockDescriptorBody,
    ChannelsBlockSampleType, DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind,
    GpuPoolSlotFrameDescriptorBody, HelperProcessLossCountBoardWriter,
    ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE, Iceoryx2Node, ImageFrameDescriptorBody,
    ImageFrameH273Colour, InboundLinkLossCountBoardSlotAndWiringGeneration, InboundLinkName,
    InputMailboxesInner, InputPortMessagePayload, MessageGroupBoundary, MessageReadFromInputPort,
    OutputWriterInner, PacketsUnitDescriptorBody, ReadMode, ResolvedAudioWindowContract,
    StreamClockBoardPublishListener, array_frame_attribute_table_of, array_frame_cpu_data_offsets,
    input_port_borrow_quota as resolve_input_port_borrow_quota,
};
use streamlib::sdk::processor_interpreter::PROCESSOR_INTERPRETER_PROCESSOR_ID_ENVIRONMENT_VARIABLE;
use streamlib::sdk::rhi::{TensorElementType, TensorStorageBufferLayout, TextureFormat};

use crate::python_bag_conversion::{
    cast_payload_into_read_target, family_type_built_offering_gpu_access,
    python_type_name_for_error_message, records_message_dict_of,
};
use crate::python_port_family_read_types::{
    PythonArrayFrameCpuAttribute, PythonChannelsBlock, PythonPacketsUnit,
};
use crate::python_port_payload::{PythonPortPayloadBorrow, PythonPortPayloadLoan};
use crate::python_processor_context::{
    PooledGpuFrameClaimHeldByARead, PythonGpuContextLimitedAccess,
};
use crate::python_processor_declaration::read_a_channel_count_or_the_source_spelling;

/// One node's links, as seen from Python.
///
/// Frozen because the engine hands the same object to the node's own
/// thread and reads it from the wiring path; the interior `OnceLock`s are
/// written once before the node's first callback.
#[pyclass(name = "NodeLinkDataAccess", module = "tatolab.stream", frozen)]
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
    /// The wake on each publish of this helper's stream clock, once its setup
    /// found the stream stepped.
    stream_clock_board_publish_listener: OnceLock<StreamClockBoardPublishListener>,
    /// The ports the processor class declared. A declared port no link has
    /// reached reads as empty and swallows writes; an undeclared name is
    /// refused.
    declared_input_ports: parking_lot::Mutex<HashSet<String>>,
    declared_output_ports: parking_lot::Mutex<HashSet<String>>,
    /// The ports Python lent to the node's native code through its module host
    /// table; Python's own reads and writes of them are refused by name.
    input_ports_lent_to_native_code: parking_lot::Mutex<HashSet<String>>,
    output_ports_lent_to_native_code: parking_lot::Mutex<HashSet<String>>,
    /// Whether the runtime drives `process()` from this node's inputs, which
    /// would race native code waiting on a lent input for the same wakeups.
    runtime_drives_process_from_inputs: std::sync::atomic::AtomicBool,
}

/// The data plane a module host table reaches a helper's lent ports through.
pub(crate) struct HelperProcessDataPlaneLentToNativeCode {
    pub(crate) input_mailboxes: Arc<InputMailboxesInner>,
    pub(crate) output_writer: Arc<OutputWriterInner>,
    /// Lent port names, each cast the way `@node` casts it.
    pub(crate) lent_input_port_names: Vec<String>,
    pub(crate) lent_output_port_names: Vec<String>,
}

impl PythonProcessorLinkDataAccess {
    pub(crate) fn new() -> Self {
        Self {
            input_mailboxes: OnceLock::new(),
            output_writer: OnceLock::new(),
            iceoryx2_node: OnceLock::new(),
            loss_count_board_writer: OnceLock::new(),
            stream_clock_board_publish_listener: OnceLock::new(),
            declared_input_ports: parking_lot::Mutex::new(HashSet::new()),
            declared_output_ports: parking_lot::Mutex::new(HashSet::new()),
            input_ports_lent_to_native_code: parking_lot::Mutex::new(HashSet::new()),
            output_ports_lent_to_native_code: parking_lot::Mutex::new(HashSet::new()),
            runtime_drives_process_from_inputs: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Record whether the runtime drives `process()` from this node's inputs,
    /// refusing it when an input is already lent to native code.
    pub(crate) fn note_whether_the_runtime_drives_process_from_inputs(
        &self,
        runtime_drives_process_from_inputs: bool,
    ) -> PyResult<()> {
        if runtime_drives_process_from_inputs {
            let lent_input_ports = self.input_ports_lent_to_native_code.lock();
            if !lent_input_ports.is_empty() {
                let mut lent: Vec<&String> = lent_input_ports.iter().collect();
                lent.sort();
                return Err(PyValueError::new_err(format!(
                    "input ports {lent:?} are lent to native code, so the runtime cannot drive \
                     this node's `process()` from its inputs. A node that lends an input is \
                     declared `execution=\"manual\"`"
                )));
            }
        }
        self.runtime_drives_process_from_inputs.store(
            runtime_drives_process_from_inputs,
            std::sync::atomic::Ordering::Release,
        );
        Ok(())
    }

    /// Lend `input_port_names` and `output_port_names` to the node's native
    /// code, refusing a name the class never declared or one already lent.
    pub(crate) fn lend_ports_to_native_code(
        &self,
        input_port_names: &[String],
        output_port_names: &[String],
    ) -> PyResult<HelperProcessDataPlaneLentToNativeCode> {
        let (Some(input_mailboxes), Some(output_writer)) =
            (self.input_mailboxes.get(), self.output_writer.get())
        else {
            return Err(not_a_helper_process_data_plane_error());
        };
        if !input_port_names.is_empty()
            && self
                .runtime_drives_process_from_inputs
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(PyValueError::new_err(format!(
                "input ports {input_port_names:?} cannot be lent to native code: the runtime \
                 drives this node's `process()` from its inputs. A node that lends an input \
                 is declared `execution=\"manual\"`"
            )));
        }
        let lent_input_port_names = Self::cast_and_check_ports_to_lend(
            "input",
            input_port_names,
            &self.declared_input_ports,
            &self.input_ports_lent_to_native_code,
        )?;
        let lent_output_port_names = Self::cast_and_check_ports_to_lend(
            "output",
            output_port_names,
            &self.declared_output_ports,
            &self.output_ports_lent_to_native_code,
        )?;
        self.input_ports_lent_to_native_code
            .lock()
            .extend(lent_input_port_names.iter().cloned());
        self.output_ports_lent_to_native_code
            .lock()
            .extend(lent_output_port_names.iter().cloned());
        Ok(HelperProcessDataPlaneLentToNativeCode {
            input_mailboxes: Arc::clone(input_mailboxes),
            output_writer: Arc::clone(output_writer),
            lent_input_port_names,
            lent_output_port_names,
        })
    }

    fn cast_and_check_ports_to_lend(
        direction: &str,
        port_names: &[String],
        declared_ports: &parking_lot::Mutex<HashSet<String>>,
        ports_already_lent: &parking_lot::Mutex<HashSet<String>>,
    ) -> PyResult<Vec<String>> {
        let declared_ports = declared_ports.lock();
        let ports_already_lent = ports_already_lent.lock();
        let mut lent_port_names: Vec<String> = Vec::with_capacity(port_names.len());
        for port_name in port_names {
            let port_name = declared_port_name_the_spelling_names(port_name)?.into_owned();
            if !declared_ports.contains(&port_name) {
                let mut declared: Vec<&String> = declared_ports.iter().collect();
                declared.sort();
                return Err(PyValueError::new_err(format!(
                    "{direction} port {port_name:?} cannot be lent to native code: the node \
                     declares no such port (it declares {declared:?})"
                )));
            }
            if ports_already_lent.contains(&port_name) || lent_port_names.contains(&port_name) {
                return Err(PyValueError::new_err(format!(
                    "{direction} port {port_name:?} is already lent to native code; a port is \
                     lent once"
                )));
            }
            lent_port_names.push(port_name);
        }
        Ok(lent_port_names)
    }

    fn refuse_a_python_read_of_a_port_lent_to_native_code(&self, port_name: &str) -> PyResult<()> {
        if self
            .input_ports_lent_to_native_code
            .lock()
            .contains(port_name)
        {
            return Err(PyRuntimeError::new_err(format!(
                "input port {port_name:?} is lent to this node's native code through its module \
                 host table, so Python does not read it"
            )));
        }
        Ok(())
    }

    fn refuse_a_python_write_to_a_port_lent_to_native_code(&self, port_name: &str) -> PyResult<()> {
        if self
            .output_ports_lent_to_native_code
            .lock()
            .contains(port_name)
        {
            return Err(PyRuntimeError::new_err(format!(
                "output port {port_name:?} is lent to this node's native code through its module \
                 host table, so Python does not write it"
            )));
        }
        Ok(())
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
        self.refuse_a_python_read_of_a_port_lent_to_native_code(port_name)?;
        match self.input_mailboxes.get() {
            Some(input_mailboxes) if input_mailboxes.has_port(port_name) => {
                Ok(Some(input_mailboxes))
            }
            _ if self.declared_input_ports.lock().contains(port_name) => Ok(None),
            _ => Err(unwired_port_error("input", port_name)),
        }
    }

    /// The writer `port_name` publishes through; `None` for a declared output
    /// port no link has reached yet, whose messages go nowhere rather than
    /// raising.
    fn output_writer_reaching(&self, port_name: &str) -> PyResult<Option<&Arc<OutputWriterInner>>> {
        self.refuse_a_python_write_to_a_port_lent_to_native_code(port_name)?;
        match self.output_writer.get() {
            Some(output_writer) if output_writer.has_port(port_name) => Ok(Some(output_writer)),
            _ if self.declared_output_ports.lock().contains(port_name) => Ok(None),
            _ => Err(unwired_port_error("output", port_name)),
        }
    }

    /// The open loss-count board and where a link being wired mirrors onto it,
    /// or `None` with no board open, where there is nowhere to mirror.
    ///
    /// A helper with a board open must be told where every link's counts go, so
    /// a link wired without it is refused naming what was missing.
    fn loss_count_board_writer_with_where_a_link_mirrors_to<MirrorTarget>(
        &self,
        link_direction: &str,
        link_id: &str,
        mirror_target: Option<MirrorTarget>,
        missing_arguments: &str,
    ) -> PyResult<Option<(&Arc<HelperProcessLossCountBoardWriter>, MirrorTarget)>> {
        match (self.loss_count_board_writer.get(), mirror_target) {
            (None, _) => Ok(None),
            (Some(board_writer), Some(mirror_target)) => Ok(Some((board_writer, mirror_target))),
            (Some(_), None) => Err(PyValueError::new_err(format!(
                "{link_direction} link {link_id:?} was wired without {missing_arguments}, so this \
                 helper cannot mirror the link's losses onto its loss-count board"
            ))),
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

    /// The next message's payload on `port_name`, offering
    /// `offered_gpu_limited_access` to `into.from_bytes` when `into` is named.
    ///
    /// The one read body; the context-carrying reader passes its capability
    /// and the bare data plane passes nothing.
    pub(crate) fn read_from_input_port_offering_gpu_access<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        read_shape: PortPayloadReadShape<'_, 'py>,
        offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        let port_name = &*declared_port_name_the_spelling_names(port_name)?;
        let Some(input_mailboxes) = self.input_mailboxes_reaching(port_name)? else {
            return Ok(None);
        };
        let read = python
            .detach(|| input_mailboxes.read_raw(port_name))
            .map_err(|read_failure| PyRuntimeError::new_err(read_failure.to_string()))?;
        match read {
            Some(message) => payload_borrowed_into_the_read_target_when_named(
                python,
                port_name,
                message.payload,
                message.payload_descriptor,
                message.timestamp_ns,
                message.group_boundary,
                read_shape,
                offered_gpu_limited_access,
            )
            .map(Some),
            None => Ok(None),
        }
    }

    /// The next message's payload on `port_name` with its stamp, or
    /// `(None, None)` when the mailbox is empty, offering
    /// `offered_gpu_limited_access` the pooled frame the message carries.
    pub(crate) fn read_from_input_port_with_timestamp_offering_gpu_access<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        read_shape: PortPayloadReadShape<'_, 'py>,
        offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
    ) -> PyResult<(Option<Bound<'py, PyAny>>, Option<i64>)> {
        let port_name = &*declared_port_name_the_spelling_names(port_name)?;
        let Some(input_mailboxes) = self.input_mailboxes_reaching(port_name)? else {
            return Ok((None, None));
        };
        let read = python
            .detach(|| input_mailboxes.read_raw(port_name))
            .map_err(|read_failure| PyRuntimeError::new_err(read_failure.to_string()))?;
        match read {
            Some(message) => Ok((
                Some(payload_borrowed_into_the_read_target_when_named(
                    python,
                    port_name,
                    message.payload,
                    message.payload_descriptor,
                    message.timestamp_ns,
                    message.group_boundary,
                    read_shape,
                    offered_gpu_limited_access,
                )?),
                Some(message.timestamp_ns),
            )),
            None => Ok((None, None)),
        }
    }

    /// The next message's payload on `port_name` with the inbound link it
    /// arrived on, or `None` when the mailbox is empty.
    ///
    /// The read a destination taking many links on one port uses: each inbound
    /// link is one producer, named by the source channel name it subscribed to.
    pub(crate) fn read_from_input_port_naming_its_inbound_link<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        read_shape: PortPayloadReadShape<'_, 'py>,
        offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
    ) -> PyResult<Option<(Bound<'py, PyAny>, String)>> {
        Ok(self
            .read_from_input_port_naming_its_inbound_link_and_timestamp(
                python,
                port_name,
                read_shape,
                offered_gpu_limited_access,
            )?
            .map(|(payload, inbound_link_name, _timestamp_ns)| (payload, inbound_link_name)))
    }

    /// The same read, keeping the frame header's timestamp.
    ///
    /// A many-track sink needs both halves at once: the link says which
    /// producer sent the message, and the stamp is the producer's own — the
    /// source frame's instant, not the moment the message was read.
    pub(crate) fn read_from_input_port_naming_its_inbound_link_and_timestamp<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        read_shape: PortPayloadReadShape<'_, 'py>,
        offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
    ) -> PyResult<Option<(Bound<'py, PyAny>, String, i64)>> {
        Ok(self
            .read_message_naming_its_inbound_link_with_its_payload_cast(
                python,
                port_name,
                read_shape,
                offered_gpu_limited_access,
            )?
            .map(|(payload, message, inbound_link_name)| {
                (
                    payload,
                    inbound_link_name.as_str().to_string(),
                    message.timestamp_ns,
                )
            }))
    }

    /// The next message on `port_name` as a `tatolab.stream.Message` — its
    /// payload, stamp, group flag, losses and inbound link — or `None` when the
    /// mailbox is empty.
    pub(crate) fn read_message_from_input_port<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        read_shape: PortPayloadReadShape<'_, 'py>,
        offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        static TATOLAB_STREAM_MESSAGE_CLASS: PyOnceLock<Py<PyType>> = PyOnceLock::new();
        let Some((payload, message, inbound_link_name)) = self
            .read_message_naming_its_inbound_link_with_its_payload_cast(
                python,
                port_name,
                read_shape,
                offered_gpu_limited_access,
            )?
        else {
            return Ok(None);
        };
        let message_fields = PyDict::new(python);
        message_fields.set_item("payload", payload)?;
        message_fields.set_item("timestamp_ns", message.timestamp_ns)?;
        message_fields.set_item(
            "starts_group",
            message.group_boundary == MessageGroupBoundary::StartsGroup,
        )?;
        message_fields.set_item("lost_since_last_read", message.lost_since_last_read)?;
        message_fields.set_item("inbound_link", inbound_link_name.as_str())?;
        TATOLAB_STREAM_MESSAGE_CLASS
            .import(python, "tatolab.stream._message", "Message")?
            .call((), Some(&message_fields))
            .map(Some)
    }

    /// The one fan-in read body: the next message on `port_name`, its payload
    /// borrowed or cast by `into`, and the inbound link it arrived on.
    fn read_message_naming_its_inbound_link_with_its_payload_cast<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        read_shape: PortPayloadReadShape<'_, 'py>,
        offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
    ) -> PyResult<Option<(Bound<'py, PyAny>, MessageReadFromInputPort, InboundLinkName)>> {
        let port_name = &*declared_port_name_the_spelling_names(port_name)?;
        let Some(input_mailboxes) = self.input_mailboxes_reaching(port_name)? else {
            return Ok(None);
        };
        let read = python
            .detach(|| input_mailboxes.read_raw_from_inbound_link(port_name))
            .map_err(|read_failure| PyRuntimeError::new_err(read_failure.to_string()))?;
        let Some((mut message, inbound_link_name)) = read else {
            return Ok(None);
        };
        let payload = payload_borrowed_into_the_read_target_when_named(
            python,
            port_name,
            std::mem::take(&mut message.payload),
            message.payload_descriptor,
            message.timestamp_ns,
            message.group_boundary,
            read_shape,
            offered_gpu_limited_access,
        )?;
        Ok(Some((payload, message, inbound_link_name)))
    }

    /// Count the message last read from `port_name` as lost on the inbound
    /// link that delivered it, once — into the link's dropped count the
    /// parent's `graph` renders, through the helper's board.
    pub(crate) fn count_the_message_last_read_from_input_port_as_lost(
        &self,
        python: Python<'_>,
        port_name: &str,
    ) -> PyResult<()> {
        let port_name = &*declared_port_name_the_spelling_names(port_name)?;
        let Some(input_mailboxes) = self.input_mailboxes_reaching(port_name)? else {
            return Err(PyRuntimeError::new_err(format!(
                "no link has reached input port '{port_name}', so nothing read from it can be \
                 counted as lost"
            )));
        };
        python
            .detach(|| input_mailboxes.count_the_message_last_read_as_lost(port_name))
            .map_err(|refusal| PyRuntimeError::new_err(refusal.to_string()))
    }

    /// Every inbound link feeding `port_name`, in wiring order.
    pub(crate) fn inbound_links_of_input_port(&self, port_name: &str) -> PyResult<Vec<String>> {
        let port_name = &*declared_port_name_the_spelling_names(port_name)?;
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

/// What a read hands back for a message's payload.
#[derive(Clone, Copy)]
pub(crate) enum PortPayloadReadShape<'target, 'py> {
    /// The `PortPayloadBorrow` of the sample: a read by name.
    RawBorrow,
    /// The family's own type, built from the message's descriptor kind: a
    /// read through a port attribute of that family.
    FamilyType(PortFamilyOfTheRead),
    /// What `into.from_bytes` builds from the borrow.
    CastInto(&'target Bound<'py, PyAny>),
}

/// The family of the `Input` attribute a read names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PortFamilyOfTheRead {
    Image,
    Array,
    Channels,
    Packets,
    Records,
}

impl PortFamilyOfTheRead {
    /// The family a declaration spells as `declaration_name`, such as `Image`.
    pub(crate) fn of_declaration_name(declaration_name: &str) -> Option<Self> {
        Some(match declaration_name {
            "Image" => Self::Image,
            "Array" => Self::Array,
            "Channels" => Self::Channels,
            "Packets" => Self::Packets,
            "Records" => Self::Records,
            _ => return None,
        })
    }

    /// The descriptor kind every message of this family carries.
    fn descriptor_kind(self) -> DataChannelPayloadDescriptorKind {
        match self {
            Self::Image => DataChannelPayloadDescriptorKind::ImageFrame,
            Self::Array => DataChannelPayloadDescriptorKind::ArrayFrame,
            Self::Channels => DataChannelPayloadDescriptorKind::ChannelsBlock,
            Self::Packets => DataChannelPayloadDescriptorKind::PacketsUnit,
            Self::Records => DataChannelPayloadDescriptorKind::RecordsMessage,
        }
    }
}

impl<'target, 'py> PortPayloadReadShape<'target, 'py> {
    /// The shape a read by name asks for: cast by `into`, or the raw borrow.
    pub(crate) fn by_name_cast_into(into: Option<&'target Bound<'py, PyAny>>) -> Self {
        into.map_or(Self::RawBorrow, Self::CastInto)
    }
}

/// One message's payload as a `PortPayloadBorrow`, or what `into.from_bytes`
/// builds from that borrow when `into` names a target — the half every read
/// shares once the payload is in hand.
#[allow(clippy::too_many_arguments)]
fn payload_borrowed_into_the_read_target_when_named<'py>(
    python: Python<'py>,
    port_name: &str,
    payload: InputPortMessagePayload,
    payload_descriptor: DataChannelPayloadDescriptor,
    timestamp_ns: i64,
    group_boundary: MessageGroupBoundary,
    read_shape: PortPayloadReadShape<'_, 'py>,
    offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
) -> PyResult<Bound<'py, PyAny>> {
    let into = match read_shape {
        PortPayloadReadShape::CastInto(into) => Some(into),
        PortPayloadReadShape::RawBorrow | PortPayloadReadShape::FamilyType(_) => None,
    };
    if let PortPayloadReadShape::FamilyType(family) = read_shape {
        let kind = payload_descriptor.known_kind();
        if kind != Ok(family.descriptor_kind()) {
            return Err(PyTypeError::new_err(format!(
                "a read of {family:?} input port {port_name:?} got a message described as \
                 {kind:?} rather than {:?}: its producer wrote it by name rather than through \
                 a {family:?} Output. Read it by name to take its raw payload.",
                family.descriptor_kind()
            )));
        }
        match kind {
            Ok(DataChannelPayloadDescriptorKind::ChannelsBlock) => {
                let block =
                    ChannelsBlockDescriptorBody::from_descriptor_body(&payload_descriptor.body);
                return Ok(Bound::new(
                    python,
                    PythonChannelsBlock::holding(port_name, payload, block, timestamp_ns)?,
                )?
                .into_any());
            }
            Ok(DataChannelPayloadDescriptorKind::RecordsMessage) => {
                return records_message_dict_of(python, port_name, payload.bytes());
            }
            Ok(DataChannelPayloadDescriptorKind::ImageFrame) => {
                return image_frame_of_the_read(
                    python,
                    port_name,
                    ImageFrameDescriptorBody::from_descriptor_body(&payload_descriptor.body),
                    timestamp_ns,
                    offered_gpu_limited_access,
                );
            }
            Ok(DataChannelPayloadDescriptorKind::ArrayFrame) => {
                return array_frame_of_the_read(
                    python,
                    port_name,
                    payload,
                    ArrayFrameDescriptorBody::from_descriptor_body(&payload_descriptor.body),
                    timestamp_ns,
                    offered_gpu_limited_access,
                );
            }
            Ok(DataChannelPayloadDescriptorKind::PacketsUnit) => {
                let unit =
                    PacketsUnitDescriptorBody::from_descriptor_body(&payload_descriptor.body);
                let payload_borrow =
                    Bound::new(python, PythonPortPayloadBorrow::holding(payload, None))?;
                return Ok(Bound::new(
                    python,
                    PythonPacketsUnit {
                        payload: payload_borrow.unbind(),
                        pts: timestamp_ns,
                        starts_group: group_boundary == MessageGroupBoundary::StartsGroup,
                        codec: unit.codec,
                    },
                )?
                .into_any());
            }
            _ => {}
        }
    }
    let mut pooled_gpu_frame_claim = None;
    let pooled_frame_the_message_carries = match payload_descriptor.known_kind() {
        Ok(DataChannelPayloadDescriptorKind::GpuPoolSlotFrame) => Some(
            GpuPoolSlotFrameDescriptorBody::from_descriptor_body(&payload_descriptor.body),
        ),
        Ok(DataChannelPayloadDescriptorKind::ImageFrame) => {
            Some(ImageFrameDescriptorBody::from_descriptor_body(&payload_descriptor.body).slot)
        }
        _ => None,
    };
    if let (Some(pooled_frame), Some(gpu_limited_access)) =
        (pooled_frame_the_message_carries, offered_gpu_limited_access)
    {
        let claimed = gpu_limited_access
            .get()
            .note_and_claim_the_pooled_gpu_frame_a_read_carries(python, pooled_frame);
        match (claimed, into) {
            (Ok(claim), _) => pooled_gpu_frame_claim = claim,
            (Err(refusal), Some(_)) => return Err(refusal),
            (Err(refusal), None) => tracing::warn!(
                "a read on {port_name:?} kept its payload, though the pooled GPU frame it \
                 carries could not be claimed by its claim table: {refusal}"
            ),
        }
    }
    let payload_borrow = Bound::new(
        python,
        PythonPortPayloadBorrow::holding(payload, pooled_gpu_frame_claim),
    )?
    .into_any();
    match into {
        Some(read_target) => cast_payload_into_read_target(
            port_name,
            &payload_borrow,
            read_target,
            offered_gpu_limited_access,
        ),
        None => Ok(payload_borrow),
    }
}

/// The surface id a pooled GPU frame a read carried is published under, with
/// the claim the read took on it; refused where the read offers no GPU
/// capability or no claim table this helper holds covers the frame.
fn claimed_pooled_gpu_frame_of_the_read(
    python: Python<'_>,
    port_name: &str,
    pooled_frame: GpuPoolSlotFrameDescriptorBody,
    offered_gpu_limited_access: Option<&Bound<'_, PythonGpuContextLimitedAccess>>,
    what_the_read_carries: &str,
) -> PyResult<(String, Option<PooledGpuFrameClaimHeldByARead>)> {
    let gpu_limited_access = offered_gpu_limited_access.ok_or_else(|| {
        PyRuntimeError::new_err(format!(
            "{what_the_read_carries} on {port_name:?} lives in a GPU pool slot, and this read \
             offers no GPU capability to claim it with"
        ))
    })?;
    let claim = gpu_limited_access
        .get()
        .note_and_claim_the_pooled_gpu_frame_a_read_carries(python, pooled_frame)?;
    let surface_id = gpu_limited_access
        .get()
        .published_frame_id_of_the_pooled_gpu_frame(&pooled_frame)
        .ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "{what_the_read_carries} on {port_name:?} names a GPU pool slot no claim table \
                 this helper holds covers"
            ))
        })?;
    Ok((surface_id, claim))
}

/// The `tatolab.stream.video_frame.ColorInfo` an Image frame's H.273 codes spell.
fn color_info_of_the_image_frame_colour<'py>(
    python: Python<'py>,
    colour: ImageFrameH273Colour,
) -> PyResult<Bound<'py, PyAny>> {
    let [primaries, transfer, matrix, range] =
        color_info_axis_names_of_h273_color_vui(&H273ColorVui {
            primaries: Some(colour.primaries),
            transfer: Some(colour.transfer),
            matrix: Some(colour.matrix),
            full_range: match colour.range {
                0 => Some(false),
                1 => Some(true),
                _ => None,
            },
        });
    let axes = PyDict::new(python);
    axes.set_item("primaries", primaries)?;
    axes.set_item("transfer", transfer)?;
    axes.set_item("matrix", matrix)?;
    axes.set_item("range", range)?;
    python
        .import("tatolab.stream.video_frame")?
        .getattr("ColorInfo")?
        .call((), Some(&axes))
}

/// The `ImageFrame` an Image descriptor states, its slot claimed.
fn image_frame_of_the_read<'py>(
    python: Python<'py>,
    port_name: &str,
    image_frame: ImageFrameDescriptorBody,
    timestamp_ns: i64,
    offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
) -> PyResult<Bound<'py, PyAny>> {
    let format = TextureFormat::from_repr_value(image_frame.format).ok_or_else(|| {
        PyValueError::new_err(format!(
            "an Image frame on {port_name:?} names texture format {}, which this reader does \
             not know",
            image_frame.format
        ))
    })?;
    let (surface_id, _claim_held_while_the_frame_claims) = claimed_pooled_gpu_frame_of_the_read(
        python,
        port_name,
        image_frame.slot,
        offered_gpu_limited_access,
        "an Image frame",
    )?;
    let fields = PyDict::new(python);
    fields.set_item("surface_id", surface_id)?;
    fields.set_item("width", image_frame.width)?;
    fields.set_item("height", image_frame.height)?;
    fields.set_item("format", format.wire_name())?;
    fields.set_item(
        "colour",
        color_info_of_the_image_frame_colour(python, image_frame.colour)?,
    )?;
    fields.set_item("timestamp_ns", timestamp_ns)?;
    family_type_built_offering_gpu_access(
        &python
            .import("tatolab.stream.image_frame")?
            .getattr("ImageFrame")?,
        &fields,
        offered_gpu_limited_access,
    )
}

/// The `ArrayFrame` an Array payload's attribute table states: each CPU
/// attribute borrowed in the sample, each GPU attribute's slot claimed.
fn array_frame_of_the_read<'py>(
    python: Python<'py>,
    port_name: &str,
    payload: InputPortMessagePayload,
    array_frame: ArrayFrameDescriptorBody,
    timestamp_ns: i64,
    offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
) -> PyResult<Bound<'py, PyAny>> {
    let rows = array_frame_attribute_table_of(array_frame, payload.bytes()).map_err(|refusal| {
        PyValueError::new_err(format!("an Array frame on {port_name:?}: {refusal}"))
    })?;
    let payload_base_address = payload.bytes().as_ptr() as usize;
    let borrowed_payload = Arc::new(parking_lot::Mutex::new(payload));
    let array_frame_module = python.import("tatolab.stream.array_frame")?;
    let attributes = PyDict::new(python);
    let mut claims_held_while_each_attribute_claims = Vec::new();
    for row in &rows {
        if attributes.contains(&row.name)? {
            return Err(PyValueError::new_err(format!(
                "an Array frame on {port_name:?} names attribute {:?} twice",
                row.name
            )));
        }
        let attribute = match row.residency {
            ArrayFrameAttributeResidency::Cpu {
                payload_byte_offset,
                byte_len,
            } => Bound::new(
                python,
                PythonArrayFrameCpuAttribute::over(
                    port_name,
                    &borrowed_payload,
                    payload_base_address,
                    row,
                    payload_byte_offset,
                    byte_len,
                )?,
            )?
            .into_any(),
            ArrayFrameAttributeResidency::GpuPoolSlot(pooled_frame) => {
                let (surface_id, claim) = claimed_pooled_gpu_frame_of_the_read(
                    python,
                    port_name,
                    pooled_frame,
                    offered_gpu_limited_access,
                    &format!("Array attribute {:?}", row.name),
                )?;
                claims_held_while_each_attribute_claims.push(claim);
                let fields = PyDict::new(python);
                fields.set_item("surface_id", surface_id)?;
                fields.set_item("dtype", &row.dtype)?;
                fields.set_item("shape", PyTuple::new(python, &row.shape)?)?;
                family_type_built_offering_gpu_access(
                    &array_frame_module.getattr("ArrayFrameGpuAttribute")?,
                    &fields,
                    offered_gpu_limited_access,
                )?
            }
        };
        attributes.set_item(&row.name, attribute)?;
    }
    array_frame_module
        .getattr("ArrayFrame")?
        .call1((attributes, timestamp_ns))
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
) -> PyResult<PythonProcessorLinkDataAccess> {
    let iceoryx2_domain_root = std::env::var_os(ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE)
        .filter(|root| !root.is_empty())
        .ok_or_else(|| {
            PyRuntimeError::new_err(format!(
                "{ICEORYX2_DOMAIN_ROOT_ENVIRONMENT_VARIABLE} is not set: a helper process \
                 opens its iceoryx2 node only in the domain its parent runtime hands it"
            ))
        })?;
    let node_name = std::env::var(PROCESSOR_INTERPRETER_PROCESSOR_ID_ENVIRONMENT_VARIABLE)
        .map_or_else(
            |_| format!("streamlib-helper/pid{}", std::process::id()),
            |processor_id| format!("streamlib-helper/{processor_id}"),
        );
    let node = python
        .detach(|| Iceoryx2Node::new(std::path::Path::new(&iceoryx2_domain_root), &node_name))
        .map_err(|node_failure| PyRuntimeError::new_err(node_failure.to_string()))?;
    Ok(PythonProcessorLinkDataAccess::over_helper_process_iceoryx2_node(node))
}

#[pymethods]
impl PythonProcessorLinkDataAccess {
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
    #[allow(clippy::too_many_arguments)]
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
    ))]
    #[allow(clippy::too_many_arguments)]
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
    ) -> PyResult<()> {
        let (node, input_mailboxes) = self.helper_process_input_plane()?;
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

    /// The next message's payload on `port_name`, or `None` when the mailbox is
    /// empty.
    ///
    /// Without `into` the payload arrives as a `PortPayloadBorrow`; with it the
    /// read returns `into.from_bytes(payload)`, so a mismatch surfaces here, at
    /// the consuming read, and nowhere else.
    ///
    /// This is the plumbing a helper process wires by hand; it holds no
    /// context, so a target's `from_bytes` is offered no GPU capability. The
    /// read a processor writes is `ctx.inputs.read`, which does.
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

    /// Name the ports the processor class declared, ahead of any wiring.
    ///
    /// A declared port that no link has reached — a processor added to a
    /// running graph before its connects, or one whose last link was taken
    /// away — reads as empty and drops what is written to it; only a name the
    /// class never declared is refused.
    pub(crate) fn declare_ports(
        &self,
        input_port_names: Vec<String>,
        output_port_names: Vec<String>,
    ) {
        self.declared_input_ports.lock().extend(input_port_names);
        self.declared_output_ports.lock().extend(output_port_names);
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

impl PythonProcessorLinkDataAccess {
    /// Publish `samples`, a `[channel][sample]` array, as one Channels block
    /// on `port_name` starting at `start_sample`, at `sample_rate`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_channels_block_to_output_port(
        &self,
        python: Python<'_>,
        port_name: &str,
        samples: &Bound<'_, PyAny>,
        start_sample: u64,
        sample_rate: u32,
        declared_shape: ChannelsOutputPortDeclaredShape,
        timestamp_ns: Option<i64>,
        starts_group: bool,
    ) -> PyResult<()> {
        if sample_rate == 0 {
            return Err(PyValueError::new_err(format!(
                "write_channels_block() on output port {port_name:?} takes a sample_rate above 0"
            )));
        }
        let (held_samples, sample_type, channel_count, samples_per_channel) =
            PythonBufferHeldForOneWrite::of_planar_channel_samples(port_name, samples)?;
        if let Some(declared_sample_type) = declared_shape.sample_type
            && declared_sample_type != sample_type
        {
            return Err(PyTypeError::new_err(format!(
                "a write on Channels output port {port_name:?} holds {sample_type:?} samples, \
                 and the port declares {declared_sample_type:?}"
            )));
        }
        if let Some(declared_channel_count) = declared_shape.channel_count
            && declared_channel_count != channel_count
        {
            return Err(PyValueError::new_err(format!(
                "a write on Channels output port {port_name:?} holds {channel_count} channels, \
                 and the port declares {declared_channel_count} names"
            )));
        }
        let block = ChannelsBlockDescriptorBody {
            start_sample,
            sample_rate,
            channel_count,
            samples_per_channel,
            sample_type: sample_type as u32,
        };
        drop(held_samples);
        self.write_described_by(
            python,
            port_name,
            samples,
            timestamp_ns,
            starts_group,
            |_| Ok(DataChannelPayloadDescriptor::channels_block(block)),
        )
    }

    /// Publish `image`, an `ImageFrame` this node read or a GPU surface it
    /// acquired from a pool, as one Image frame on `port_name`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_image_frame_to_output_port(
        &self,
        python: Python<'_>,
        port_name: &str,
        image: &Bound<'_, PyAny>,
        declared_formats: &[String],
        gpu_limited_access: &PythonGpuContextLimitedAccess,
        timestamp_ns: Option<i64>,
        starts_group: bool,
    ) -> PyResult<()> {
        let not_an_image = || {
            PyTypeError::new_err(format!(
                "a write on Image output port {port_name:?} takes an ImageFrame this node read \
                 or a texture it acquired from a pool; got a {}",
                python_type_name_for_error_message(image, "value")
            ))
        };
        let surface_id: String = image
            .getattr_opt("surface_id")?
            .ok_or_else(not_an_image)?
            .extract()
            .map_err(|_| not_an_image())?;
        let slot = gpu_limited_access
            .descriptor_of_the_pooled_gpu_frame_named(&surface_id)
            .ok_or_else(|| {
                PyValueError::new_err(format!(
                    "a write on Image output port {port_name:?} names surface {surface_id:?}, \
                     which is no pooled GPU frame this node acquired or read"
                ))
            })?;
        let format_name: String = image.getattr("format")?.extract()?;
        let format = TextureFormat::from_wire_name(&format_name).ok_or_else(|| {
            PyValueError::new_err(format!(
                "a write on Image output port {port_name:?} names format {format_name:?}, which \
                 is no texture format an Image frame carries"
            ))
        })?;
        if !declared_formats.is_empty() && !declared_formats.contains(&format_name) {
            return Err(PyValueError::new_err(format!(
                "a write on Image output port {port_name:?} carries format {format_name:?}, and \
                 the port declares formats={declared_formats:?}"
            )));
        }
        let image_frame = ImageFrameDescriptorBody {
            slot,
            width: image.getattr("width")?.extract()?,
            height: image.getattr("height")?.extract()?,
            format: format as u32,
            colour: image_frame_colour_the_image_states(image)?,
        };
        self.write_described_by(
            python,
            port_name,
            PyBytes::new(python, &[]).as_any(),
            timestamp_ns,
            starts_group,
            |_| Ok(DataChannelPayloadDescriptor::image_frame(image_frame)),
        )
    }

    /// Publish `attributes`, a mapping of name to a C-contiguous CPU array or
    /// a tensor surface this node acquired from a pool, as one Array frame on
    /// `port_name`; refused when a name in `declared_names` is missing.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn write_array_frame_to_output_port(
        &self,
        python: Python<'_>,
        port_name: &str,
        attributes: &Bound<'_, PyAny>,
        declared_names: &[String],
        declared_residency: Option<ArrayOutputPortDeclaredResidency>,
        gpu_limited_access: &PythonGpuContextLimitedAccess,
        timestamp_ns: Option<i64>,
        starts_group: bool,
    ) -> PyResult<()> {
        let attributes = attributes.cast::<PyMapping>().map_err(|_| {
            PyTypeError::new_err(format!(
                "a write on Array output port {port_name:?} takes a mapping of attribute name \
                 to a CPU array or a GPU tensor surface; got a {}",
                python_type_name_for_error_message(attributes, "value")
            ))
        })?;
        for declared_name in declared_names {
            if !attributes.contains(declared_name)? {
                return Err(PyValueError::new_err(format!(
                    "a write on Array output port {port_name:?} is missing attribute \
                     {declared_name:?}, which the port declares in names="
                )));
            }
        }
        let mut rows = Vec::new();
        let mut cpu_data_held = Vec::new();
        for item in attributes.items()?.iter() {
            let (name, value): (String, Bound<'_, PyAny>) = item.extract()?;
            if let Ok(read_attribute) = value.cast::<PythonArrayFrameCpuAttribute>() {
                let byte_len = read_attribute.get().attribute_bytes().len() as u64;
                rows.push(ArrayFrameAttributeRow {
                    name,
                    dtype: read_attribute.get().dtype().to_string(),
                    shape: read_attribute.get().shape(),
                    residency: ArrayFrameAttributeResidency::Cpu {
                        payload_byte_offset: 0,
                        byte_len,
                    },
                });
                cpu_data_held.push((
                    rows.len() - 1,
                    ArrayAttributeCpuBytesHeldForOneWrite::ReadAttribute(read_attribute.clone()),
                ));
                continue;
            }
            match value.getattr_opt("surface_id")? {
                Some(surface_id) => {
                    let surface_id: String = surface_id.extract()?;
                    let pooled_frame = gpu_limited_access
                        .descriptor_of_the_pooled_gpu_frame_named(&surface_id)
                        .ok_or_else(|| {
                            PyValueError::new_err(format!(
                                "Array attribute {name:?} on {port_name:?} names surface \
                                 {surface_id:?}, which is no pooled GPU frame this node \
                                 acquired or read"
                            ))
                        })?;
                    let (Some(dtype), Some(shape)) = (
                        value.getattr("dtype")?.extract::<Option<String>>()?,
                        value.getattr("shape")?.extract::<Option<Vec<u64>>>()?,
                    ) else {
                        return Err(PyValueError::new_err(format!(
                            "Array attribute {name:?} on {port_name:?} names surface \
                             {surface_id:?}, which is pixels rather than a tensor: acquire it \
                             with acquire_storage_buffer_from_node_output_pool"
                        )));
                    };
                    TensorStorageBufferLayout::from_wire(shape.clone(), &dtype).map_err(
                        |refusal| {
                            PyValueError::new_err(format!(
                                "Array attribute {name:?} on {port_name:?}: {refusal}"
                            ))
                        },
                    )?;
                    rows.push(ArrayFrameAttributeRow {
                        name,
                        dtype,
                        shape,
                        residency: ArrayFrameAttributeResidency::GpuPoolSlot(pooled_frame),
                    });
                }
                None => {
                    let (held, format, shape) = PythonBufferHeldForOneWrite::of_c_contiguous_array(
                        port_name,
                        &value,
                        "a mapping whose values are C-contiguous arrays or GPU tensor surfaces",
                    )?;
                    let dtype = array_attribute_element_type_of_the_buffer_format(&format)
                        .ok_or_else(|| {
                            PyTypeError::new_err(format!(
                                "Array attribute {name:?} on {port_name:?} has buffer format \
                                 {format:?}; an attribute's elements are one of {}",
                                TensorElementType::ALL
                                    .map(TensorElementType::wire_name)
                                    .join(", ")
                            ))
                        })?;
                    TensorStorageBufferLayout::new(shape.clone(), dtype).map_err(|refusal| {
                        PyValueError::new_err(format!(
                            "Array attribute {name:?} on {port_name:?}: {refusal}"
                        ))
                    })?;
                    let byte_len = held.bytes().len() as u64;
                    rows.push(ArrayFrameAttributeRow {
                        name,
                        dtype: dtype.wire_name().to_string(),
                        shape,
                        residency: ArrayFrameAttributeResidency::Cpu {
                            payload_byte_offset: 0,
                            byte_len,
                        },
                    });
                    cpu_data_held.push((
                        rows.len() - 1,
                        ArrayAttributeCpuBytesHeldForOneWrite::PythonBuffer(held),
                    ));
                }
            }
        }
        if let Some(declared_residency) = declared_residency {
            for row in &rows {
                let row_is_on_the_gpu =
                    matches!(row.residency, ArrayFrameAttributeResidency::GpuPoolSlot(_));
                if row_is_on_the_gpu
                    != (declared_residency == ArrayOutputPortDeclaredResidency::Gpu)
                {
                    return Err(PyValueError::new_err(format!(
                        "Array attribute {:?} on {port_name:?} is {}, and the port declares \
                         residency={:?}",
                        row.name,
                        if row_is_on_the_gpu {
                            "a GPU tensor surface"
                        } else {
                            "CPU memory"
                        },
                        declared_residency.wire_name()
                    )));
                }
            }
        }
        let array_frame = ArrayFrameDescriptorBody {
            attribute_count: u32::try_from(rows.len()).map_err(|_| {
                PyValueError::new_err(format!(
                    "a write on Array output port {port_name:?} holds {} attributes, past what \
                     an Array frame states",
                    rows.len()
                ))
            })?,
        };
        let cpu_data_byte_lens: Vec<u64> = cpu_data_held
            .iter()
            .map(|(_, held)| held.bytes().len() as u64)
            .collect();
        let (cpu_data_offsets, payload_byte_len) =
            array_frame_cpu_data_offsets(rows.len(), &cpu_data_byte_lens);
        for ((row_index, _), offset) in cpu_data_held.iter().zip(&cpu_data_offsets) {
            if let ArrayFrameAttributeResidency::Cpu {
                payload_byte_offset,
                ..
            } = &mut rows[*row_index].residency
            {
                *payload_byte_offset = *offset;
            }
        }
        let attribute_table = rows
            .iter()
            .map(|row| {
                row.to_row_bytes().map_err(|refusal| {
                    PyValueError::new_err(format!(
                        "a write on Array output port {port_name:?}: {refusal}"
                    ))
                })
            })
            .collect::<PyResult<Vec<_>>>()?;
        let port_name = &*declared_port_name_the_spelling_names(port_name)?;
        let Some(output_writer) = self.output_writer_reaching(port_name)? else {
            return Ok(());
        };
        let payload_byte_len = usize::try_from(payload_byte_len).map_err(|_| {
            PyValueError::new_err(format!(
                "a write on Array output port {port_name:?} spans {payload_byte_len} bytes, past \
                 what this machine addresses"
            ))
        })?;
        let mut loan = match python
            .detach(|| output_writer.loan_output_port_payload(port_name, payload_byte_len))
        {
            Ok(loan) => loan,
            Err(refusal @ Error::PayloadExceedsChannelCeiling { .. }) => {
                tracing::warn!("output port {port_name:?}: {refusal}; the Array frame was dropped");
                return Ok(());
            }
            Err(refusal) => return Err(PyRuntimeError::new_err(refusal.to_string())),
        };
        let payload = loan.payload_bytes_mut();
        payload.fill(0);
        for (row_index, row_bytes) in attribute_table.iter().enumerate() {
            let start = row_index * ARRAY_FRAME_ATTRIBUTE_ROW_BYTES;
            payload[start..start + ARRAY_FRAME_ATTRIBUTE_ROW_BYTES].copy_from_slice(row_bytes);
        }
        for ((_, held), offset) in cpu_data_held.iter().zip(&cpu_data_offsets) {
            let start = *offset as usize;
            payload[start..start + held.bytes().len()].copy_from_slice(held.bytes());
        }
        drop(cpu_data_held);
        let timestamp_ns = timestamp_ns
            .unwrap_or_else(crate::python_helper_stream_clock::helper_stream_clock_now_ns);
        let group_boundary = if starts_group {
            MessageGroupBoundary::StartsGroup
        } else {
            MessageGroupBoundary::ContinuesGroup
        };
        python
            .detach(|| {
                output_writer.publish_output_port_payload_loan(
                    loan,
                    timestamp_ns,
                    group_boundary,
                    DataChannelPayloadDescriptor::array_frame(array_frame),
                )
            })
            .map_err(|refusal| PyRuntimeError::new_err(refusal.to_string()))
    }

    /// [`Self::write_to_output_port`], the message's header carrying the
    /// descriptor `descriptor_of_the_payload` answers for the payload's bytes.
    pub(crate) fn write_described_by(
        &self,
        python: Python<'_>,
        port_name: &str,
        payload: &Bound<'_, PyAny>,
        timestamp_ns: Option<i64>,
        starts_group: bool,
        descriptor_of_the_payload: impl FnOnce(&[u8]) -> PyResult<DataChannelPayloadDescriptor>,
    ) -> PyResult<()> {
        let port_name = &*declared_port_name_the_spelling_names(port_name)?;
        let payload_buffer = PythonBufferHeldForOneWrite::of(port_name, payload)?;
        let payload_bytes = payload_buffer.bytes();
        let payload_descriptor = descriptor_of_the_payload(payload_bytes)?;
        let Some(output_writer) = self.output_writer_reaching(port_name)? else {
            return Ok(());
        };
        let timestamp_ns = timestamp_ns
            .unwrap_or_else(crate::python_helper_stream_clock::helper_stream_clock_now_ns);
        let group_boundary = if starts_group {
            MessageGroupBoundary::StartsGroup
        } else {
            MessageGroupBoundary::ContinuesGroup
        };
        match python.detach(|| {
            output_writer.write_bytes_described_by(
                port_name,
                payload_bytes,
                timestamp_ns,
                group_boundary,
                payload_descriptor,
            )
        }) {
            Ok(()) => Ok(()),
            Err(Error::PayloadExceedsChannelCeiling {
                channel,
                payload_bytes,
                largest_admitted_frame_bytes,
                tier,
                refused_bags_on_the_output_port: refused_bags,
                ..
            }) => {
                tracing::warn!(
                    "output port {port_name:?} refused a {payload_bytes}-byte message past the \
                     {largest_admitted_frame_bytes} bytes channel {channel:?} admits ({tier} \
                     tier); the message was dropped, and the port has refused {refused_bags} so \
                     far"
                );
                Ok(())
            }
            Err(write_failure) => Err(PyRuntimeError::new_err(write_failure.to_string())),
        }
    }
}

/// The H.273 codes an `ImageFrame`'s `colour`, or a `VideoFrame`'s
/// `color_info`, spells; unspecified when the object states neither.
fn image_frame_colour_the_image_states(image: &Bound<'_, PyAny>) -> PyResult<ImageFrameH273Colour> {
    let colour = match image.getattr_opt("colour")? {
        Some(colour) => Some(colour),
        None => image.getattr_opt("color_info")?,
    }
    .filter(|colour| !colour.is_none());
    let Some(colour) = colour else {
        return Ok(ImageFrameH273Colour {
            primaries: H273_UNSPECIFIED,
            transfer: H273_UNSPECIFIED,
            matrix: H273_UNSPECIFIED,
            range: H273_UNSPECIFIED,
        });
    };
    let axis =
        |axis_name: &str| -> PyResult<Option<String>> { colour.getattr(axis_name)?.extract() };
    let (primaries, transfer, matrix, range) = (
        axis("primaries")?,
        axis("transfer")?,
        axis("matrix")?,
        axis("range")?,
    );
    let vui = h273_color_vui_from_color_info_axis_names(
        primaries.as_deref(),
        transfer.as_deref(),
        matrix.as_deref(),
        range.as_deref(),
    )
    .map_err(|refusal| PyValueError::new_err(refusal.to_string()))?;
    Ok(ImageFrameH273Colour {
        primaries: vui.primaries.unwrap_or(H273_UNSPECIFIED),
        transfer: vui.transfer.unwrap_or(H273_UNSPECIFIED),
        matrix: vui.matrix.unwrap_or(H273_UNSPECIFIED),
        range: vui.full_range.map_or(H273_UNSPECIFIED, u8::from),
    })
}

/// The tensor element type a buffer's struct format string names.
fn array_attribute_element_type_of_the_buffer_format(format: &str) -> Option<TensorElementType> {
    match format {
        "f" => Some(TensorElementType::Float32),
        "e" => Some(TensorElementType::Float16),
        "B" => Some(TensorElementType::Uint8),
        "i" => Some(TensorElementType::Int32),
        _ => None,
    }
}

/// What a Channels output port's class states about the blocks it carries,
/// which a write through its attribute is held to; nothing for a write by name.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ChannelsOutputPortDeclaredShape {
    /// The `dtype=` the port states.
    pub(crate) sample_type: Option<ChannelsBlockSampleType>,
    /// How many `names=` the port states.
    pub(crate) channel_count: Option<u32>,
}

/// Where an `Output(Array, residency=)` says every attribute it writes lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArrayOutputPortDeclaredResidency {
    Gpu,
    Cpu,
}

impl ArrayOutputPortDeclaredResidency {
    /// The spelling `residency=` takes.
    fn wire_name(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Cpu => "cpu",
        }
    }
}

/// The bytes of one CPU attribute an Array write copies into its sample: a
/// Python buffer, or an attribute a read of an Array port returned.
enum ArrayAttributeCpuBytesHeldForOneWrite<'py> {
    PythonBuffer(PythonBufferHeldForOneWrite),
    ReadAttribute(Bound<'py, PythonArrayFrameCpuAttribute>),
}

impl ArrayAttributeCpuBytesHeldForOneWrite<'_> {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::PythonBuffer(held) => held.bytes(),
            Self::ReadAttribute(read_attribute) => read_attribute.get().attribute_bytes(),
        }
    }
}

/// The contiguous buffer a write was handed, held for as long as its bytes are
/// copied out.
struct PythonBufferHeldForOneWrite {
    view: Box<pyo3::ffi::Py_buffer>,
}

// SAFETY: the view is read only while its write holds it, and released through
// the GIL its `Drop` attaches.
unsafe impl Send for PythonBufferHeldForOneWrite {}
// SAFETY: the bytes behind the view are only read.
unsafe impl Sync for PythonBufferHeldForOneWrite {}

impl PythonBufferHeldForOneWrite {
    /// Hold `payload`'s buffer, or refuse anything that exports no contiguous
    /// one, naming what it got.
    fn of(port_name: &str, payload: &Bound<'_, PyAny>) -> PyResult<Self> {
        let mut view = Box::new(std::mem::MaybeUninit::<pyo3::ffi::Py_buffer>::zeroed());
        // SAFETY: `view` is writable storage for one `Py_buffer`, which
        // `PyObject_GetBuffer` fills on success and leaves unowned on failure.
        let got = unsafe {
            pyo3::ffi::PyObject_GetBuffer(
                payload.as_ptr(),
                view.as_mut_ptr(),
                pyo3::ffi::PyBUF_SIMPLE,
            )
        };
        if got != 0 {
            let refusal = PyErr::fetch(payload.py());
            return Err(PyTypeError::new_err(format!(
                "write() on output port {port_name:?} takes the message's payload as a \
                 contiguous buffer such as bytes, a memoryview or a numpy array; got a {} \
                 ({refusal}). Encode a record with `tatolab.stream.msgpack.pack(record)`.",
                python_type_name_for_error_message(payload, "value")
            )));
        }
        // SAFETY: `PyObject_GetBuffer` succeeded, so the view is filled.
        let view = unsafe { Box::from_raw(Box::into_raw(view).cast::<pyo3::ffi::Py_buffer>()) };
        Ok(Self { view })
    }

    /// Hold `array`'s buffer as a C-contiguous array, answering its struct
    /// format string and its extents, or refuse it naming what `what` takes.
    fn of_c_contiguous_array(
        port_name: &str,
        array: &Bound<'_, PyAny>,
        what: &str,
    ) -> PyResult<(Self, String, Vec<u64>)> {
        let mut view = Box::new(std::mem::MaybeUninit::<pyo3::ffi::Py_buffer>::zeroed());
        // SAFETY: as in `of`; the flags ask for the shape and the format too.
        let got = unsafe {
            pyo3::ffi::PyObject_GetBuffer(
                array.as_ptr(),
                view.as_mut_ptr(),
                pyo3::ffi::PyBUF_C_CONTIGUOUS | pyo3::ffi::PyBUF_FORMAT,
            )
        };
        if got != 0 {
            let refusal = PyErr::fetch(array.py());
            return Err(PyTypeError::new_err(format!(
                "a write on output port {port_name:?} takes {what}; got a {} ({refusal})",
                python_type_name_for_error_message(array, "value")
            )));
        }
        // SAFETY: `PyObject_GetBuffer` succeeded, so the view is filled.
        let held = Self {
            view: unsafe { Box::from_raw(Box::into_raw(view).cast::<pyo3::ffi::Py_buffer>()) },
        };
        let format = if held.view.format.is_null() {
            "B".to_string()
        } else {
            // SAFETY: a filled view's non-null `format` is a NUL-terminated struct string.
            unsafe { std::ffi::CStr::from_ptr(held.view.format) }
                .to_string_lossy()
                .trim_start_matches(['@', '=', '<'])
                .to_string()
        };
        let extents = if held.view.ndim == 0 {
            Vec::new()
        } else if held.view.shape.is_null() {
            vec![held.view.len as u64]
        } else {
            // SAFETY: a view asked for its shape holds `ndim` extents at `shape`.
            unsafe { std::slice::from_raw_parts(held.view.shape, held.view.ndim as usize) }
                .iter()
                .map(|&extent| extent as u64)
                .collect()
        };
        Ok((held, format, extents))
    }

    /// Hold `samples`' buffer as a C-contiguous `[channel][sample]` array of
    /// `float32` or `float64`, or refuse it naming what it got.
    fn of_planar_channel_samples(
        port_name: &str,
        samples: &Bound<'_, PyAny>,
    ) -> PyResult<(Self, ChannelsBlockSampleType, u32, u32)> {
        let what = "a C-contiguous [channel][sample] float32 or float64 array";
        let (held, format, extents) = Self::of_c_contiguous_array(port_name, samples, what)?;
        let sample_type = match format.as_str() {
            "f" => ChannelsBlockSampleType::F32,
            "d" => ChannelsBlockSampleType::F64,
            other => {
                return Err(PyTypeError::new_err(format!(
                    "a write on Channels output port {port_name:?} takes {what}; got buffer \
                     format {other:?}"
                )));
            }
        };
        let [channel_count, samples_per_channel] = extents[..] else {
            return Err(PyValueError::new_err(format!(
                "a write on Channels output port {port_name:?} takes {what}; got {} dimensions",
                extents.len()
            )));
        };
        let extent = |extent: u64| {
            u32::try_from(extent).map_err(|_| {
                PyValueError::new_err(format!(
                    "a write on Channels output port {port_name:?}: an extent of {extent} is \
                     past what a Channels block states"
                ))
            })
        };
        Ok((
            held,
            sample_type,
            extent(channel_count)?,
            extent(samples_per_channel)?,
        ))
    }

    fn bytes(&self) -> &[u8] {
        if self.view.len == 0 {
            return &[];
        }
        // SAFETY: a `PyBUF_SIMPLE` view is `len` contiguous bytes at `buf`,
        // valid until the view is released.
        unsafe { std::slice::from_raw_parts(self.view.buf.cast::<u8>(), self.view.len as usize) }
    }
}

impl Drop for PythonBufferHeldForOneWrite {
    fn drop(&mut self) {
        // SAFETY: the view was filled by `PyObject_GetBuffer` and is released
        // once, attached.
        Python::attach(|_| unsafe { pyo3::ffi::PyBuffer_Release(&mut *self.view) });
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
        "this processor's links belong to the engine that wired them, so they cannot be wired \
         again from Python — only a helper process wires its own",
    )
}

/// The declared port an author's spelling names: `@node` cast each declared
/// name, so a lookup casts its argument and `read("Video")` finds `video`.
fn declared_port_name_the_spelling_names(port_name: &str) -> PyResult<Cow<'_, str>> {
    cast_exposed_name_to_url_safe(port_name)
        .map_err(|names_no_port| PyValueError::new_err(names_no_port.to_string()))
}

fn unwired_port_error(direction: &str, port_name: &str) -> PyErr {
    PyRuntimeError::new_err(format!(
        "{direction} port {port_name:?} is not one this processor declared, so no link \
         can reach it"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::python_bag_conversion::records_message_payload_of;
    use pyo3::types::{PyBytes, PyDict, PyString};

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

    /// A source and a destination plane joined by one link, `frames_to_downstream`
    /// into `frames_from_upstream`.
    ///
    /// The destination subscribes first: iceoryx2 drops a send with no
    /// subscriber attached, so wiring the publisher first would race.
    fn one_wired_link(
        python: Python<'_>,
        label: &str,
    ) -> (PythonProcessorLinkDataAccess, PythonProcessorLinkDataAccess) {
        let (channel, notify) = unique_channel_names(label);
        let source = helper_plane();
        let destination = helper_plane();
        destination
            .wire_input_link(
                python,
                "frames_from_upstream",
                &channel,
                &channel,
                &notify,
                "read_next_in_order",
                8,
                8,
                2,
                1,
                "link-1",
                None,
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
                None,
            )
            .unwrap();
        (source, destination)
    }

    /// The whole point of the wiring surface: a helper process opens its own
    /// publisher and its own subscriber on the services the parent named, and a
    /// message written on one shows up on the other, its payload the same bytes.
    /// Both planes live in this test's thread because iceoryx2's ports are
    /// `!Send`.
    #[test]
    fn a_message_written_on_a_wired_output_arrives_on_a_wired_input_as_the_same_bytes() {
        Python::initialize();
        Python::attach(|python| {
            let (source, destination) = one_wired_link(python, "roundtrip");
            let payload_that_is_not_msgpack: &[u8] = b"\xc1 raw bytes, never decoded";

            source
                .write_to_output_port(
                    python,
                    "frames_to_downstream",
                    PyBytes::new(python, payload_that_is_not_msgpack).as_any(),
                    None,
                    true,
                )
                .unwrap();

            let received = destination
                .read_from_input_port(python, "frames_from_upstream", None)
                .unwrap()
                .expect("the wired input received nothing");
            assert_eq!(
                PythonPortPayloadBorrow::bytes_the_read_borrowed(&received),
                payload_that_is_not_msgpack
            );
        });
    }

    /// The next message on the one-link fixture's input, read as its family's type.
    fn read_as_the_family_type<'py>(
        python: Python<'py>,
        destination: &PythonProcessorLinkDataAccess,
        family: PortFamilyOfTheRead,
    ) -> Bound<'py, PyAny> {
        destination
            .read_from_input_port_offering_gpu_access(
                python,
                "frames_from_upstream",
                PortPayloadReadShape::FamilyType(family),
                None,
            )
            .unwrap()
            .expect("the wired input received nothing")
    }

    /// A `[channel][sample]` array written as a Channels block reads back as a
    /// `ChannelsBlock` stating its shape, rate and first sample through a port
    /// attribute, and as the raw borrow by name.
    #[test]
    fn a_channels_block_written_from_a_planar_array_reads_back_as_a_channels_block() {
        Python::initialize();
        Python::attach(|python| {
            let (source, destination) = one_wired_link(python, "channels-block");
            let samples = python
                .eval(
                    c"memoryview(__import__('array').array('f', [0.0, 0.5, 1.0, -1.0, -0.5, 0.25])).cast('B').cast('f', [2, 3])",
                    None,
                    None,
                )
                .unwrap();
            for _ in 0..2 {
                source
                    .write_channels_block_to_output_port(
                        python,
                        "frames_to_downstream",
                        &samples,
                        96_000,
                        48_000,
                        ChannelsOutputPortDeclaredShape::default(),
                        None,
                        true,
                    )
                    .unwrap();
            }

            let received =
                read_as_the_family_type(python, &destination, PortFamilyOfTheRead::Channels);
            assert!(received.cast::<PythonChannelsBlock>().is_ok());
            let attribute = |name: &str| received.getattr(name).unwrap();
            assert_eq!(attribute("channel_count").extract::<u32>().unwrap(), 2);
            assert_eq!(
                attribute("samples_per_channel").extract::<u32>().unwrap(),
                3
            );
            assert_eq!(attribute("sample_rate").extract::<u32>().unwrap(), 48_000);
            assert_eq!(attribute("start_sample").extract::<u64>().unwrap(), 96_000);
            assert_eq!(attribute("dtype").extract::<String>().unwrap(), "f32");

            let by_name = destination
                .read_from_input_port(python, "frames_from_upstream", None)
                .unwrap()
                .expect("the second block arrived");
            assert_eq!(
                PythonPortPayloadBorrow::bytes_the_read_borrowed(&by_name).len(),
                2 * 3 * 4,
                "a read by name hands back the raw borrow"
            );

            let one_dimensional = python
                .eval(c"__import__('array').array('f', [0.0, 1.0])", None, None)
                .unwrap();
            let refusal = source
                .write_channels_block_to_output_port(
                    python,
                    "frames_to_downstream",
                    &one_dimensional,
                    0,
                    48_000,
                    ChannelsOutputPortDeclaredShape::default(),
                    None,
                    true,
                )
                .expect_err("a 1-D array is not [channel][sample]");
            assert!(
                refusal.to_string().contains("got 1 dimensions"),
                "{refusal}"
            );
        });
    }

    /// A dict written as a Records message reads back as the same dict, and a
    /// dict missing a declared key is refused naming it.
    #[test]
    fn a_records_message_round_trips_a_dict_and_refuses_a_missing_declared_key() {
        Python::initialize();
        Python::attach(|python| {
            let (source, destination) = one_wired_link(python, "records-message");
            let record = python
                .eval(
                    c"{'kind': 'tap', 'at': 3, 'extra': [1.5, None]}",
                    None,
                    None,
                )
                .unwrap();
            let declared_names = vec!["kind".to_string(), "at".to_string()];
            let encoded =
                records_message_payload_of("frames_to_downstream", &record, &declared_names)
                    .unwrap();
            source
                .write_described_by(
                    python,
                    "frames_to_downstream",
                    PyBytes::new(python, &encoded).as_any(),
                    None,
                    true,
                    |_| Ok(DataChannelPayloadDescriptor::records_message()),
                )
                .unwrap();

            let received =
                read_as_the_family_type(python, &destination, PortFamilyOfTheRead::Records);
            assert!(received.eq(&record).unwrap(), "got {received}");

            let missing_at = python.eval(c"{'kind': 'tap'}", None, None).unwrap();
            let refusal =
                records_message_payload_of("frames_to_downstream", &missing_at, &declared_names)
                    .expect_err("`at` is declared");
            assert!(refusal.to_string().contains("\"at\""), "{refusal}");
        });
    }

    /// The stream distribution's source root on `sys.path`, where the family
    /// types a read builds live; a `cargo test` run has no installed
    /// `tatolab-stream`.
    fn put_the_stream_distribution_source_on_sys_path(python: Python<'_>) {
        let stream_distribution_source_root =
            concat!(env!("CARGO_MANIFEST_DIR"), "/../tatolab-stream");
        let sys_path = python.import("sys").unwrap().getattr("path").unwrap();
        if !sys_path.contains(stream_distribution_source_root).unwrap() {
            sys_path
                .call_method1("insert", (0, stream_distribution_source_root))
                .unwrap();
        }
    }

    /// Lend this interpreter the one runtime callable a family type's claim
    /// reaches, as the runtime lends `tatolab.runtime._engine` to a node's.
    #[cfg(target_os = "linux")]
    fn lend_the_typed_read_gpu_offer_to_this_interpreter(python: Python<'_>) {
        let lent_engine = pyo3::types::PyModule::new(python, "_engine").unwrap();
        lent_engine
            .add_function(
                pyo3::wrap_pyfunction!(
                    crate::python_bag_conversion::gpu_limited_access_of_the_typed_read_in_progress,
                    &lent_engine
                )
                .unwrap(),
            )
            .unwrap();
        let sys_modules = python.import("sys").unwrap().getattr("modules").unwrap();
        sys_modules
            .set_item(
                "tatolab.runtime",
                pyo3::types::PyModule::new(python, "tatolab.runtime").unwrap(),
            )
            .unwrap();
        sys_modules
            .set_item("tatolab.runtime._engine", lent_engine)
            .unwrap();
    }

    /// Take back what [`lend_the_typed_read_gpu_offer_to_this_interpreter`]
    /// lent, so later tests in this interpreter see no runtime lent.
    #[cfg(target_os = "linux")]
    fn withdraw_the_lent_typed_read_gpu_offer(python: Python<'_>) {
        python
            .run(
                c"import sys\n\
                  sys.modules.pop('tatolab.runtime._engine', None)\n\
                  sys.modules.pop('tatolab.runtime', None)\n\
                  import tatolab.stream._runtime_lend as lend\n\
                  lend._native_callables_resolved_by_name.clear()\n",
                None,
                None,
            )
            .unwrap();
    }

    /// An Array frame of CPU attributes reads back as a mapping of name to a
    /// tensor borrowed in the sample, each stating its dtype and shape; a
    /// write missing a declared name is refused naming it.
    #[test]
    fn an_array_frame_of_cpu_attributes_reads_back_as_borrowed_tensors_by_name() {
        Python::initialize();
        Python::attach(|python| {
            put_the_stream_distribution_source_on_sys_path(python);
            let (source, destination) = one_wired_link(python, "array-frame");
            let no_gpu = PythonGpuContextLimitedAccess::new_for_helper_process(None);
            let attributes = python
                .eval(
                    c"{'boxes': memoryview(bytes(range(24))).cast('f', [2, 3]), \
                       'labels': memoryview(bytes([7, 8, 9]))}",
                    None,
                    None,
                )
                .unwrap();
            source
                .write_array_frame_to_output_port(
                    python,
                    "frames_to_downstream",
                    &attributes,
                    &["boxes".to_string()],
                    None,
                    &no_gpu,
                    Some(42),
                    true,
                )
                .unwrap();

            let received =
                read_as_the_family_type(python, &destination, PortFamilyOfTheRead::Array);
            assert_eq!(received.len().unwrap(), 2);
            assert_eq!(
                received
                    .getattr("timestamp_ns")
                    .unwrap()
                    .extract::<i64>()
                    .unwrap(),
                42
            );
            for (name, dtype, shape, expected_bytes) in [
                (
                    "boxes",
                    "float32",
                    vec![2u64, 3],
                    (0u8..24).collect::<Vec<_>>(),
                ),
                ("labels", "uint8", vec![3], vec![7, 8, 9]),
            ] {
                let attribute = received.get_item(name).unwrap();
                let attribute = attribute
                    .cast::<PythonArrayFrameCpuAttribute>()
                    .expect("a CPU attribute reads as an ArrayFrameCpuAttribute");
                assert_eq!(attribute.get().dtype(), dtype);
                assert_eq!(attribute.get().shape(), shape);
                assert_eq!(
                    attribute.get().attribute_bytes(),
                    &expected_bytes[..],
                    "{name}"
                );
            }

            source
                .write_array_frame_to_output_port(
                    python,
                    "frames_to_downstream",
                    &received,
                    &["boxes".to_string()],
                    None,
                    &no_gpu,
                    Some(43),
                    true,
                )
                .expect("a read Array frame relays unchanged");
            let relayed = read_as_the_family_type(python, &destination, PortFamilyOfTheRead::Array);
            let relayed_boxes = relayed.get_item("boxes").unwrap();
            let relayed_boxes = relayed_boxes
                .cast::<PythonArrayFrameCpuAttribute>()
                .unwrap();
            assert_eq!(relayed_boxes.get().shape(), vec![2u64, 3]);
            assert_eq!(
                relayed_boxes.get().attribute_bytes(),
                &(0u8..24).collect::<Vec<_>>()[..]
            );

            let residency_refusal = source
                .write_array_frame_to_output_port(
                    python,
                    "frames_to_downstream",
                    &attributes,
                    &[],
                    Some(ArrayOutputPortDeclaredResidency::Gpu),
                    &no_gpu,
                    None,
                    true,
                )
                .expect_err("a CPU attribute is refused on a GPU-resident port");
            assert!(
                residency_refusal.to_string().contains("residency=\"gpu\""),
                "{residency_refusal}"
            );

            let missing_boxes = python
                .eval(c"{'labels': memoryview(bytes([1]))}", None, None)
                .unwrap();
            let refusal = source
                .write_array_frame_to_output_port(
                    python,
                    "frames_to_downstream",
                    &missing_boxes,
                    &["boxes".to_string()],
                    None,
                    &no_gpu,
                    None,
                    true,
                )
                .expect_err("`boxes` is declared");
            assert!(refusal.to_string().contains("\"boxes\""), "{refusal}");

            let float64 = python
                .eval(c"{'boxes': memoryview(bytes(8)).cast('d')}", None, None)
                .unwrap();
            let refusal = source
                .write_array_frame_to_output_port(
                    python,
                    "frames_to_downstream",
                    &float64,
                    &[],
                    None,
                    &no_gpu,
                    None,
                    true,
                )
                .expect_err("float64 is no tensor element type");
            assert!(refusal.to_string().contains("float32"), "{refusal}");
        });
    }

    /// An Image frame over a claim-tabled pool slot reads back as an
    /// `ImageFrame` naming the slot's published frame, its size, format and
    /// colour, holding the slot while it lives; written on, it publishes the
    /// same frame again. Needs a Vulkan device; skips where none comes up.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_image_frame_reads_back_claimed_and_writes_on_as_the_same_frame() {
        use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;
        use streamlib::sdk::context::{GpuPoolClaimTablesOfOnePool, SurfaceStore};
        use streamlib::sdk::engine::host_rhi::{HostVulkanBuffer, HostVulkanDevice};
        use streamlib::sdk::rhi::{PixelBuffer, PixelFormat};

        const POOL_SLOT_KEY: &str = "image-frame-round-trip-slot";
        let host_device = match HostVulkanDevice::new() {
            Ok(host_device) => host_device,
            Err(unavailable) => {
                tracing::warn!("skipping: no host Vulkan device ({unavailable})");
                return;
            }
        };
        let exported = match HostVulkanBuffer::new_opaque_fd_export(&host_device, 64 * 4 * 16) {
            Ok(exported) => exported,
            Err(unavailable) => {
                tracing::warn!(
                    "skipping: no OPAQUE_FD buffer export on this device ({unavailable})"
                );
                return;
            }
        };
        let pool_slot = PixelBuffer::from_host_vulkan_buffer(
            Arc::new(exported),
            64,
            16,
            4,
            PixelFormat::Bgra32,
        );
        let share = SurfaceShareUnderTest::start("image-frame-round-trip");
        let engine_surface_store = SurfaceStore::new(
            share.socket_path.to_string_lossy().into_owned(),
            "engine:image-frame-round-trip".to_string(),
        );
        engine_surface_store
            .connect()
            .expect("the engine's store connects");
        engine_surface_store
            .register_buffer(POOL_SLOT_KEY, &pool_slot)
            .expect("the engine registers the pool slot");
        let mut claim_tables_of_the_pool =
            GpuPoolClaimTablesOfOnePool::named("image-frame-round-trip");
        let row_lease = claim_tables_of_the_pool
            .lease_a_row_for_a_slot(
                share.check_out_leases().gpu_pool_claim_tables(),
                POOL_SLOT_KEY,
            )
            .expect("the pool leases its slot a row");
        row_lease.row().publish_generation(1);
        share.publish_frame_generation_over_a_registered_pool_slot(POOL_SLOT_KEY, 1);
        let exchange_client = share.exchange_client_labelled_importing_on(
            "image-frame-round-trip",
            Ok(host_device.physical_device_uuid()),
        );

        Python::initialize();
        Python::attach(|python| {
            put_the_stream_distribution_source_on_sys_path(python);
            lend_the_typed_read_gpu_offer_to_this_interpreter(python);
            let gpu = Bound::new(
                python,
                PythonGpuContextLimitedAccess::new_for_helper_process(Some(exchange_client)),
            )
            .unwrap();
            let image_frame = ImageFrameDescriptorBody {
                slot: GpuPoolSlotFrameDescriptorBody {
                    pool: row_lease.table().pool_uuid(),
                    slot: row_lease.row_index(),
                    generation: 1,
                    timeline_value: 0,
                    image_layout: 0,
                },
                width: 64,
                height: 16,
                format: TextureFormat::Bgra8Unorm as u32,
                colour: ImageFrameH273Colour {
                    primaries: 1,
                    transfer: 13,
                    matrix: 1,
                    range: 1,
                },
            };
            let read_image_frame_on = |destination: &PythonProcessorLinkDataAccess| {
                destination
                    .read_from_input_port_offering_gpu_access(
                        python,
                        "frames_from_upstream",
                        PortPayloadReadShape::FamilyType(PortFamilyOfTheRead::Image),
                        Some(&gpu),
                    )
                    .unwrap()
                    .expect("the wired input received nothing")
            };
            let (source, destination) = one_wired_link(python, "image-frame-read");
            source
                .write_described_by(
                    python,
                    "frames_to_downstream",
                    PyBytes::new(python, &[]).as_any(),
                    Some(7),
                    true,
                    |_| Ok(DataChannelPayloadDescriptor::image_frame(image_frame)),
                )
                .unwrap();
            let frame = read_image_frame_on(&destination);
            let field = |name: &str| frame.getattr(name).unwrap();
            assert_eq!(
                field("surface_id").extract::<String>().unwrap(),
                format!("{POOL_SLOT_KEY}#1")
            );
            assert_eq!(field("width").extract::<u32>().unwrap(), 64);
            assert_eq!(field("height").extract::<u32>().unwrap(), 16);
            assert_eq!(field("format").extract::<String>().unwrap(), "bgra8_unorm");
            assert_eq!(field("timestamp_ns").extract::<i64>().unwrap(), 7);
            let colour = field("colour");
            for (axis, name) in [
                ("primaries", "bt709"),
                ("transfer", "srgb"),
                ("matrix", "bt709"),
                ("range", "full"),
            ] {
                assert_eq!(
                    colour.getattr(axis).unwrap().extract::<String>().unwrap(),
                    name
                );
            }
            assert!(row_lease.row().is_claimed(), "a read frame holds its slot");

            let (source_on, destination_on) = one_wired_link(python, "image-frame-write-on");
            let format_refusal = source_on
                .write_image_frame_to_output_port(
                    python,
                    "frames_to_downstream",
                    &frame,
                    &["nv12".to_string()],
                    gpu.get(),
                    None,
                    true,
                )
                .expect_err("the frame's format is not the one the port declares");
            assert!(
                format_refusal.to_string().contains("formats="),
                "{format_refusal}"
            );
            source_on
                .write_image_frame_to_output_port(
                    python,
                    "frames_to_downstream",
                    &frame,
                    &[frame
                        .getattr("format")
                        .unwrap()
                        .extract::<String>()
                        .unwrap()],
                    gpu.get(),
                    None,
                    true,
                )
                .unwrap();
            let frame_written_on = read_image_frame_on(&destination_on);
            for name in ["surface_id", "width", "height", "format", "colour"] {
                assert!(
                    frame_written_on
                        .getattr(name)
                        .unwrap()
                        .eq(frame.getattr(name).unwrap())
                        .unwrap(),
                    "{name} survives the write on"
                );
            }
            drop((frame, frame_written_on));
            python.run(c"import gc; gc.collect()", None, None).unwrap();
            withdraw_the_lent_typed_read_gpu_offer(python);
        });
        assert!(
            !row_lease.row().is_claimed(),
            "a dropped frame lets its slot go"
        );
    }

    /// `ImageToArray` borrows an Image frame's pool slot as its one Array
    /// attribute: the attribute a reader claims exports the very address the
    /// Image frame itself does, the slot's mapping, with no copy between.
    /// Needs a Vulkan device; skips where none comes up.
    #[cfg(target_os = "linux")]
    #[test]
    fn image_to_array_borrows_the_frames_own_slot_with_no_copy() {
        use crate::python_surface_share_service_for_tests::SurfaceShareUnderTest;
        use streamlib::sdk::context::{GpuPoolClaimTablesOfOnePool, SurfaceStore};
        use streamlib::sdk::engine::host_rhi::{HostVulkanBuffer, HostVulkanDevice};
        use streamlib::sdk::rhi::{PixelBuffer, PixelFormat};
        use streamlib_media_builtins::port_family_conversion::image_to_array::array_frame_of_the_image_frame;

        const POOL_SLOT_KEY: &str = "image-to-array-slot";
        let host_device = match HostVulkanDevice::new() {
            Ok(host_device) => host_device,
            Err(unavailable) => {
                tracing::warn!("skipping: no host Vulkan device ({unavailable})");
                return;
            }
        };
        let exported = match HostVulkanBuffer::new_opaque_fd_export(&host_device, 64 * 4 * 16) {
            Ok(exported) => exported,
            Err(unavailable) => {
                tracing::warn!(
                    "skipping: no OPAQUE_FD buffer export on this device ({unavailable})"
                );
                return;
            }
        };
        let pool_slot = PixelBuffer::from_host_vulkan_buffer(
            Arc::new(exported),
            64,
            16,
            4,
            PixelFormat::Bgra32,
        );
        let share = SurfaceShareUnderTest::start("image-to-array");
        let engine_surface_store = SurfaceStore::new(
            share.socket_path.to_string_lossy().into_owned(),
            "engine:image-to-array".to_string(),
        );
        engine_surface_store
            .connect()
            .expect("the engine's store connects");
        engine_surface_store
            .register_buffer(POOL_SLOT_KEY, &pool_slot)
            .expect("the engine registers the pool slot");
        let mut claim_tables_of_the_pool = GpuPoolClaimTablesOfOnePool::named("image-to-array");
        let row_lease = claim_tables_of_the_pool
            .lease_a_row_for_a_slot(
                share.check_out_leases().gpu_pool_claim_tables(),
                POOL_SLOT_KEY,
            )
            .expect("the pool leases its slot a row");
        row_lease.row().publish_generation(1);
        share.publish_frame_generation_over_a_registered_pool_slot(POOL_SLOT_KEY, 1);
        let exchange_client = share.exchange_client_labelled_importing_on(
            "image-to-array",
            Ok(host_device.physical_device_uuid()),
        );

        Python::initialize();
        Python::attach(|python| {
            put_the_stream_distribution_source_on_sys_path(python);
            lend_the_typed_read_gpu_offer_to_this_interpreter(python);
            let gpu = Bound::new(
                python,
                PythonGpuContextLimitedAccess::new_for_helper_process(Some(exchange_client)),
            )
            .unwrap();
            let image_frame = DataChannelPayloadDescriptor::image_frame(ImageFrameDescriptorBody {
                slot: GpuPoolSlotFrameDescriptorBody {
                    pool: row_lease.table().pool_uuid(),
                    slot: row_lease.row_index(),
                    generation: 1,
                    timeline_value: 0,
                    image_layout: 0,
                },
                width: 64,
                height: 16,
                format: TextureFormat::Bgra8Unorm as u32,
                colour: ImageFrameH273Colour {
                    primaries: 1,
                    transfer: 13,
                    matrix: 1,
                    range: 1,
                },
            });
            let (array_descriptor, array_payload) =
                array_frame_of_the_image_frame(image_frame).expect("a bgra8 frame converts");
            let data_address_of = |tensor: &Bound<'_, PyAny>| {
                let capsule = tensor.call_method0("__dlpack__").unwrap();
                // SAFETY: an unconsumed `__dlpack__` capsule carries a
                // `ManagedTensor` under the name `dltensor`.
                unsafe {
                    let managed_tensor =
                        pyo3::ffi::PyCapsule_GetPointer(capsule.as_ptr(), c"dltensor".as_ptr())
                            as *const streamlib_adapter_cuda::dlpack::ManagedTensor;
                    (*managed_tensor).dl_tensor.data as usize
                }
            };

            let (image_source, image_destination) = one_wired_link(python, "image-to-array-image");
            image_source
                .write_described_by(
                    python,
                    "frames_to_downstream",
                    PyBytes::new(python, &[]).as_any(),
                    Some(7),
                    true,
                    |_| Ok(image_frame),
                )
                .unwrap();
            let read_image = image_destination
                .read_from_input_port_offering_gpu_access(
                    python,
                    "frames_from_upstream",
                    PortPayloadReadShape::FamilyType(PortFamilyOfTheRead::Image),
                    Some(&gpu),
                )
                .unwrap()
                .expect("the image link received nothing");
            let image_data_address = data_address_of(&read_image);

            let (array_source, array_destination) = one_wired_link(python, "image-to-array-array");
            array_source
                .write_described_by(
                    python,
                    "frames_to_downstream",
                    PyBytes::new(python, &array_payload).as_any(),
                    Some(7),
                    true,
                    |_| Ok(array_descriptor),
                )
                .unwrap();
            let read_array = array_destination
                .read_from_input_port_offering_gpu_access(
                    python,
                    "frames_from_upstream",
                    PortPayloadReadShape::FamilyType(PortFamilyOfTheRead::Array),
                    Some(&gpu),
                )
                .unwrap()
                .expect("the array link received nothing");
            let image_attribute = read_array.get_item("image").unwrap();
            assert_eq!(
                image_attribute
                    .getattr("dtype")
                    .unwrap()
                    .extract::<String>()
                    .unwrap(),
                "uint8"
            );
            assert_eq!(
                image_attribute
                    .getattr("shape")
                    .unwrap()
                    .extract::<Vec<u64>>()
                    .unwrap(),
                vec![16, 64, 4]
            );
            let array_data_address = data_address_of(&image_attribute);

            assert_ne!(image_data_address, 0);
            assert_eq!(
                array_data_address, image_data_address,
                "the attribute is the frame's slot mapping, not a copy of it"
            );
            assert!(
                row_lease.row().is_claimed(),
                "the read attribute holds its slot"
            );
            drop((read_image, image_attribute, read_array));
            python.run(c"import gc; gc.collect()", None, None).unwrap();
            withdraw_the_lent_typed_read_gpu_offer(python);
        });
        assert!(
            !row_lease.row().is_claimed(),
            "dropped reads let the slot go"
        );
    }

    /// A read through a port attribute refuses a message whose descriptor
    /// kind is not its family's, naming both.
    #[test]
    fn a_family_read_refuses_a_message_described_as_another_kind() {
        Python::initialize();
        Python::attach(|python| {
            let (source, destination) = one_wired_link(python, "family-kind-mismatch");
            source
                .write_described_by(
                    python,
                    "frames_to_downstream",
                    PyBytes::new(python, b"written by name").as_any(),
                    None,
                    true,
                    |_| Ok(DataChannelPayloadDescriptor::bytes()),
                )
                .unwrap();
            let refusal = destination
                .read_from_input_port_offering_gpu_access(
                    python,
                    "frames_from_upstream",
                    PortPayloadReadShape::FamilyType(PortFamilyOfTheRead::Records),
                    None,
                )
                .expect_err("a Bytes message is no Records message");
            assert!(refusal.to_string().contains("Records"), "{refusal}");
            assert!(refusal.to_string().contains("Bytes"), "{refusal}");
        });
    }

    /// A write is held to what its port declares: an Array attribute with a
    /// zero extent, and a Channels block of the wrong sample type or channel
    /// count, are refused before anything is published.
    #[test]
    fn a_write_refuses_an_array_attribute_with_no_elements_and_a_channels_block_off_its_declaration()
     {
        Python::initialize();
        Python::attach(|python| {
            let (source, _destination) = one_wired_link(python, "declared-shape-refusals");
            let no_gpu = PythonGpuContextLimitedAccess::new_for_helper_process(None);
            let empty_boxes = python
                .eval(c"{'boxes': memoryview(bytes(0)).cast('f')}", None, None)
                .unwrap();
            let refusal = source
                .write_array_frame_to_output_port(
                    python,
                    "frames_to_downstream",
                    &empty_boxes,
                    &[],
                    None,
                    &no_gpu,
                    None,
                    true,
                )
                .expect_err("a zero extent describes no memory");
            assert!(refusal.to_string().contains("zero dimension"), "{refusal}");

            let stereo_f32 = python
                .eval(c"memoryview(bytes(16)).cast('f', [2, 2])", None, None)
                .unwrap();
            for (declared_shape, expected) in [
                (
                    ChannelsOutputPortDeclaredShape {
                        sample_type: Some(ChannelsBlockSampleType::F64),
                        channel_count: None,
                    },
                    "F64",
                ),
                (
                    ChannelsOutputPortDeclaredShape {
                        sample_type: None,
                        channel_count: Some(1),
                    },
                    "1 names",
                ),
            ] {
                let refusal = source
                    .write_channels_block_to_output_port(
                        python,
                        "frames_to_downstream",
                        &stereo_f32,
                        0,
                        48_000,
                        declared_shape,
                        None,
                        true,
                    )
                    .expect_err("the block is off the port's declaration");
                assert!(refusal.to_string().contains(expected), "{refusal}");
            }
        });
    }

    /// A Packets unit reads back with its bytes, its stamp, whether a decoder
    /// may start at it and its codec; a plain byte stream names no codec.
    #[test]
    fn a_packets_unit_reads_back_with_its_stamp_group_flag_and_codec() {
        Python::initialize();
        Python::attach(|python| {
            let (source, destination) = one_wired_link(python, "packets-unit");
            for (codec, starts_group) in [("h264", false), ("", true)] {
                let descriptor =
                    DataChannelPayloadDescriptor::packets_unit(&PacketsUnitDescriptorBody {
                        codec: codec.to_string(),
                    })
                    .unwrap();
                source
                    .write_described_by(
                        python,
                        "frames_to_downstream",
                        PyBytes::new(python, b"\x00\x00\x01").as_any(),
                        Some(42),
                        starts_group,
                        |_| Ok(descriptor),
                    )
                    .unwrap();
            }
            for (codec, starts_group) in [("h264", false), ("", true)] {
                let unit =
                    read_as_the_family_type(python, &destination, PortFamilyOfTheRead::Packets);
                assert!(unit.cast::<PythonPacketsUnit>().is_ok());
                assert_eq!(
                    unit.getattr("codec").unwrap().extract::<String>().unwrap(),
                    codec
                );
                assert_eq!(unit.getattr("pts").unwrap().extract::<i64>().unwrap(), 42);
                assert_eq!(
                    unit.getattr("starts_group")
                        .unwrap()
                        .extract::<bool>()
                        .unwrap(),
                    starts_group
                );
                assert_eq!(
                    PythonPortPayloadBorrow::bytes_the_read_borrowed(
                        &unit.getattr("payload").unwrap()
                    ),
                    b"\x00\x00\x01"
                );
            }
        });
    }

    /// Groups an engine-internal writer declared given up upstream reach a
    /// helper's reader as `lost_since_last_read` on the next message, through
    /// the same mailboxes a native reader runs.
    #[test]
    fn a_loss_declared_upstream_of_a_link_reaches_a_helpers_reader_as_lost_since_last_read() {
        Python::initialize();
        Python::attach(|python| {
            let (source, destination) = one_wired_link(python, "declared-upstream-loss");
            source
                .output_writer
                .get()
                .expect("the source wired an output")
                .declare_groups_given_up_upstream_before_the_next_write("frames_to_downstream", 1)
                .unwrap();
            source
                .write_to_output_port(
                    python,
                    "frames_to_downstream",
                    PyBytes::new(python, b"after the given-up group").as_any(),
                    None,
                    true,
                )
                .unwrap();

            let (_, message, _) = destination
                .read_message_naming_its_inbound_link_with_its_payload_cast(
                    python,
                    "frames_from_upstream",
                    PortPayloadReadShape::RawBorrow,
                    None,
                )
                .unwrap()
                .expect("the wired input received nothing");
            assert_eq!(message.lost_since_last_read, 1);
        });
    }

    /// `starts_group` lands in the frame header's group flag, which a Rust
    /// reader on the far side of the link observes on the message it reads,
    /// beside the stamp the write gave it.
    #[test]
    fn the_group_flag_and_the_stamp_a_write_names_reach_a_rust_reader() {
        Python::initialize();
        Python::attach(|python| {
            let (source, destination) = one_wired_link(python, "groupflag");
            for (timestamp_ns, starts_group) in [(11i64, false), (12i64, true)] {
                source
                    .write_to_output_port(
                        python,
                        "frames_to_downstream",
                        PyBytes::new(python, b"access unit").as_any(),
                        Some(timestamp_ns),
                        starts_group,
                    )
                    .unwrap();
            }

            let input_mailboxes = destination
                .input_mailboxes
                .get()
                .expect("a wired input plane holds its mailboxes");
            for (timestamp_ns, group_boundary) in [
                (11i64, MessageGroupBoundary::ContinuesGroup),
                (12i64, MessageGroupBoundary::StartsGroup),
            ] {
                let message = input_mailboxes
                    .read_raw("frames_from_upstream")
                    .unwrap()
                    .expect("the wired input received nothing");
                assert_eq!(message.payload, b"access unit");
                assert_eq!(message.timestamp_ns, timestamp_ns);
                assert_eq!(message.group_boundary, group_boundary);
            }
        });
    }

    /// A payload that exports no buffer is refused at the write, naming the
    /// buffer it wants and the type it got — and refused on a declared port no
    /// link has reached, too, so an author learns before the first link
    /// arrives.
    #[test]
    fn a_write_refuses_a_payload_that_is_not_a_buffer_by_name() {
        Python::initialize();
        Python::attach(|python| {
            let (source, destination) = one_wired_link(python, "refusal");
            source.declare_ports(vec![], vec!["unlinked_output".to_string()]);
            let record = PyDict::new(python);
            record.set_item("frame_index", 7i64).unwrap();
            let not_bytes = [
                ("dict", record.into_any()),
                ("str", PyString::new(python, "frame").into_any()),
                ("int", 7i64.into_pyobject(python).unwrap().into_any()),
            ];
            for port_name in ["frames_to_downstream", "unlinked_output"] {
                for (type_name, payload) in &not_bytes {
                    let refusal = source
                        .write_to_output_port(python, port_name, payload, None, true)
                        .expect_err("a payload that is not a buffer must be refused");
                    assert!(
                        refusal.is_instance_of::<PyTypeError>(python),
                        "got: {refusal}"
                    );
                    let refusal_text = refusal.to_string();
                    assert!(
                        refusal_text.contains("contiguous buffer")
                            && refusal_text.contains(&format!("got a {type_name}")),
                        "the refusal names the buffer and what it got: {refusal_text}"
                    );
                }
            }
            assert!(
                destination
                    .read_from_input_port(python, "frames_from_upstream", None)
                    .unwrap()
                    .is_none(),
                "a refused write publishes nothing"
            );
        });
    }

    /// A helper's egress crosses the subprocess boundary by construction, so the
    /// tier it labels a ceiling refusal with is the one the parent classified
    /// this channel under when it chose the ceiling the envelope carries.
    #[test]
    fn a_ceiling_refusal_on_a_helpers_egress_names_the_untrusted_session_tier() {
        Python::initialize();
        Python::attach(|python| {
            let (channel, notify) = unique_channel_names("egress-tier");
            let ceiling_bytes = 1 << 20;
            let source = helper_plane();
            source
                .wire_output_link(
                    python,
                    "frames_to_downstream",
                    &channel,
                    &notify,
                    1024,
                    ceiling_bytes,
                    8,
                    2,
                    1,
                    "link-egress-tier",
                    None,
                )
                .unwrap();

            let output_writer = source
                .output_writer
                .get()
                .expect("a wired output plane holds its writer");
            let over_the_ceiling = vec![0u8; ceiling_bytes + 1];
            let refusal = output_writer
                .write_bytes_with_timestamp(
                    "frames_to_downstream",
                    &over_the_ceiling,
                    1,
                    MessageGroupBoundary::StartsGroup,
                )
                .expect_err("a message past the channel ceiling must be refused");

            match refusal {
                Error::PayloadExceedsChannelCeiling { tier, .. } => assert_eq!(
                    tier.to_string(),
                    "untrusted-session",
                    "a helper's egress is an untrusted-session channel"
                ),
                other => panic!("expected a ceiling refusal, got {other:?}"),
            }
        });
    }

    /// A helper's callbacks reach its parent's board as the step ledger reads
    /// them: in flight while one runs, and the step it finished once it returns.
    #[test]
    fn a_helpers_step_callbacks_reach_the_board_its_parent_reads() {
        Python::initialize();
        Python::attach(|python| {
            let board_name = format!("wiring{}_steps/loss-counts", std::process::id());
            let parent_board = Iceoryx2Node::for_this_test_process()
                .create_helper_process_loss_count_board(&board_name, Vec::new())
                .unwrap();
            let helper = helper_plane();
            helper
                .open_loss_count_board(python, &board_name, Vec::new())
                .expect("the helper opens the board its parent named");

            helper.note_a_step_callback_started();
            let in_a_callback = parent_board.step_progress_entry();
            assert_eq!(
                (
                    in_a_callback.callbacks_in_flight,
                    in_a_callback.steps_finished
                ),
                (1, 0)
            );
            helper.note_a_step_callback_returned(Some(2));
            let returned = parent_board.step_progress_entry();
            assert_eq!(
                (returned.callbacks_in_flight, returned.steps_finished),
                (0, 3)
            );
        });
    }

    /// A helper in a stepped stream wakes on its descriptor when the engine
    /// opens a step, and goes quiet again once it took the wake.
    #[test]
    fn a_helper_listening_on_its_stream_clock_wakes_when_a_step_opens() {
        use streamlib::sdk::context::{StreamClock, StreamClockSpec};

        Python::initialize();
        Python::attach(|python| {
            let board_name = format!("wiring{}_steps/stream-clock", std::process::id());
            let clock = StreamClock::starting_at(
                StreamClockSpec::Stepped {
                    step_ns: 1_000_000,
                    pace: None,
                    stop_after_steps: None,
                },
                10,
            );
            let board = Iceoryx2Node::for_this_test_process()
                .create_stream_clock_board(&board_name, clock.published())
                .unwrap();
            let clock = clock.published_on(board);
            let helper = helper_plane();
            assert_eq!(helper.stream_clock_board_publish_listener_fd(), None);
            helper
                .listen_for_each_stream_clock_board_publish(python, &board_name)
                .expect("the helper listens on the board its engine named");
            let publish_listener_fd = helper
                .stream_clock_board_publish_listener_fd()
                .expect("a listening helper has the descriptor to wait on");
            let is_readable_within = |timeout_ms: i32| {
                let mut poll_descriptor = libc::pollfd {
                    fd: publish_listener_fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: one live descriptor, counted as one.
                unsafe { libc::poll(&mut poll_descriptor, 1, timeout_ms) == 1 }
            };
            helper.take_every_stream_clock_board_publish_noted();
            assert!(!is_readable_within(0));

            assert_eq!(clock.open_the_waiting_step(), Some(0));
            assert!(is_readable_within(1_000));
            helper.take_every_stream_clock_board_publish_noted();
            assert!(!is_readable_within(0));
        });
    }

    /// A helper that opened its parent's board mirrors an overrun `ordered`
    /// port's losses onto its link's slot and a refusal at the ceiling onto its
    /// output port's entry, with nothing but the reads and writes the processor
    /// already makes — no loop flush and no timer.
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
                    None,
                )
                .unwrap();
            destination
                .wire_output_link(
                    python,
                    "frames_to_downstream",
                    &format!("{channel}_onward"),
                    &format!("{notify}_onward"),
                    64,
                    1024,
                    8,
                    2,
                    1,
                    "link-onward",
                    Some(31),
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
                    None,
                )
                .unwrap();

            let write_frame = |frame_index: i64| {
                source
                    .write_to_output_port(
                        python,
                        "frames_to_downstream",
                        PyBytes::new(python, &frame_index.to_le_bytes()).as_any(),
                        None,
                        true,
                    )
                    .unwrap();
            };
            let read_frame_index = || {
                let payload = destination
                    .read_from_input_port(python, "frames_from_upstream", None)
                    .unwrap()
                    .expect("the port holds a message");
                i64::from_le_bytes(
                    PythonPortPayloadBorrow::bytes_the_read_borrowed(&payload)
                        .try_into()
                        .expect("an eight-byte frame index"),
                )
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
                "the four messages the eight-deep ring overwrote land on the link's slot at the \
                 read"
            );
            destination
                .count_the_message_last_read_from_input_port_as_lost(python, "frames_from_upstream")
                .expect("the message the link delivered is countable");
            assert_eq!(
                slot_counts(),
                Some((21, 5, 0)),
                "a message the reader counts lost lands on the link's slot as it is counted"
            );

            destination
                .write_to_output_port(
                    python,
                    "frames_to_downstream",
                    PyBytes::new(python, &[0u8; 4096]).as_any(),
                    None,
                    true,
                )
                .expect("a refusal at the ceiling is never raised");
            destination
                .write_to_output_port(
                    python,
                    "frames_to_downstream",
                    PyBytes::new(python, &[0u8; 16]).as_any(),
                    None,
                    true,
                )
                .expect("a message under the ceiling is sent");
            assert_eq!(
                parent_board
                    .output_port_entry("frames_to_downstream")
                    .map(|entry| (
                        entry.wiring_generation,
                        entry.refused_bags,
                        entry.written_bags
                    )),
                Some((31, 1, 1)),
                "the refused message is counted as refused, the sent one as written"
            );

            let second_channel = format!("{channel}_second");
            let refusal = destination
                .wire_input_link(
                    python,
                    "frames_from_upstream",
                    &second_channel,
                    &second_channel,
                    &notify,
                    "read_next_in_order",
                    8,
                    8,
                    2,
                    1,
                    "link-with-no-slot",
                    None,
                    None,
                    None,
                    None,
                )
                .expect_err("a helper with a board open is told where every link's counts go");
            assert!(
                refusal.to_string().contains("link-with-no-slot")
                    && refusal.to_string().contains("loss_count_slot"),
                "the refusal names the link and what it was wired without: {refusal}"
            );
            let refusal = destination
                .wire_output_link(
                    python,
                    "frames_to_downstream",
                    &format!("{channel}_onward_again"),
                    &format!("{notify}_onward_again"),
                    64,
                    1024,
                    8,
                    2,
                    1,
                    "link-with-no-channel-generation",
                    None,
                )
                .expect_err(
                    "a helper with a board open is told which channel a port's refusals belong to",
                );
            assert!(
                refusal
                    .to_string()
                    .contains("link-with-no-channel-generation")
                    && refusal
                        .to_string()
                        .contains("output_port_wiring_generation"),
                "the refusal names the link and what it was wired without: {refusal}"
            );
        });
    }

    /// The window contract crosses the parent→child envelope and the child's
    /// own stage honours it: a 48 kHz stereo source published on the wired
    /// output as msgpack `AudioBlock`s reaches the wired input as
    /// exactly-512-sample 16 kHz mono windows, in this process, through the
    /// same engine code the parent's mailboxes run.
    ///
    /// The wheel crate's own test target is the only place this is provable
    /// without a device: everything above it is either a Python-level refusal
    /// or a rig-gated graph.
    #[test]
    fn a_windowed_input_link_hands_the_child_windows_rather_than_the_blocks_it_was_sent() {
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
                    None,
                )
                .unwrap();

            const SOURCE_FRAMES_PER_BLOCK: usize = 512;
            const SOURCE_RATE: i64 = 48_000;
            for block in 0..8i64 {
                let source_block = streamlib_media_builtins::AudioBlock {
                    interleaved_sample_bytes: (0..SOURCE_FRAMES_PER_BLOCK * 2)
                        .flat_map(|scalar| (scalar as f32 / 1024.0).to_le_bytes())
                        .collect(),
                    sample_rate: SOURCE_RATE as u32,
                    channels: 2,
                    sample_count: SOURCE_FRAMES_PER_BLOCK as u32,
                    dtype: streamlib_media_builtins::AudioSampleDtype::F32,
                    first_sample_timestamp_ns: block
                        * SOURCE_FRAMES_PER_BLOCK as i64
                        * 1_000_000_000
                        / SOURCE_RATE,
                };
                let payload = rmp_serde::to_vec_named(&source_block).unwrap();
                source
                    .write_to_output_port(
                        python,
                        "audio",
                        PyBytes::new(python, &payload).as_any(),
                        None,
                        true,
                    )
                    .unwrap();
            }

            let window_payload = destination
                .read_from_input_port(python, "audio_from_upstream", None)
                .unwrap()
                .expect("the windowed input received nothing");
            let window: streamlib_media_builtins::AudioBlock = rmp_serde::from_slice(
                &PythonPortPayloadBorrow::bytes_the_read_borrowed(&window_payload),
            )
            .expect("the stage emits a msgpack AudioBlock");
            assert_eq!(
                window.sample_count, 512,
                "the child's stage owes exactly what the contract declared"
            );
            assert_eq!(window.sample_rate, 16_000);
            assert_eq!(window.channels, 1);
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
                    None,
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
                    None,
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
                    None,
                )
                .unwrap_err();
            assert!(
                refusal.to_string().contains("whenever"),
                "the refusal must name what it got: {refusal}"
            );
        });
    }

    /// A source and a destination plane joined by one link, as the Python
    /// objects a helper's code holds.
    fn one_wired_link_as_python_objects<'py>(
        python: Python<'py>,
        label: &str,
    ) -> Bound<'py, PyDict> {
        let (source, destination) = one_wired_link(python, label);
        let planes = PyDict::new(python);
        planes
            .set_item("source", Bound::new(python, source).unwrap())
            .unwrap();
        planes
            .set_item("destination", Bound::new(python, destination).unwrap())
            .unwrap();
        planes
    }

    /// A read hands out the sample it arrived in: a `memoryview` of it and a
    /// DLPack tensor over it both start at the sample's own bytes, read-only,
    /// and `bytes()` is the copy.
    #[test]
    fn a_borrowed_read_shares_the_samples_memory_through_memoryview_and_dlpack() {
        Python::initialize();
        Python::attach(|python| {
            let planes = one_wired_link_as_python_objects(python, "borrowshares");
            let source = planes.get_item("source").unwrap().unwrap();
            let destination = planes.get_item("destination").unwrap().unwrap();
            source
                .call_method1(
                    "write_to_output_port",
                    (
                        "frames_to_downstream",
                        PyBytes::new(python, b"one shared sample"),
                    ),
                )
                .unwrap();
            let received = destination
                .call_method1("read_from_input_port", ("frames_from_upstream",))
                .unwrap();
            let (sample_address, is_a_channel_sample) = received
                .cast::<PythonPortPayloadBorrow>()
                .expect("a read hands back a PortPayloadBorrow")
                .get()
                .borrowed_payload_address_and_whether_it_is_a_channel_sample();
            assert!(is_a_channel_sample, "the read borrows the channel sample");

            let memoryview = python
                .import("builtins")
                .unwrap()
                .getattr("memoryview")
                .unwrap()
                .call1((&received,))
                .unwrap();
            let memoryview_buffer =
                PythonBufferHeldForOneWrite::of("frames_from_upstream", &memoryview).unwrap();
            assert_eq!(
                memoryview_buffer.bytes().as_ptr() as usize,
                sample_address,
                "memoryview(read) must be the sample's bytes, not a copy"
            );
            drop(memoryview_buffer);
            assert!(
                memoryview
                    .getattr("readonly")
                    .unwrap()
                    .extract::<bool>()
                    .unwrap()
            );

            let capsule = received.call_method0("__dlpack__").unwrap();
            // SAFETY: an unconsumed `__dlpack__` capsule carries a
            // `ManagedTensor` under the name `dltensor`.
            let tensor_data_address = unsafe {
                let managed_tensor =
                    pyo3::ffi::PyCapsule_GetPointer(capsule.as_ptr(), c"dltensor".as_ptr())
                        as *const streamlib_adapter_cuda::dlpack::ManagedTensor;
                (*managed_tensor).dl_tensor.data as usize
            };
            assert_eq!(tensor_data_address, sample_address);
            assert_eq!(
                received
                    .call_method0("__dlpack_device__")
                    .unwrap()
                    .extract::<(i32, i32)>()
                    .unwrap(),
                (1, 0)
            );

            let copied = python
                .import("builtins")
                .unwrap()
                .getattr("bytes")
                .unwrap()
                .call1((&received,))
                .unwrap();
            assert_eq!(
                copied.cast::<PyBytes>().unwrap().as_bytes(),
                b"one shared sample"
            );
        });
    }

    /// `ctx.loan` publishes what its block wrote on a clean exit, gives the
    /// sample back unpublished when the block raised, and a write takes any
    /// contiguous buffer.
    #[test]
    fn a_loan_publishes_on_a_clean_exit_and_gives_the_sample_back_when_its_block_raised() {
        Python::initialize();
        Python::attach(|python| {
            let planes = one_wired_link_as_python_objects(python, "loanexit");
            python
                .run(
                    cr#"
with source.loan_on_output_port("frames_to_downstream", 5, timestamp_ns=77) as out:
    assert len(out) == 5
    with memoryview(out) as view:
        view[:] = b"hello"
published, timestamp_ns = destination.read_from_input_port_with_timestamp("frames_from_upstream")
assert bytes(published) == b"hello", bytes(published)
assert timestamp_ns == 77, timestamp_ns

class TheBlockFailed(Exception):
    pass

try:
    with source.loan_on_output_port("frames_to_downstream", 4) as out:
        with memoryview(out) as view:
            view[:] = b"lost"
        raise TheBlockFailed()
except TheBlockFailed:
    pass
else:
    raise AssertionError("the block's exception must propagate")
assert destination.read_from_input_port("frames_from_upstream") is None

source.write_to_output_port("frames_to_downstream", bytearray(b"from a bytearray"))
source.write_to_output_port("frames_to_downstream", memoryview(b"xx from a view")[3:])
assert bytes(destination.read_from_input_port("frames_from_upstream")) == b"from a bytearray"
assert bytes(destination.read_from_input_port("frames_from_upstream")) == b"from a view"
"#,
                    Some(&planes),
                    Some(&planes),
                )
                .unwrap();
        });
    }

    /// Leaving a loan's block while an export of it lives publishes nothing
    /// and raises `BufferError`; the port stays loaned, refusing a second loan
    /// and a write, until the export goes.
    #[test]
    fn a_loan_left_exported_publishes_nothing_and_holds_its_port_until_the_export_goes() {
        Python::initialize();
        Python::attach(|python| {
            let planes = one_wired_link_as_python_objects(python, "loanexported");
            python
                .run(
                    cr#"
def refused(call, refusal_type):
    try:
        call()
    except refusal_type as refusal:
        return str(refusal)
    raise AssertionError(f"{call} must raise {refusal_type.__name__}")

view = None
def leave_the_block_with_an_export_alive():
    global view
    with source.loan_on_output_port("frames_to_downstream", 4) as out:
        view = memoryview(out)
        view[:] = b"kept"

assert "live export" in refused(leave_the_block_with_an_export_alive, BufferError)
assert "has a loan open" in refused(
    lambda: source.loan_on_output_port("frames_to_downstream", 4), RuntimeError
)
assert "has a loan open" in refused(
    lambda: source.write_to_output_port("frames_to_downstream", b"x"), RuntimeError
)
assert destination.read_from_input_port("frames_from_upstream") is None

view.release()
with source.loan_on_output_port("frames_to_downstream", 2) as out:
    tensor = out.__dlpack__()
    del tensor
    with memoryview(out) as view:
        view[:] = b"ok"
assert bytes(destination.read_from_input_port("frames_from_upstream")) == b"ok"

def leave_the_block_with_a_tensor_alive():
    global tensor
    with source.loan_on_output_port("frames_to_downstream", 2) as out:
        tensor = out.__dlpack__()

assert "live export" in refused(leave_the_block_with_a_tensor_alive, BufferError)
del tensor
source.write_to_output_port("frames_to_downstream", b"after")
assert bytes(destination.read_from_input_port("frames_from_upstream")) == b"after"
"#,
                    Some(&planes),
                    Some(&planes),
                )
                .unwrap();
        });
    }
}
