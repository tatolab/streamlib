// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The declared tensor shape and element type a storage buffer carries.

use crate::core::{Error, Result};

/// The element type of a tensor storage buffer, spelled on every wire as its
/// DLPack-conventional lowercase name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TensorElementType {
    /// IEEE 754 binary32.
    Float32,
    /// IEEE 754 binary16.
    Float16,
    /// Unsigned 8-bit integer.
    Uint8,
    /// Signed 32-bit integer.
    Int32,
}

impl TensorElementType {
    /// Every element type, in the order refusals list them.
    pub const ALL: [Self; 4] = [Self::Float32, Self::Float16, Self::Uint8, Self::Int32];

    /// The wire and Python spelling: `float32`, `float16`, `uint8`, `int32`.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Float32 => "float32",
            Self::Float16 => "float16",
            Self::Uint8 => "uint8",
            Self::Int32 => "int32",
        }
    }

    /// The element type a wire name spells, or `None` for any other string.
    pub fn from_wire_name(wire_name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|element_type| element_type.wire_name() == wire_name)
    }

    /// Bytes one element occupies.
    pub fn byte_width(self) -> u64 {
        match self {
            Self::Float32 | Self::Int32 => 4,
            Self::Float16 => 2,
            Self::Uint8 => 1,
        }
    }
}

impl std::fmt::Display for TensorElementType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.wire_name())
    }
}

/// The surface-share wire key a tensor's dimensions travel under.
const SURFACE_SHARE_TENSOR_SHAPE_FIELD: &str = "shape";

/// The surface-share wire key a tensor's element type travels under.
const SURFACE_SHARE_TENSOR_DTYPE_FIELD: &str = "dtype";

/// A contiguous row-major tensor: its shape and its element type, validated
/// so the byte size is non-zero and fits a `u64`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TensorStorageBufferLayout {
    shape: Vec<u64>,
    element_type: TensorElementType,
    byte_size: u64,
}

impl TensorStorageBufferLayout {
    /// Validate `shape` and `element_type`, refusing an empty shape, a zero
    /// dimension, or a byte size past `u64`.
    pub fn new(shape: Vec<u64>, element_type: TensorElementType) -> Result<Self> {
        if shape.is_empty() {
            return Err(Error::Configuration(
                "a tensor storage buffer needs at least one dimension; its shape was empty"
                    .to_string(),
            ));
        }
        if shape.contains(&0) {
            return Err(Error::Configuration(format!(
                "tensor shape {shape:?} has a zero dimension, which describes no memory"
            )));
        }
        let byte_size = shape
            .iter()
            .try_fold(element_type.byte_width(), |bytes, dimension| {
                bytes.checked_mul(*dimension)
            })
            .ok_or_else(|| {
                Error::Configuration(format!(
                    "tensor shape {shape:?} of {element_type} overflows a 64-bit byte size"
                ))
            })?;
        Ok(Self {
            shape,
            element_type,
            byte_size,
        })
    }

    /// A byte-shaped buffer: one `uint8` dimension of `byte_size`.
    pub fn of_bytes(byte_size: u64) -> Result<Self> {
        Self::new(vec![byte_size], TensorElementType::Uint8)
    }

    /// Parse a wire `(shape, dtype)` pair, refusing an unknown dtype by name.
    pub fn from_wire(shape: Vec<u64>, dtype_wire_name: &str) -> Result<Self> {
        let element_type = TensorElementType::from_wire_name(dtype_wire_name).ok_or_else(|| {
            Error::Configuration(format!(
                "unknown tensor dtype {dtype_wire_name:?}; a tensor storage buffer takes one of {}",
                TensorElementType::ALL
                    .map(TensorElementType::wire_name)
                    .join(", ")
            ))
        })?;
        Self::new(shape, element_type)
    }

    /// The layout a surface-share registration or checkout states in its
    /// `shape` and `dtype` fields, refusing either missing or malformed.
    pub fn from_surface_share_fields(fields: &serde_json::Value) -> Result<Self> {
        let shape = fields
            .get(SURFACE_SHARE_TENSOR_SHAPE_FIELD)
            .and_then(serde_json::Value::as_array)
            .and_then(|dimensions| {
                dimensions
                    .iter()
                    .map(serde_json::Value::as_u64)
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or_else(|| {
                Error::Configuration(
                    "a tensor storage buffer carries no shape array of unsigned integers"
                        .to_string(),
                )
            })?;
        let dtype = fields
            .get(SURFACE_SHARE_TENSOR_DTYPE_FIELD)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                Error::Configuration("a tensor storage buffer carries no dtype".to_string())
            })?;
        Self::from_wire(shape, dtype)
    }

    /// Write this layout's `shape` and `dtype` fields onto a surface-share
    /// registration or lookup reply.
    pub(crate) fn write_surface_share_fields(&self, fields: &mut serde_json::Value) {
        fields[SURFACE_SHARE_TENSOR_SHAPE_FIELD] = self.shape.as_slice().into();
        fields[SURFACE_SHARE_TENSOR_DTYPE_FIELD] = self.element_type.wire_name().into();
    }

    /// The dimensions, outermost first.
    pub fn shape(&self) -> &[u64] {
        &self.shape
    }

    /// The element type.
    pub fn element_type(&self) -> TensorElementType {
        self.element_type
    }

    /// The tensor's exact byte size — never an allocation's rounded size.
    pub fn byte_size(&self) -> u64 {
        self.byte_size
    }
}

impl std::fmt::Display for TensorStorageBufferLayout {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:?} of {}", self.shape, self.element_type)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_input_tensor_sizes_to_its_elements() {
        let layout =
            TensorStorageBufferLayout::new(vec![1, 3, 640, 640], TensorElementType::Float32)
                .expect("a model input shape is valid");
        assert_eq!(layout.byte_size(), 3 * 640 * 640 * 4);
    }

    #[test]
    fn an_odd_shape_keeps_its_exact_byte_size() {
        let layout = TensorStorageBufferLayout::new(vec![3, 7, 11], TensorElementType::Float16)
            .expect("an odd shape is valid");
        assert_eq!(layout.byte_size(), 3 * 7 * 11 * 2);
    }

    #[test]
    fn an_empty_shape_is_refused() {
        let refusal = TensorStorageBufferLayout::new(Vec::new(), TensorElementType::Uint8)
            .expect_err("no dimensions describe no tensor");
        assert!(refusal.to_string().contains("at least one dimension"));
    }

    #[test]
    fn a_zero_dimension_is_refused() {
        let refusal = TensorStorageBufferLayout::new(vec![4, 0], TensorElementType::Int32)
            .expect_err("a zero dimension describes no memory");
        assert!(refusal.to_string().contains("zero dimension"));
    }

    #[test]
    fn a_byte_size_past_u64_is_refused() {
        let refusal = TensorStorageBufferLayout::new(vec![u64::MAX, 2], TensorElementType::Uint8)
            .expect_err("the byte size overflows");
        assert!(refusal.to_string().contains("overflows"));
    }

    #[test]
    fn every_dtype_round_trips_its_wire_name() {
        for element_type in TensorElementType::ALL {
            assert_eq!(
                TensorElementType::from_wire_name(element_type.wire_name()),
                Some(element_type)
            );
        }
    }

    #[test]
    fn the_surface_share_fields_round_trip_the_layout() {
        let layout = TensorStorageBufferLayout::new(vec![3, 7, 11], TensorElementType::Float16)
            .expect("an odd shape is valid");
        let mut fields = serde_json::json!({});
        layout.write_surface_share_fields(&mut fields);
        assert_eq!(
            fields,
            serde_json::json!({"shape": [3, 7, 11], "dtype": "float16"})
        );
        assert_eq!(
            TensorStorageBufferLayout::from_surface_share_fields(&fields).unwrap(),
            layout
        );
        assert_eq!(layout.to_string(), "[3, 7, 11] of float16");
    }

    #[test]
    fn surface_share_fields_without_a_shape_are_refused_by_name() {
        let refusal = TensorStorageBufferLayout::from_surface_share_fields(&serde_json::json!({
            "shape": [3, -1],
            "dtype": "float32",
        }))
        .expect_err("a negative dimension is not a shape");
        assert!(refusal.to_string().contains("no shape"));
    }

    #[test]
    fn an_unknown_dtype_is_refused_naming_the_known_ones() {
        let refusal = TensorStorageBufferLayout::from_wire(vec![2], "float64")
            .expect_err("float64 is not a tensor dtype");
        let refusal = refusal.to_string();
        assert!(refusal.contains("\"float64\""));
        assert!(refusal.contains("float32, float16, uint8, int32"));
    }
}
