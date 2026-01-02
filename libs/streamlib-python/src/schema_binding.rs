// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Python bindings for schema creation.
//!
//! Provides the `create_schema` function that creates a Rust-backed
//! `DynamicDataFrameSchema` from Python field definitions and registers
//! it in the global `SCHEMA_REGISTRY`.

use pyo3::prelude::*;
use pyo3::types::PyList;
use std::sync::Arc;
use streamlib::core::links::LinkBufferReadMode;
use streamlib::core::schema::{
    DataFrameSchemaField, DynamicDataFrameSchema, PrimitiveType, SemanticVersion,
};
use streamlib::core::schema_registry::SCHEMA_REGISTRY;

/// Python-visible wrapper for a Rust schema.
#[pyclass(name = "Schema")]
#[derive(Clone)]
pub struct PySchema {
    inner: Arc<DynamicDataFrameSchema>,
}

impl PySchema {
    pub fn inner(&self) -> &DynamicDataFrameSchema {
        &self.inner
    }
}

#[pymethods]
impl PySchema {
    /// Get the schema name.
    #[getter]
    fn name(&self) -> &str {
        use streamlib::core::schema::DataFrameSchema;
        self.inner.name()
    }

    /// Get the total byte size of the schema.
    #[getter]
    fn byte_size(&self) -> usize {
        use streamlib::core::schema::DataFrameSchema;
        self.inner.byte_size()
    }

    /// Get the number of fields.
    fn field_count(&self) -> usize {
        use streamlib::core::schema::DataFrameSchema;
        self.inner.fields().len()
    }

    /// Get field names.
    fn field_names(&self) -> Vec<String> {
        use streamlib::core::schema::DataFrameSchema;
        self.inner.fields().iter().map(|f| f.name.clone()).collect()
    }

    fn __repr__(&self) -> String {
        use streamlib::core::schema::DataFrameSchema;
        format!(
            "Schema(name='{}', fields={}, byte_size={})",
            self.inner.name(),
            self.inner.fields().len(),
            self.inner.byte_size()
        )
    }
}

/// Parse a primitive type string to PrimitiveType.
fn parse_primitive_type(type_str: &str) -> PyResult<PrimitiveType> {
    match type_str {
        "bool" => Ok(PrimitiveType::Bool),
        "i32" => Ok(PrimitiveType::I32),
        "i64" => Ok(PrimitiveType::I64),
        "u32" => Ok(PrimitiveType::U32),
        "u64" => Ok(PrimitiveType::U64),
        "f32" => Ok(PrimitiveType::F32),
        "f64" => Ok(PrimitiveType::F64),
        _ => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "Unknown primitive type: '{}'. Expected one of: bool, i32, i64, u32, u64, f32, f64",
            type_str
        ))),
    }
}

/// Create a Rust-backed schema from Python field definitions.
///
/// This function is called by the @schema decorator to create a
/// DynamicDataFrameSchema from Python field descriptors and register
/// it in the global SCHEMA_REGISTRY.
///
/// Args:
///     name: Schema name for registry.
///     fields: List of field dictionaries with keys:
///         - name: Field name
///         - primitive_type: Type string ("f32", "i64", "bool", etc.)
///         - shape: List of dimensions (empty for scalar)
///         - description: Field description
///
/// Returns:
///     PySchema wrapper around the created schema.
#[pyfunction]
pub fn create_schema(name: String, fields: &Bound<'_, PyList>) -> PyResult<PySchema> {
    let mut schema_fields = Vec::new();

    for field in fields.iter() {
        // Extract field attributes from dict
        let field_name: String = field.get_item("name")?.extract()?;
        let primitive_type_str: String = field.get_item("primitive_type")?.extract()?;
        let shape: Vec<usize> = field.get_item("shape")?.extract()?;
        let description: String = field.get_item("description")?.extract()?;

        // Parse primitive type
        let primitive = parse_primitive_type(&primitive_type_str)?;

        // Create schema field
        let schema_field = DataFrameSchemaField {
            name: field_name,
            description,
            type_name: primitive_type_str,
            shape,
            internal: false,
            primitive: Some(primitive),
        };

        schema_fields.push(schema_field);
    }

    // Create the dynamic schema
    let dynamic_schema = DynamicDataFrameSchema::new(name.clone(), schema_fields.clone());

    // Register in the global SCHEMA_REGISTRY so Rust code can access it
    // Use default settings for DataFrame schemas
    if let Err(e) = SCHEMA_REGISTRY.register_dataframe_schema(
        name.clone(),
        SemanticVersion::new(1, 0, 0),
        schema_fields,
        LinkBufferReadMode::SkipToLatest,
        16, // default capacity
    ) {
        // Log warning but don't fail - schema might already be registered
        tracing::warn!("Failed to register schema '{}' in registry: {}", name, e);
    }

    Ok(PySchema {
        inner: Arc::new(dynamic_schema),
    })
}

/// Check if a schema is registered in the global registry.
#[pyfunction]
pub fn schema_exists(name: &str) -> bool {
    SCHEMA_REGISTRY.contains(name)
}

/// List all registered schema names.
#[pyfunction]
pub fn list_schemas() -> Vec<String> {
    SCHEMA_REGISTRY
        .list()
        .iter()
        .map(|e| e.name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_primitive_type() {
        assert_eq!(parse_primitive_type("f32").unwrap(), PrimitiveType::F32);
        assert_eq!(parse_primitive_type("i64").unwrap(), PrimitiveType::I64);
        assert_eq!(parse_primitive_type("bool").unwrap(), PrimitiveType::Bool);
        assert!(parse_primitive_type("invalid").is_err());
    }
}
