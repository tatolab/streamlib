// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The DLPack shape and strides a pixel format, or a tensor storage
//! buffer, maps to.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use streamlib::sdk::rhi::PixelFormat;
use streamlib_adapter_cuda::dlpack::{DataType, DataTypeCode};

/// The DLPack shape a pixel format maps to.
#[derive(Clone, Debug)]
pub(crate) struct PixelExchangeTensorLayout {
    pub(crate) shape: Vec<i64>,
    /// Strides in **elements**, per the DLPack spec — not numpy's bytes.
    pub(crate) strides: Vec<i64>,
    pub(crate) dtype: DataType,
}

/// The DLPack element dtype and trailing channel axis for a single-plane
/// format — the two facts the engine's canonical `bits_per_pixel` /
/// `plane_count` tables do not carry. `None` for formats DLPack cannot
/// express as one buffer (multi-plane; exporting plane 0 alone would
/// hand out luma while silently dropping chroma).
fn single_plane_shape(format: PixelFormat) -> Option<(u32, DataType, Option<i64>)> {
    if format.plane_count() > 1 || format == PixelFormat::Unknown {
        return None;
    }
    let bytes_per_pixel = format.bits_per_pixel() / 8;
    let (dtype, channel_axis) = match format {
        PixelFormat::Bgra32 | PixelFormat::Rgba32 | PixelFormat::Argb32 => (DataType::U8, Some(4)),
        // 16 bits per channel: the element is a u16, so the channel axis
        // is still 4 wide but each element spans two bytes.
        PixelFormat::Rgba64 => (
            DataType {
                code: DataTypeCode::UInt,
                bits: 16,
                lanes: 1,
            },
            Some(4),
        ),
        // The float formats a kernel output carries: the element is the
        // channel, so torch sees float16 / float32 directly.
        PixelFormat::Rgba16Float => (
            DataType {
                code: DataTypeCode::Float,
                bits: 16,
                lanes: 1,
            },
            Some(4),
        ),
        PixelFormat::Rgba32Float => (
            DataType {
                code: DataTypeCode::Float,
                bits: 32,
                lanes: 1,
            },
            Some(4),
        ),
        PixelFormat::Gray8 => (DataType::U8, None),
        // Packed 4:2:2 — two bytes per pixel, but the byte pair is not a
        // per-pixel channel tuple. Exported as raw bytes with the trailing
        // axis naming the pair; interpreting it as colour is the
        // consumer's job.
        PixelFormat::Yuyv422 | PixelFormat::Uyvy422 => (DataType::U8, Some(2)),
        PixelFormat::Nv12VideoRange | PixelFormat::Nv12FullRange | PixelFormat::Unknown => {
            return None;
        }
    };
    Some((bytes_per_pixel, dtype, channel_axis))
}

impl PixelExchangeTensorLayout {
    /// A 1-D `uint8` run of `byte_len` bytes: a message payload's shape.
    pub(crate) fn one_dimensional_bytes(byte_len: usize) -> PyResult<Self> {
        let byte_len = i64::try_from(byte_len).map_err(|_| {
            PyValueError::new_err(format!(
                "a {byte_len}-byte payload is past what DLPack's int64 shape carries"
            ))
        })?;
        Ok(Self {
            shape: vec![byte_len],
            strides: vec![1],
            dtype: DataType {
                code: DataTypeCode::UInt,
                bits: 8,
                lanes: 1,
            },
        })
    }

    /// The contiguous row-major layout a tensor storage buffer declares:
    /// its own shape, element strides from the innermost dimension out.
    pub(crate) fn for_tensor_storage_buffer(
        tensor_layout: &streamlib::sdk::rhi::TensorStorageBufferLayout,
    ) -> PyResult<Self> {
        use streamlib::sdk::rhi::TensorElementType;

        let shape = tensor_layout
            .shape()
            .iter()
            .map(|dimension| i64::try_from(*dimension))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| {
                PyValueError::new_err(format!(
                    "tensor shape {:?} has a dimension DLPack's int64 cannot carry",
                    tensor_layout.shape()
                ))
            })?;
        let mut strides = vec![1i64; shape.len()];
        for axis in (0..shape.len().saturating_sub(1)).rev() {
            strides[axis] = strides[axis + 1]
                .checked_mul(shape[axis + 1])
                .ok_or_else(|| {
                    PyValueError::new_err(format!(
                        "tensor shape {:?} has an element stride DLPack's int64 cannot carry",
                        tensor_layout.shape()
                    ))
                })?;
        }
        let (code, bits) = match tensor_layout.element_type() {
            TensorElementType::Float32 => (DataTypeCode::Float, 32),
            TensorElementType::Float16 => (DataTypeCode::Float, 16),
            TensorElementType::Uint8 => (DataTypeCode::UInt, 8),
            TensorElementType::Int32 => (DataTypeCode::Int, 32),
        };
        Ok(Self {
            shape,
            strides,
            dtype: DataType {
                code,
                bits,
                lanes: 1,
            },
        })
    }

    /// Derive the layout for a single-plane surface.
    ///
    /// `bytes_per_row` comes from the allocation rather than from
    /// `width * bytes_per_pixel` so a padded row pitch stays correct.
    pub(crate) fn for_pixel_format(
        format: PixelFormat,
        width: u32,
        height: u32,
        bytes_per_row: u64,
    ) -> PyResult<Self> {
        let Some((bytes_per_pixel, dtype, channel_axis)) = single_plane_shape(format) else {
            return Err(PyValueError::new_err(format!(
                "pixel format {:?} has no single-buffer DLPack shape: DLPack expresses one \
                 strided linear buffer, and exporting only its first plane would hand out \
                 part of the image. Convert to a packed format first.",
                format
            )));
        };

        let element_bytes = u64::from(dtype.bits / 8);
        if element_bytes == 0 || !bytes_per_row.is_multiple_of(element_bytes) {
            return Err(PyValueError::new_err(format!(
                "row pitch {bytes_per_row} is not a whole number of {element_bytes}-byte elements"
            )));
        }
        let row_stride_elements = (bytes_per_row / element_bytes) as i64;
        let channels_per_pixel = i64::from(bytes_per_pixel) / element_bytes as i64;

        let (shape, strides) = match channel_axis {
            Some(channels) => (
                vec![i64::from(height), i64::from(width), channels],
                vec![row_stride_elements, channels_per_pixel, 1],
            ),
            None => (
                vec![i64::from(height), i64::from(width)],
                vec![row_stride_elements, channels_per_pixel],
            ),
        };
        Ok(Self {
            shape,
            strides,
            dtype,
        })
    }
}
