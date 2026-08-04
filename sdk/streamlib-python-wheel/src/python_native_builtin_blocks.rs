// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The wheel-exported names for the native media built-ins.
//!
//! `streamlib.TestPatternSource` is a marker class: never instantiated, never
//! subclassed, carrying no Python behavior. `Runtime.add` recognizes the type
//! object itself and resolves it to the statically-linked native processor —
//! per-frame paths never enter the interpreter.

use pyo3::prelude::*;
use streamlib::sdk::descriptors::{Org, Package, TypeName};
use streamlib::sdk::processors::ProcessorTypeReference;

/// `streamlib.TestPatternSource` — SMPTE-style color bars, no hardware.
///
/// No `#[new]`: instantiating a marker is always a mistake, and PyO3's
/// "no constructor defined" error says so.
#[pyclass(name = "TestPatternSource", module = "streamlib", frozen)]
pub(crate) struct PythonTestPatternSourceBlock;

#[pymethods]
impl PythonTestPatternSourceBlock {
    /// The class is named `Test*`, which pytest would otherwise collect as a
    /// test class; this attribute tells it not to.
    #[classattr]
    #[pyo3(name = "__test__")]
    fn dunder_test() -> bool {
        false
    }
}

/// Resolve a Python object to a native built-in's type reference, if it is
/// one of the wheel-exported marker type objects.
pub(crate) fn native_builtin_type_reference(
    python: Python<'_>,
    processor_class: &Bound<'_, PyAny>,
) -> Option<ProcessorTypeReference> {
    if processor_class.is(python.get_type::<PythonTestPatternSourceBlock>()) {
        return Some(ProcessorTypeReference::new(
            Org::new("tatolab").expect("static org ident"),
            Package::new("media-builtins").expect("static package ident"),
            TypeName::new("TestPatternSource").expect("static type ident"),
        ));
    }
    None
}

/// Register the native built-in processor types on the process-wide registry.
/// Idempotent; called once at module import.
pub(crate) fn register_native_builtin_processor_types() {
    streamlib_media_builtins::register_media_builtin_processor_types();
}
