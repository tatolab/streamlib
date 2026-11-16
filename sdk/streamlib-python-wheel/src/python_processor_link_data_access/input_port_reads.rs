// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyDict, PyTuple, PyType};
use streamlib::sdk::color::h273_color_vui::{
    H273ColorVui, color_info_axis_names_of_h273_color_vui,
};
use streamlib::sdk::iceoryx2::{
    ArrayFrameAttributeResidency, ArrayFrameDescriptorBody, ChannelsBlockDescriptorBody,
    DataChannelPayloadDescriptor, DataChannelPayloadDescriptorKind, GpuPoolSlotFrameDescriptorBody,
    ImageFrameDescriptorBody, ImageFrameH273Colour, InboundLinkName, InputPortMessagePayload,
    MessageGroupBoundary, MessageReadFromInputPort, PacketsUnitDescriptorBody,
    array_frame_attribute_table_of,
};
use streamlib::sdk::rhi::TextureFormat;

use super::{
    PythonProcessorLinkDataAccess, declared_port_name_the_spelling_names, unwired_port_error,
};
use crate::python_bag_conversion::{
    cast_payload_into_read_target, family_type_built_offering_gpu_access, records_message_dict_of,
};
use crate::python_port_family_read_types::{
    PythonArrayFrameCpuAttribute, PythonChannelsBlock, PythonPacketsUnit,
};
use crate::python_port_payload::PythonPortPayloadBorrow;
use crate::python_processor_context::{
    PooledGpuFrameClaimHeldByARead, PythonGpuContextLimitedAccess,
};

impl PythonProcessorLinkDataAccess {
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
    pub(super) fn read_message_naming_its_inbound_link_with_its_payload_cast<'py>(
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

    /// The message `port_name` holds for this read, as it arrived; `None` for
    /// none, or for a declared input port no link has reached yet.
    pub(crate) fn read_the_message_at_an_input_port_as_it_arrived(
        &self,
        python: Python<'_>,
        port_name: &str,
    ) -> PyResult<Option<MessageReadFromInputPort>> {
        let port_name = &*declared_port_name_the_spelling_names(port_name)?;
        let Some(input_mailboxes) = self.input_mailboxes_reaching(port_name)? else {
            return Ok(None);
        };
        python
            .detach(|| input_mailboxes.read_raw(port_name))
            .map_err(|refusal| PyRuntimeError::new_err(refusal.to_string()))
    }
}

/// What a read hands back for a message's payload.
#[derive(Clone, Copy)]
pub(crate) enum PortPayloadReadShape<'target, 'py> {
    /// The `PortPayloadBorrow` of the sample: a read by name.
    RawBorrow,
    /// The arriving family's own type, built from the message's descriptor
    /// kind: a read through a port attribute taking those families.
    FamilyType(PortFamiliesOfTheRead),
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
    const ALL: [Self; 5] = [
        Self::Image,
        Self::Array,
        Self::Channels,
        Self::Packets,
        Self::Records,
    ];

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

/// The families an `Input` attribute a read names takes: its own and its
/// `also_takes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PortFamiliesOfTheRead {
    declared_family: PortFamilyOfTheRead,
    is_taken_by_family: [bool; PortFamilyOfTheRead::ALL.len()],
}

impl PortFamiliesOfTheRead {
    /// An input taking `declared_family` alone.
    #[cfg(test)]
    pub(crate) fn of_one(declared_family: PortFamilyOfTheRead) -> Self {
        Self::of_the_declared_family_and_also(declared_family, [])
    }

    /// An input taking `declared_family` and each of `also_taken_families`.
    pub(crate) fn of_the_declared_family_and_also(
        declared_family: PortFamilyOfTheRead,
        also_taken_families: impl IntoIterator<Item = PortFamilyOfTheRead>,
    ) -> Self {
        let mut is_taken_by_family = [false; PortFamilyOfTheRead::ALL.len()];
        for family in std::iter::once(declared_family).chain(also_taken_families) {
            is_taken_by_family[family as usize] = true;
        }
        Self {
            declared_family,
            is_taken_by_family,
        }
    }

    /// The taken family whose messages carry `kind`.
    fn family_carrying(
        self,
        kind: DataChannelPayloadDescriptorKind,
    ) -> Option<PortFamilyOfTheRead> {
        PortFamilyOfTheRead::ALL.into_iter().find(|family| {
            self.is_taken_by_family[*family as usize] && family.descriptor_kind() == kind
        })
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
#[allow(
    clippy::too_many_arguments,
    reason = "each argument is a separate piece of the read the caller already holds"
)]
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
    if let PortPayloadReadShape::FamilyType(families) = read_shape {
        let kind = payload_descriptor.known_kind();
        let family_of_the_message = kind
            .as_ref()
            .ok()
            .and_then(|kind| families.family_carrying(*kind));
        if family_of_the_message.is_none() {
            let family = families.declared_family;
            return Err(PyTypeError::new_err(format!(
                "a read of {family:?} input port {port_name:?} got a message described as \
                 {kind:?} rather than {:?}: its producer wrote it by name rather than through \
                 an Output of a family the port takes. Read it by name to take its raw payload.",
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

/// The `tatolab.stream.ColorInfo` an Image frame's H.273 codes spell.
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
        .import("tatolab.stream.color_info")?
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
