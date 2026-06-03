// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Processor and port descriptor types for introspection.
//!
//! These live in the engine-free `streamlib-processor-schema` crate so the
//! engine, the `#[processor]` macro, and the plugin-SDK all share one
//! definition — `ProcessorDescriptor` crosses the cdylib plugin ABI as
//! `rmp_serde` msgpack, so host and plugin must agree byte-for-byte. The
//! engine re-exports them here so every `crate::core::descriptors::*` path
//! resolves unchanged.

pub use streamlib_processor_schema::descriptors::{
    port_schema_spec_wire, CodeExamples, ConfigDescriptor, ConfigField, PortDescriptor,
    ProcessorDescriptor, ProcessorRuntime,
};
pub use streamlib_processor_schema::{
    ModuleIdent, Org, Package, PortSchemaSpec, ProcessorScheduling, SchemaIdent, SemVer,
    SemVerRange, TypeName,
};
