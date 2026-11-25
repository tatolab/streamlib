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

use super::{PythonNodeLinkDataAccess, declared_port_name_the_spelling_names, unwired_port_error};
use crate::python_bag_conversion::{
    records_message_dict_of, type_of_its_kind_built_offering_gpu_access,
};
use crate::python_message_kind_read_types::{
    PythonAudio, PythonEncodedMedia, PythonTensorCpuAttribute,
};
use crate::python_node_context::{PooledGpuFrameClaimHeldByARead, PythonGpuContextLimitedAccess};
use crate::python_port_payload::PythonPortPayloadBorrow;

impl PythonNodeLinkDataAccess {
    /// The next message's payload on `port_name`, offering
    /// `offered_gpu_limited_access` to the type of its kind a read builds.
    ///
    /// The one read body; the context-carrying reader passes its capability
    /// and the bare data plane passes nothing.
    pub(crate) fn read_from_input_port_offering_gpu_access<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        read_shape: PortPayloadReadShape,
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
            Some(message) => payload_read_as_its_shape(
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
        read_shape: PortPayloadReadShape,
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
                Some(payload_read_as_its_shape(
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

    /// The next message on `port_name` as a `tatolab.stream.Message` — its
    /// payload, stamp, group flag, losses and inbound link — or `None` when the
    /// mailbox is empty.
    pub(crate) fn read_message_from_input_port<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        read_shape: PortPayloadReadShape,
        offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
    ) -> PyResult<Option<Bound<'py, PyAny>>> {
        static TATOLAB_STREAM_MESSAGE_CLASS: PyOnceLock<Py<PyType>> = PyOnceLock::new();
        let Some((payload, message, inbound_link_name)) = self
            .read_message_naming_its_inbound_link(
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
    /// read as `read_shape` asks, and the inbound link it arrived on.
    pub(super) fn read_message_naming_its_inbound_link<'py>(
        &self,
        python: Python<'py>,
        port_name: &str,
        read_shape: PortPayloadReadShape,
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
        let payload = payload_read_as_its_shape(
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
pub(crate) enum PortPayloadReadShape {
    /// The `PortPayloadBorrow` of the sample: a read naming no kind of message.
    RawBorrow,
    /// The type of the arriving message's kind, built from its descriptor: a read naming the
    /// kinds of message its port takes.
    OfItsKind(MessageKindsOfTheRead),
}

/// A kind of message a read or a write names by its wire name, each the kind of a descriptor
/// the type of its port travels as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MessageKindOfTheRead {
    ImageFrame,
    ArrayFrame,
    ChannelsBlock,
    PacketsUnit,
    RecordsMessage,
}

impl MessageKindOfTheRead {
    const ALL: [Self; 5] = [
        Self::ImageFrame,
        Self::ArrayFrame,
        Self::ChannelsBlock,
        Self::PacketsUnit,
        Self::RecordsMessage,
    ];

    /// The kind's name on the wire, such as `image_frame`.
    pub(crate) fn wire_name(self) -> &'static str {
        match self {
            Self::ImageFrame => "image_frame",
            Self::ArrayFrame => "array_frame",
            Self::ChannelsBlock => "channels_block",
            Self::PacketsUnit => "packets_unit",
            Self::RecordsMessage => "records_message",
        }
    }

    /// The kind the wire names `wire_name`, or `None` for any other name.
    pub(crate) fn of_wire_name(wire_name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.wire_name() == wire_name)
    }

    /// The descriptor kind every message of this kind carries.
    fn descriptor_kind(self) -> DataChannelPayloadDescriptorKind {
        match self {
            Self::ImageFrame => DataChannelPayloadDescriptorKind::ImageFrame,
            Self::ArrayFrame => DataChannelPayloadDescriptorKind::ArrayFrame,
            Self::ChannelsBlock => DataChannelPayloadDescriptorKind::ChannelsBlock,
            Self::PacketsUnit => DataChannelPayloadDescriptorKind::PacketsUnit,
            Self::RecordsMessage => DataChannelPayloadDescriptorKind::RecordsMessage,
        }
    }
}

/// The kinds of message a read takes: the one its port's pin type travels as and any other
/// its pin's types travel as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MessageKindsOfTheRead {
    declared_kind: MessageKindOfTheRead,
    is_taken_by_kind: [bool; MessageKindOfTheRead::ALL.len()],
}

impl MessageKindsOfTheRead {
    /// An input taking `declared_kind` alone.
    #[cfg(test)]
    pub(crate) fn of_one(declared_kind: MessageKindOfTheRead) -> Self {
        Self::of_the_declared_kind_and_also(declared_kind, [])
    }

    /// An input taking `declared_kind` and each of `also_taken_kinds`.
    pub(crate) fn of_the_declared_kind_and_also(
        declared_kind: MessageKindOfTheRead,
        also_taken_kinds: impl IntoIterator<Item = MessageKindOfTheRead>,
    ) -> Self {
        let mut is_taken_by_kind = [false; MessageKindOfTheRead::ALL.len()];
        for taken in std::iter::once(declared_kind).chain(also_taken_kinds) {
            is_taken_by_kind[taken as usize] = true;
        }
        Self {
            declared_kind,
            is_taken_by_kind,
        }
    }

    /// These kinds and `kind` too.
    pub(crate) fn also_taking(mut self, kind: MessageKindOfTheRead) -> Self {
        self.is_taken_by_kind[kind as usize] = true;
        self
    }

    /// Whether a message described as `descriptor_kind` is of a kind this read takes.
    fn takes(self, descriptor_kind: DataChannelPayloadDescriptorKind) -> bool {
        MessageKindOfTheRead::ALL.into_iter().any(|kind| {
            self.is_taken_by_kind[kind as usize] && kind.descriptor_kind() == descriptor_kind
        })
    }
}

/// One message's payload as a `PortPayloadBorrow`, or the type of its kind a read
/// naming kinds builds from it — the half every read shares once the
/// payload is in hand.
#[allow(
    clippy::too_many_arguments,
    reason = "each argument is a separate piece of the read the caller already holds"
)]
fn payload_read_as_its_shape<'py>(
    python: Python<'py>,
    port_name: &str,
    payload: InputPortMessagePayload,
    payload_descriptor: DataChannelPayloadDescriptor,
    timestamp_ns: i64,
    group_boundary: MessageGroupBoundary,
    read_shape: PortPayloadReadShape,
    offered_gpu_limited_access: Option<&Bound<'py, PythonGpuContextLimitedAccess>>,
) -> PyResult<Bound<'py, PyAny>> {
    if let PortPayloadReadShape::OfItsKind(kinds) = read_shape {
        let kind = payload_descriptor.known_kind();
        if !kind.as_ref().is_ok_and(|kind| kinds.takes(*kind)) {
            let declared_kind = kinds.declared_kind;
            return Err(PyTypeError::new_err(format!(
                "a read of {declared_kind:?} input port {port_name:?} got a message described \
                 as {kind:?} rather than {:?}: its producer wrote it as raw bytes rather than as \
                 a kind of message the port takes. Read it naming no kind to take its raw \
                 payload.",
                declared_kind.descriptor_kind()
            )));
        }
        match kind {
            Ok(DataChannelPayloadDescriptorKind::ChannelsBlock) => {
                let block =
                    ChannelsBlockDescriptorBody::from_descriptor_body(&payload_descriptor.body);
                return Ok(Bound::new(
                    python,
                    PythonAudio::holding(port_name, payload, block, timestamp_ns)?,
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
                    PythonEncodedMedia {
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
    let pooled_frame_the_message_carries = payload_descriptor.pooled_gpu_frame_it_names();
    if let (Some(pooled_frame), Some(gpu_limited_access)) =
        (pooled_frame_the_message_carries, offered_gpu_limited_access)
    {
        let claimed = gpu_limited_access
            .get()
            .note_and_claim_the_pooled_gpu_frame_a_read_carries(python, pooled_frame);
        match claimed {
            Ok(claim) => pooled_gpu_frame_claim = claim,
            Err(refusal) => tracing::warn!(
                "a read on {port_name:?} kept its payload, though the pooled GPU frame it \
                 carries could not be claimed by its claim table: {refusal}"
            ),
        }
    }
    Ok(Bound::new(
        python,
        PythonPortPayloadBorrow::holding(payload, pooled_gpu_frame_claim),
    )?
    .into_any())
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

/// The `Texture` an Image descriptor states, its slot claimed.
pub(crate) fn image_frame_of_the_read<'py>(
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
    type_of_its_kind_built_offering_gpu_access(
        &python
            .import("tatolab.stream.image_frame")?
            .getattr("Texture")?,
        &fields,
        offered_gpu_limited_access,
    )
}

/// The `Tensor` an Array payload's attribute table states: each CPU
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
                PythonTensorCpuAttribute::over(
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
                type_of_its_kind_built_offering_gpu_access(
                    &array_frame_module.getattr("TensorGpuAttribute")?,
                    &fields,
                    offered_gpu_limited_access,
                )?
            }
        };
        attributes.set_item(&row.name, attribute)?;
    }
    array_frame_module
        .getattr("Tensor")?
        .call1((attributes, timestamp_ns))
}
