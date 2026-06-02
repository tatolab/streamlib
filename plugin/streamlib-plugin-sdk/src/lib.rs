// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Engine-free StreamLib plugin-authoring SDK.
//!
//! Packages (plugins) depend on this crate **by its real name**
//! (`streamlib-plugin-sdk`) — never the `streamlib` engine facade — and author
//! processors against `streamlib_plugin_sdk::sdk::*`. Because this crate's
//! dependency graph excludes `streamlib-engine`, a plugin `.slpkg` cdylib that
//! links it does NOT statically link a second copy of the engine. That second
//! copy — and its duplicated process-global state (Vulkan dispatch, signal /
//! panic hooks, PUBSUB, the escalate gate) — is what corrupts the GPU driver
//! during concurrent setup; keeping this zone engine-free by construction makes
//! that mistake impossible to make by accident.
//!
//! The SDK carries the **cdylib (vtable-marshal) arm** of the dual-mode
//! authoring types. The host `*Inner` backings + `HOST_*_VTABLE` impls stay in
//! the engine; the resource-view and context shims here are layout-matched
//! `#[repr(C)]` twins of the engine's, so the host can build a struct and a
//! plugin can read its fields across the plugin ABI soundly.

/// Public plugin-authoring surface. Packages author against
/// `streamlib_plugin_sdk::sdk::*`; the `#[processor]` macro and
/// `export_plugin!` resolve their emitted paths into this module.
pub mod sdk {
    // ---- Canonical Error / Result (engine-free, plugin/ zone) ----
    /// `Error`, `Result`, `PortDirection`.
    pub use streamlib_error as error;

    // ---- Descriptor + identity types (engine-free shared crate) ----
    /// Processor / port descriptor + structured-identity types. Mirrors the
    /// engine's `core::descriptors` union so the macro's
    /// `descriptors::{SchemaIdent, ProcessorDescriptor, …}` paths resolve.
    pub mod descriptors {
        pub use streamlib_processor_schema::descriptors::{
            port_schema_spec_wire, CodeExamples, ConfigDescriptor, ConfigField, PortDescriptor,
            ProcessorDescriptor, ProcessorRuntime,
        };
        pub use streamlib_processor_schema::{
            ModuleIdent, Org, Package, PortSchemaSpec, ProcessorScheduling, SchemaIdent, SemVer,
            SemVerRange, TypeName,
        };
    }

    // ---- Execution mode types (engine-free shared crate) ----
    /// `ProcessExecution`, `ExecutionConfig`, `ThreadPriority`.
    pub mod execution {
        pub use streamlib_processor_schema::{ExecutionConfig, ProcessExecution, ThreadPriority};
    }

    /// `serde_json` re-export — required by macro-emitted `serde_json::to_value`.
    pub use serde_json;

    // ---- Procedural macros (real-name, no aliasing) ----
    /// `#[streamlib_plugin_sdk::sdk::processor("…")]` attribute macro.
    pub use streamlib_macros::processor;
    /// `#[derive(ConfigDescriptor)]` derive macro.
    pub use streamlib_macros::ConfigDescriptor;
    pub use streamlib_macros::{
        module_ident, module_ident_any_version, module_ident_joined,
        module_ident_joined_any_version, schema_ident, schema_ident_any_version,
    };
}
