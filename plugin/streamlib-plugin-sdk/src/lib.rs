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

// ---- Crate-internal modules carrying the cdylib (vtable-marshal) arm ----
//
// Engine-free twins of the engine's dual-mode authoring types. The host
// `*Inner` backings + `HOST_*_VTABLE` statics stay in the engine; these are
// the `#[repr(C)]` layout-matched twins + the cdylib-side vtable-marshal
// code, re-exported under `sdk::*` below.
mod context;
mod iceoryx2;
mod media_clock;
mod plugin;
mod processors;

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

    // ---- Capability-typed context views (cdylib arm) ----
    /// `RuntimeContext{Full,Limited}Access` + `GpuContext{Full,Limited}Access`
    /// — `#[repr(C)]` twins of the engine's, layout-locked so a host-built
    /// view can be read field-by-field across the plugin ABI.
    pub mod context {
        pub use crate::context::{
            GpuContextFullAccess, GpuContextLimitedAccess, RuntimeContextFullAccess,
            RuntimeContextLimitedAccess,
        };
    }

    // ---- Processor-authoring traits + support types ----
    /// Mode traits, `Config`, `EmptyConfig`, `ProcessorSpec`, the port
    /// markers, and the macro-targeted `__generated_private::GeneratedProcessor`.
    pub mod processors {
        pub use crate::processors::{
            Config, ConfigValidationError, ContinuousProcessor, DynGeneratedProcessor,
            EmptyConfig, GeneratedProcessor, InputPortMarker, ManualProcessor, OutputPortMarker,
            PortMarker, ProcessorSpec, ReactiveProcessor,
        };
        pub use crate::processors::__generated_private;
        /// Re-export so the macro's `sdk::processors::PortSchemaSpec` path
        /// resolves (the macro emits port-spec construction against it).
        pub use streamlib_processor_schema::PortSchemaSpec;
    }

    // ---- iceoryx2 transport views (cdylib arm) ----
    /// `OutputWriter` / `InputMailboxes` PluginAbiObjects, their opaque
    /// `*Inner` placeholders, and `ReadMode`.
    pub mod iceoryx2 {
        pub use crate::iceoryx2::{
            InputMailboxes, InputMailboxesInner, OutputWriter, OutputWriterInner, ReadMode,
        };
    }

    // ---- Plugin registration glue (cdylib arm) ----
    /// `install_host_services` + `RegisterHelper` — the symbols
    /// `export_plugin!` resolves into. Re-exports the ABI's `HostServices`
    /// + layout-version const for the macro's payload handling.
    pub mod plugin {
        pub use crate::plugin::{install_host_services, RegisterHelper};
        pub use streamlib_plugin_abi::{HostServices, HOST_SERVICES_LAYOUT_VERSION};
    }
}
