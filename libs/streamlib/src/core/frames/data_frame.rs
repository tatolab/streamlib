// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::links::LinkPortMessage;
use crate::core::schema::DataFrameSchema;
use std::sync::Arc;

// Implement sealed trait
impl crate::core::links::LinkPortMessageImplementor for DataFrame {}

/// Schema-based data frame for ML/inference, commands, events, etc.
#[derive(Clone)]
pub struct DataFrame {
    /// The schema defining field layout.
    pub schema: Arc<dyn DataFrameSchema>,

    /// CPU memory buffer containing field data.
    pub data: Vec<u8>,

    /// Timestamp in nanoseconds (monotonic).
    pub timestamp_ns: i64,
}

impl DataFrame {
    /// Create a new DataFrame with zeroed data.
    pub fn new(schema: Arc<dyn DataFrameSchema>, timestamp_ns: i64) -> Self {
        Self {
            data: vec![0u8; schema.byte_size()],
            schema,
            timestamp_ns,
        }
    }

    /// Create a new DataFrame with provided data buffer.
    pub fn from_data(
        schema: Arc<dyn DataFrameSchema>,
        data: Vec<u8>,
        timestamp_ns: i64,
    ) -> Result<Self, DataFrameError> {
        let expected = schema.byte_size();
        if data.len() != expected {
            return Err(DataFrameError::SizeMismatch {
                expected,
                actual: data.len(),
            });
        }
        Ok(Self {
            schema,
            data,
            timestamp_ns,
        })
    }

    /// Get a slice of f32 values for a field.
    pub fn get_f32_slice(&self, field: &str) -> Result<&[f32], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        let count = size / std::mem::size_of::<f32>();
        let ptr = self.data[offset..].as_ptr() as *const f32;
        Ok(unsafe { std::slice::from_raw_parts(ptr, count) })
    }

    /// Get a mutable slice of f32 values for a field.
    pub fn get_f32_slice_mut(&mut self, field: &str) -> Result<&mut [f32], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        let count = size / std::mem::size_of::<f32>();
        let ptr = self.data[offset..].as_mut_ptr() as *mut f32;
        Ok(unsafe { std::slice::from_raw_parts_mut(ptr, count) })
    }

    /// Get a slice of f64 values for a field.
    pub fn get_f64_slice(&self, field: &str) -> Result<&[f64], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        let count = size / std::mem::size_of::<f64>();
        let ptr = self.data[offset..].as_ptr() as *const f64;
        Ok(unsafe { std::slice::from_raw_parts(ptr, count) })
    }

    /// Get a mutable slice of f64 values for a field.
    pub fn get_f64_slice_mut(&mut self, field: &str) -> Result<&mut [f64], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        let count = size / std::mem::size_of::<f64>();
        let ptr = self.data[offset..].as_mut_ptr() as *mut f64;
        Ok(unsafe { std::slice::from_raw_parts_mut(ptr, count) })
    }

    /// Get a slice of i32 values for a field.
    pub fn get_i32_slice(&self, field: &str) -> Result<&[i32], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        let count = size / std::mem::size_of::<i32>();
        let ptr = self.data[offset..].as_ptr() as *const i32;
        Ok(unsafe { std::slice::from_raw_parts(ptr, count) })
    }

    /// Get a mutable slice of i32 values for a field.
    pub fn get_i32_slice_mut(&mut self, field: &str) -> Result<&mut [i32], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        let count = size / std::mem::size_of::<i32>();
        let ptr = self.data[offset..].as_mut_ptr() as *mut i32;
        Ok(unsafe { std::slice::from_raw_parts_mut(ptr, count) })
    }

    /// Get a slice of i64 values for a field.
    pub fn get_i64_slice(&self, field: &str) -> Result<&[i64], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        let count = size / std::mem::size_of::<i64>();
        let ptr = self.data[offset..].as_ptr() as *const i64;
        Ok(unsafe { std::slice::from_raw_parts(ptr, count) })
    }

    /// Get a mutable slice of i64 values for a field.
    pub fn get_i64_slice_mut(&mut self, field: &str) -> Result<&mut [i64], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        let count = size / std::mem::size_of::<i64>();
        let ptr = self.data[offset..].as_mut_ptr() as *mut i64;
        Ok(unsafe { std::slice::from_raw_parts_mut(ptr, count) })
    }

    /// Get a slice of u32 values for a field.
    pub fn get_u32_slice(&self, field: &str) -> Result<&[u32], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        let count = size / std::mem::size_of::<u32>();
        let ptr = self.data[offset..].as_ptr() as *const u32;
        Ok(unsafe { std::slice::from_raw_parts(ptr, count) })
    }

    /// Get a mutable slice of u32 values for a field.
    pub fn get_u32_slice_mut(&mut self, field: &str) -> Result<&mut [u32], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        let count = size / std::mem::size_of::<u32>();
        let ptr = self.data[offset..].as_mut_ptr() as *mut u32;
        Ok(unsafe { std::slice::from_raw_parts_mut(ptr, count) })
    }

    /// Get a slice of u64 values for a field.
    pub fn get_u64_slice(&self, field: &str) -> Result<&[u64], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        let count = size / std::mem::size_of::<u64>();
        let ptr = self.data[offset..].as_ptr() as *const u64;
        Ok(unsafe { std::slice::from_raw_parts(ptr, count) })
    }

    /// Get a mutable slice of u64 values for a field.
    pub fn get_u64_slice_mut(&mut self, field: &str) -> Result<&mut [u64], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        let count = size / std::mem::size_of::<u64>();
        let ptr = self.data[offset..].as_mut_ptr() as *mut u64;
        Ok(unsafe { std::slice::from_raw_parts_mut(ptr, count) })
    }

    /// Get a slice of bool values for a field (stored as u8: 0 = false, non-zero = true).
    pub fn get_bool_slice(&self, field: &str) -> Result<&[u8], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        Ok(&self.data[offset..offset + size])
    }

    /// Get a mutable slice of bool values for a field.
    pub fn get_bool_slice_mut(&mut self, field: &str) -> Result<&mut [u8], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        Ok(&mut self.data[offset..offset + size])
    }

    /// Get raw byte slice for a field.
    pub fn get_raw_slice(&self, field: &str) -> Result<&[u8], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        Ok(&self.data[offset..offset + size])
    }

    /// Get mutable raw byte slice for a field.
    pub fn get_raw_slice_mut(&mut self, field: &str) -> Result<&mut [u8], DataFrameError> {
        let (offset, size) = self
            .schema
            .field_layout(field)
            .ok_or_else(|| DataFrameError::FieldNotFound(field.to_string()))?;

        Ok(&mut self.data[offset..offset + size])
    }
}

impl LinkPortMessage for DataFrame {
    fn schema_name() -> &'static str {
        "DataFrame"
    }

    fn schema() -> std::sync::Arc<crate::core::Schema> {
        std::sync::Arc::clone(&crate::core::SCHEMA_DATA_MESSAGE)
    }
}

/// Errors that can occur when accessing DataFrame fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataFrameError {
    /// Field not found in schema.
    FieldNotFound(String),
    /// Type mismatch when accessing field.
    TypeMismatch(String),
    /// Data buffer size does not match schema byte size.
    SizeMismatch { expected: usize, actual: usize },
}

impl std::fmt::Display for DataFrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataFrameError::FieldNotFound(name) => write!(f, "Field not found: {}", name),
            DataFrameError::TypeMismatch(name) => write!(f, "Type mismatch for field: {}", name),
            DataFrameError::SizeMismatch { expected, actual } => {
                write!(
                    f,
                    "Data buffer size mismatch: expected {} bytes, got {}",
                    expected, actual
                )
            }
        }
    }
}

impl std::error::Error for DataFrameError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::schema::{DataFrameSchemaField, DynamicDataFrameSchema, PrimitiveType};

    fn create_test_schema() -> Arc<dyn DataFrameSchema> {
        Arc::new(DynamicDataFrameSchema::new(
            "test_schema".to_string(),
            vec![
                DataFrameSchemaField {
                    name: "embedding".to_string(),
                    description: String::new(),
                    type_name: "f32".to_string(),
                    primitive: Some(PrimitiveType::F32),
                    shape: vec![4],
                    internal: false,
                },
                DataFrameSchemaField {
                    name: "timestamp".to_string(),
                    description: String::new(),
                    type_name: "i64".to_string(),
                    primitive: Some(PrimitiveType::I64),
                    shape: vec![],
                    internal: false,
                },
                DataFrameSchemaField {
                    name: "active".to_string(),
                    description: String::new(),
                    type_name: "bool".to_string(),
                    primitive: Some(PrimitiveType::Bool),
                    shape: vec![],
                    internal: false,
                },
            ],
        ))
    }

    #[test]
    fn test_dataframe_creation() {
        let schema = create_test_schema();
        let frame = DataFrame::new(schema.clone(), 12345);

        assert_eq!(frame.timestamp_ns, 12345);
        assert_eq!(frame.data.len(), schema.byte_size());
        // 4 * f32 (16) + 1 * i64 (8) + 1 * bool (1) = 25 bytes
        assert_eq!(frame.data.len(), 25);
    }

    #[test]
    fn test_dataframe_f32_access() {
        let schema = create_test_schema();
        let mut frame = DataFrame::new(schema, 0);

        // Write values
        {
            let slice = frame.get_f32_slice_mut("embedding").unwrap();
            slice[0] = 1.0;
            slice[1] = 2.0;
            slice[2] = 3.0;
            slice[3] = 4.0;
        }

        // Read values
        let slice = frame.get_f32_slice("embedding").unwrap();
        assert_eq!(slice, &[1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn test_dataframe_i64_access() {
        let schema = create_test_schema();
        let mut frame = DataFrame::new(schema, 0);

        {
            let slice = frame.get_i64_slice_mut("timestamp").unwrap();
            slice[0] = 9876543210;
        }

        let slice = frame.get_i64_slice("timestamp").unwrap();
        assert_eq!(slice[0], 9876543210);
    }

    #[test]
    fn test_dataframe_bool_access() {
        let schema = create_test_schema();
        let mut frame = DataFrame::new(schema, 0);

        {
            let slice = frame.get_bool_slice_mut("active").unwrap();
            slice[0] = 1; // true
        }

        let slice = frame.get_bool_slice("active").unwrap();
        assert_eq!(slice[0], 1);
    }

    #[test]
    fn test_dataframe_field_not_found() {
        let schema = create_test_schema();
        let frame = DataFrame::new(schema, 0);

        let result = frame.get_f32_slice("nonexistent");
        assert!(matches!(result, Err(DataFrameError::FieldNotFound(_))));
    }

    #[test]
    fn test_dataframe_clone() {
        let schema = create_test_schema();
        let mut frame = DataFrame::new(schema, 12345);

        {
            let slice = frame.get_f32_slice_mut("embedding").unwrap();
            slice[0] = 42.0;
        }

        let cloned = frame.clone();
        assert_eq!(cloned.timestamp_ns, 12345);
        assert_eq!(cloned.get_f32_slice("embedding").unwrap()[0], 42.0);
    }
}
