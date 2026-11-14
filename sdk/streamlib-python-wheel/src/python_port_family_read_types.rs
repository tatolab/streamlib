// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The family types a read through a port attribute hands Python: a Channels
//! block's planar samples and an Array frame's CPU attributes, exported
//! through DLPack, and a Packets unit's bytes, each borrowed in the sample it
//! arrived in.

use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use streamlib::sdk::iceoryx2::{
    ArrayFrameAttributeRow, ChannelsBlockDescriptorBody, ChannelsBlockSampleType,
    InputPortMessagePayload,
};
use streamlib::sdk::rhi::TensorStorageBufferLayout;
use streamlib_adapter_cuda::dlpack::{DataType, DataTypeCode};

use crate::python_gpu_surface_pixel_exchange::{
    HOST_VISIBLE_DLPACK_DEVICE, PixelExchangeTensorLayout, dlpack_capsule_over,
    dlpack_device_as_python_pair, exchange_shape_for_max_version,
};
use crate::python_port_payload::refuse_a_dlpack_request_a_cpu_payload_cannot_meet;

/// A Channels block a read returned: `channel_count` runs of
/// `samples_per_channel` samples, borrowed in place.
#[pyclass(name = "ChannelsBlock", module = "tatolab.stream", frozen)]
pub(crate) struct PythonChannelsBlock {
    borrowed_payload: Arc<parking_lot::Mutex<InputPortMessagePayload>>,
    payload_base_address: usize,
    block: ChannelsBlockDescriptorBody,
    sample_type: ChannelsBlockSampleType,
    timestamp_ns: i64,
}

impl PythonChannelsBlock {
    /// Hold `payload` as the block `block` describes, refused when the
    /// payload's length is not the planar length the body states. A rate of 0
    /// is a control value's, which has no regular rate.
    pub(crate) fn holding(
        port_name: &str,
        payload: InputPortMessagePayload,
        block: ChannelsBlockDescriptorBody,
        timestamp_ns: i64,
    ) -> PyResult<Self> {
        let sample_type = ChannelsBlockSampleType::from_wire(block.sample_type).ok_or_else(|| {
            PyValueError::new_err(format!(
                "a Channels block on {port_name:?} names sample type {}, which this reader does \
                 not know",
                block.sample_type
            ))
        })?;
        let payload_byte_len = payload.bytes().len();
        if block.planar_payload_byte_len() != Some(payload_byte_len) {
            return Err(PyValueError::new_err(format!(
                "a Channels block on {port_name:?} states {} channels of {} samples, which its \
                 {payload_byte_len}-byte payload does not hold",
                block.channel_count, block.samples_per_channel
            )));
        }
        let payload_base_address = payload.bytes().as_ptr() as usize;
        Ok(Self {
            borrowed_payload: Arc::new(parking_lot::Mutex::new(payload)),
            payload_base_address,
            block,
            sample_type,
            timestamp_ns,
        })
    }

    fn sample_data_type(&self) -> DataType {
        DataType {
            code: DataTypeCode::Float,
            bits: (self.sample_type.sample_byte_len() * 8) as u8,
            lanes: 1,
        }
    }
}

#[pymethods]
impl PythonChannelsBlock {
    /// The stream-wide index of this block's first sample.
    #[getter]
    fn start_sample(&self) -> u64 {
        self.block.start_sample
    }

    /// Samples per second, per channel.
    #[getter]
    fn sample_rate(&self) -> u32 {
        self.block.sample_rate
    }

    /// How many channels the block holds.
    #[getter]
    fn channel_count(&self) -> u32 {
        self.block.channel_count
    }

    /// How many samples each channel holds.
    #[getter]
    fn samples_per_channel(&self) -> u32 {
        self.block.samples_per_channel
    }

    /// The sample type, `"f32"` or `"f64"`.
    #[getter]
    fn dtype(&self) -> &'static str {
        match self.sample_type {
            ChannelsBlockSampleType::F32 => "f32",
            ChannelsBlockSampleType::F64 => "f64",
        }
    }

    /// The stream time the block was stamped with, in nanoseconds.
    #[getter]
    fn timestamp_ns(&self) -> i64 {
        self.timestamp_ns
    }

    /// The samples as a read-only `[channel][sample]` CPU tensor over the sample.
    #[pyo3(signature = (*, stream = None, max_version = None, dl_device = None, copy = None))]
    fn __dlpack__<'py>(
        &self,
        python: Python<'py>,
        stream: Option<&Bound<'py, PyAny>>,
        max_version: Option<(u32, u32)>,
        dl_device: Option<(i32, i32)>,
        copy: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        refuse_a_dlpack_request_a_cpu_payload_cannot_meet(stream, dl_device, copy)?;
        let samples_per_channel = i64::from(self.block.samples_per_channel);
        dlpack_capsule_over(
            python,
            self.payload_base_address as u64,
            PixelExchangeTensorLayout {
                shape: vec![i64::from(self.block.channel_count), samples_per_channel],
                strides: vec![samples_per_channel, 1],
                dtype: self.sample_data_type(),
            },
            HOST_VISIBLE_DLPACK_DEVICE,
            exchange_shape_for_max_version(max_version),
            true,
            Box::new(Arc::clone(&self.borrowed_payload)),
        )
    }

    /// `(kDLCPU, 0)`: the block is host memory.
    fn __dlpack_device__(&self) -> (i32, i32) {
        dlpack_device_as_python_pair(HOST_VISIBLE_DLPACK_DEVICE)
    }
}

/// One CPU-resident attribute of an Array frame a read returned: a
/// contiguous row-major tensor borrowed in the sample it arrived in.
#[pyclass(name = "ArrayFrameCpuAttribute", module = "tatolab.stream", frozen)]
pub(crate) struct PythonArrayFrameCpuAttribute {
    borrowed_payload: Arc<parking_lot::Mutex<InputPortMessagePayload>>,
    attribute_base_address: usize,
    tensor_layout: TensorStorageBufferLayout,
}

impl PythonArrayFrameCpuAttribute {
    /// The attribute `row` states, over `byte_len` bytes at `payload_byte_offset`
    /// of the shared payload; refused when its dtype is unknown or its shape
    /// does not span exactly those bytes.
    pub(crate) fn over(
        port_name: &str,
        borrowed_payload: &Arc<parking_lot::Mutex<InputPortMessagePayload>>,
        payload_base_address: usize,
        row: &ArrayFrameAttributeRow,
        payload_byte_offset: u64,
        byte_len: u64,
    ) -> PyResult<Self> {
        let tensor_layout = TensorStorageBufferLayout::from_wire(row.shape.clone(), &row.dtype)
            .map_err(|refusal| {
                PyValueError::new_err(format!(
                    "Array attribute {:?} on {port_name:?}: {refusal}",
                    row.name
                ))
            })?;
        if tensor_layout.byte_size() != byte_len {
            return Err(PyValueError::new_err(format!(
                "Array attribute {:?} on {port_name:?} states shape {:?} of {}, which is {} \
                 bytes, over {byte_len} bytes of payload",
                row.name,
                row.shape,
                row.dtype,
                tensor_layout.byte_size()
            )));
        }
        Ok(Self {
            borrowed_payload: Arc::clone(borrowed_payload),
            attribute_base_address: payload_base_address + payload_byte_offset as usize,
            tensor_layout,
        })
    }
}

impl PythonArrayFrameCpuAttribute {
    /// The attribute's bytes, read where the sample holds them.
    pub(crate) fn attribute_bytes(&self) -> &[u8] {
        // SAFETY: the shared payload this attribute holds keeps the sample,
        // and the constructor checked the attribute's span lies inside it.
        unsafe {
            std::slice::from_raw_parts(
                self.attribute_base_address as *const u8,
                self.tensor_layout.byte_size() as usize,
            )
        }
    }
}

#[pymethods]
impl PythonArrayFrameCpuAttribute {
    /// The element type's wire name, such as `"float32"`.
    #[getter]
    pub(crate) fn dtype(&self) -> &'static str {
        self.tensor_layout.element_type().wire_name()
    }

    /// The dimensions, outermost first.
    #[getter]
    pub(crate) fn shape(&self) -> Vec<u64> {
        self.tensor_layout.shape().to_vec()
    }

    /// The attribute as a read-only CPU tensor over the sample.
    #[pyo3(signature = (*, stream = None, max_version = None, dl_device = None, copy = None))]
    fn __dlpack__<'py>(
        &self,
        python: Python<'py>,
        stream: Option<&Bound<'py, PyAny>>,
        max_version: Option<(u32, u32)>,
        dl_device: Option<(i32, i32)>,
        copy: Option<bool>,
    ) -> PyResult<Bound<'py, PyAny>> {
        refuse_a_dlpack_request_a_cpu_payload_cannot_meet(stream, dl_device, copy)?;
        dlpack_capsule_over(
            python,
            self.attribute_base_address as u64,
            PixelExchangeTensorLayout::for_tensor_storage_buffer(&self.tensor_layout)?,
            HOST_VISIBLE_DLPACK_DEVICE,
            exchange_shape_for_max_version(max_version),
            true,
            Box::new(Arc::clone(&self.borrowed_payload)),
        )
    }

    /// `(kDLCPU, 0)`: the attribute is host memory.
    fn __dlpack_device__(&self) -> (i32, i32) {
        dlpack_device_as_python_pair(HOST_VISIBLE_DLPACK_DEVICE)
    }
}

/// A Packets unit a read returned: its bytes borrowed in the sample they
/// arrived in, with its presentation stamp, whether a decoder may enter at
/// it, and its codec.
#[pyclass(name = "PacketsUnit", module = "tatolab.stream", frozen)]
pub(crate) struct PythonPacketsUnit {
    pub(crate) payload: Py<crate::python_port_payload::PythonPortPayloadBorrow>,
    pub(crate) pts: i64,
    pub(crate) starts_group: bool,
    pub(crate) codec: String,
}

#[pymethods]
impl PythonPacketsUnit {
    /// The unit's bytes, a borrow of the sample.
    #[getter]
    fn payload(
        &self,
        python: Python<'_>,
    ) -> Py<crate::python_port_payload::PythonPortPayloadBorrow> {
        self.payload.clone_ref(python)
    }

    /// The presentation stamp, in stream nanoseconds.
    #[getter]
    fn pts(&self) -> i64 {
        self.pts
    }

    /// Whether a decoder may start at this unit: a keyframe, or any unit of a
    /// plain byte stream.
    #[getter]
    fn starts_group(&self) -> bool {
        self.starts_group
    }

    /// The codec the bytes are encoded with, or `""` for a plain byte stream.
    #[getter]
    fn codec(&self) -> &str {
        &self.codec
    }
}
