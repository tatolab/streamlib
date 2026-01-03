// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration tests for Python schema registration.
//!
//! These tests verify that schemas created from Python are properly
//! registered in the Rust SCHEMA_REGISTRY.

use pyo3::prelude::*;
use pyo3::types::{IntoPyDict, PyList};
use streamlib::core::schema::PrimitiveType;
use streamlib::core::schema_registry::SCHEMA_REGISTRY;

/// Test that a schema created via create_schema is registered in SCHEMA_REGISTRY.
#[test]
fn test_python_schema_registered_in_rust() {
    #[allow(deprecated)]
    pyo3::prepare_freethreaded_python();

    #[allow(deprecated)]
    Python::with_gil(|py| {
        // Create field definitions as Python would
        let fields = PyList::new(
            py,
            vec![
                // embedding: f32[512]
                [
                    ("name", "embedding".into_pyobject(py).unwrap().into_any()),
                    (
                        "primitive_type",
                        "f32".into_pyobject(py).unwrap().into_any(),
                    ),
                    (
                        "shape",
                        vec![512usize].into_pyobject(py).unwrap().into_any(),
                    ),
                    (
                        "description",
                        "Test embedding".into_pyobject(py).unwrap().into_any(),
                    ),
                ]
                .into_py_dict(py)
                .unwrap(),
                // timestamp: i64
                [
                    ("name", "timestamp".into_pyobject(py).unwrap().into_any()),
                    (
                        "primitive_type",
                        "i64".into_pyobject(py).unwrap().into_any(),
                    ),
                    (
                        "shape",
                        Vec::<usize>::new().into_pyobject(py).unwrap().into_any(),
                    ),
                    (
                        "description",
                        "Timestamp".into_pyobject(py).unwrap().into_any(),
                    ),
                ]
                .into_py_dict(py)
                .unwrap(),
                // active: bool
                [
                    ("name", "active".into_pyobject(py).unwrap().into_any()),
                    (
                        "primitive_type",
                        "bool".into_pyobject(py).unwrap().into_any(),
                    ),
                    (
                        "shape",
                        Vec::<usize>::new().into_pyobject(py).unwrap().into_any(),
                    ),
                    (
                        "description",
                        "Is active".into_pyobject(py).unwrap().into_any(),
                    ),
                ]
                .into_py_dict(py)
                .unwrap(),
            ],
        )
        .unwrap();

        // Call create_schema (this registers in SCHEMA_REGISTRY)
        let schema_name = "TestPythonSchema";
        let result =
            streamlib_python::schema_binding::create_schema(schema_name.to_string(), &fields);
        assert!(result.is_ok(), "create_schema should succeed");

        // Verify schema is registered in SCHEMA_REGISTRY
        assert!(
            SCHEMA_REGISTRY.contains(schema_name),
            "Schema '{}' should be registered in SCHEMA_REGISTRY",
            schema_name
        );

        // Verify schema fields
        let entry = SCHEMA_REGISTRY.get(schema_name).unwrap();
        assert_eq!(entry.fields.len(), 3);

        // Check embedding field
        let embedding = entry.fields.iter().find(|f| f.name == "embedding").unwrap();
        assert_eq!(embedding.primitive, Some(PrimitiveType::F32));
        assert_eq!(embedding.shape, vec![512]);

        // Check timestamp field
        let timestamp = entry.fields.iter().find(|f| f.name == "timestamp").unwrap();
        assert_eq!(timestamp.primitive, Some(PrimitiveType::I64));
        assert!(timestamp.shape.is_empty());

        // Check active field
        let active = entry.fields.iter().find(|f| f.name == "active").unwrap();
        assert_eq!(active.primitive, Some(PrimitiveType::Bool));
    });
}

/// Test that schema_exists correctly reports registered schemas.
#[test]
fn test_schema_exists() {
    #[allow(deprecated)]
    pyo3::prepare_freethreaded_python();

    // Built-in schemas should exist
    assert!(
        streamlib_python::schema_binding::schema_exists("VideoFrame"),
        "VideoFrame should be registered"
    );
    assert!(
        streamlib_python::schema_binding::schema_exists("AudioFrame"),
        "AudioFrame should be registered"
    );

    // Non-existent schema should not exist
    assert!(
        !streamlib_python::schema_binding::schema_exists("NonExistentSchema123"),
        "NonExistentSchema123 should not be registered"
    );
}

/// Test that list_schemas returns all registered schemas.
#[test]
fn test_list_schemas() {
    #[allow(deprecated)]
    pyo3::prepare_freethreaded_python();

    let schemas = streamlib_python::schema_binding::list_schemas();

    // Should include built-in schemas
    assert!(
        schemas.contains(&"VideoFrame".to_string()),
        "Should include VideoFrame"
    );
    assert!(
        schemas.contains(&"AudioFrame".to_string()),
        "Should include AudioFrame"
    );
}
