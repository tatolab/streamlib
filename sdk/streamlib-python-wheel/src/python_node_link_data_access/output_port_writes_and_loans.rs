// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use pyo3::exceptions::{PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyMapping};
use streamlib::sdk::color::h273_color_vui::{
    H273_UNSPECIFIED, h273_color_vui_from_color_info_axis_names,
};
use streamlib::sdk::error::Error;
use streamlib::sdk::iceoryx2::{
    ARRAY_FRAME_ATTRIBUTE_ROW_BYTES, ArrayFrameAttributeResidency, ArrayFrameAttributeRow,
    ArrayFrameDescriptorBody, ChannelsBlockDescriptorBody, ChannelsBlockSampleType,
    DataChannelPayloadDescriptor, ImageFrameDescriptorBody, ImageFrameH273Colour,
    MessageAFunctionNodeIslandHandsOnUnread, MessageGroupBoundary, array_frame_cpu_data_offsets,
};
use streamlib::sdk::rhi::{TensorElementType, TensorStorageBufferLayout, TextureFormat};

use super::{PythonNodeLinkDataAccess, declared_port_name_the_spelling_names};
use crate::python_bag_conversion::python_type_name_for_error_message;
use crate::python_node_context::PythonGpuContextLimitedAccess;
use crate::python_message_kind_read_types::PythonTensorCpuAttribute;

impl PythonNodeLinkDataAccess {
    /// Publish `samples`, a `[channel][sample]` array, as one Channels block
    /// on `port_name` starting at `start_sample`, at `sample_rate`.
    #[allow(
        clippy::too_many_arguments,
        reason = "each argument is a separate field of the Channels block being written"
    )]
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
        let (held_samples, sample_type, channel_count, samples_per_channel) =
            PythonBufferHeldForOneWrite::of_planar_channel_samples(port_name, samples)?;
        if sample_rate == 0 && samples_per_channel != 1 {
            return Err(PyValueError::new_err(format!(
                "write_channels_block() on output port {port_name:?} takes a sample_rate above \
                 0, or 0 for a control value of one sample per channel, which states no \
                 regular rate; it was handed {samples_per_channel} samples per channel at 0"
            )));
        }
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

    /// Publish `image`, a `Texture` this node read or a GPU surface it
    /// acquired from a pool, as one Image frame on `port_name`.
    #[allow(
        clippy::too_many_arguments,
        reason = "each argument is a separate field of the Image frame being written"
    )]
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
        let image_frame = image_frame_descriptor_of_the_image(
            port_name,
            image,
            declared_formats,
            gpu_limited_access,
        )?;
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
    #[allow(
        clippy::too_many_arguments,
        reason = "each argument is a separate field of the Array frame being written"
    )]
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
            if let Ok(read_attribute) = value.cast::<PythonTensorCpuAttribute>() {
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

    /// Write `message` on `port_name` exactly as it arrived: its payload, its
    /// descriptor, its stamp and its group boundary. A message past the
    /// channel's ceiling is dropped with a warning, as any write's is.
    pub(crate) fn hand_a_message_on_unread(
        &self,
        python: Python<'_>,
        port_name: &str,
        message: &MessageAFunctionNodeIslandHandsOnUnread,
    ) -> PyResult<()> {
        let port_name = &*declared_port_name_the_spelling_names(port_name)?;
        let Some(output_writer) = self.output_writer_reaching(port_name)? else {
            return Ok(());
        };
        match python.detach(|| {
            output_writer.write_bytes_described_by(
                port_name,
                &message.payload_bytes,
                message.timestamp_ns,
                message.group_boundary,
                message.payload_descriptor,
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

/// The Image frame descriptor of `image`, a `Texture` this node read or a GPU surface it
/// acquired from a pool, as a write on `port_name` publishes it: its pool slot, size, format and
/// colour; refused for anything else, a surface no pool slot of this node holds, or a format
/// `declared_formats`, when it names any, does not list.
pub(crate) fn image_frame_descriptor_of_the_image(
    port_name: &str,
    image: &Bound<'_, PyAny>,
    declared_formats: &[String],
    gpu_limited_access: &PythonGpuContextLimitedAccess,
) -> PyResult<ImageFrameDescriptorBody> {
    let not_an_image = || {
        PyTypeError::new_err(format!(
            "a write on Image output port {port_name:?} takes a Texture this node read \
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
    Ok(ImageFrameDescriptorBody {
        slot,
        width: image.getattr("width")?.extract()?,
        height: image.getattr("height")?.extract()?,
        format: format as u32,
        colour: image_frame_colour_the_image_states(image)?,
    })
}

/// The H.273 codes a `Texture`'s `colour` spells; unspecified when it states none.
fn image_frame_colour_the_image_states(image: &Bound<'_, PyAny>) -> PyResult<ImageFrameH273Colour> {
    let colour = image
        .getattr_opt("colour")?
        .filter(|colour| !colour.is_none());
    let Some(colour) = colour else {
        return Ok(ImageFrameH273Colour::UNSPECIFIED);
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
    const ALL: [Self; 2] = [Self::Gpu, Self::Cpu];

    /// The spelling `residency=` takes.
    fn wire_name(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Cpu => "cpu",
        }
    }

    /// The residency `wire_name` spells, or `None` for any other spelling.
    pub(crate) fn of_wire_name(wire_name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|residency| residency.wire_name() == wire_name)
    }
}

/// The bytes of one CPU attribute an Array write copies into its sample: a
/// Python buffer, or an attribute a read of an Array port returned.
enum ArrayAttributeCpuBytesHeldForOneWrite<'py> {
    PythonBuffer(PythonBufferHeldForOneWrite),
    ReadAttribute(Bound<'py, PythonTensorCpuAttribute>),
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
pub(super) struct PythonBufferHeldForOneWrite {
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
    pub(super) fn of(port_name: &str, payload: &Bound<'_, PyAny>) -> PyResult<Self> {
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
                 ({refusal}). A dict is written through a Records Output attribute.",
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

    pub(super) fn bytes(&self) -> &[u8] {
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
