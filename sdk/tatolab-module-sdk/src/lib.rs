// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The module host table a Tatolab module's native half reaches its node
//! through: the node's lent ports, its log, the engine's monotonic clock, its
//! stop flag and the host's capabilities, from any thread, without Python.
//!
//! The lent `tatolab.runtime` builds the table; a node's Python half opens it
//! with `ctx.open_module_host_table(output_ports=[...], input_ports=[...])` and
//! hands the capsule to its native half, which reads it with
//! [`ModuleHost::from_python_module_host_table_capsule`] (feature `pyo3`) or
//! [`ModuleHost::from_module_host_table_pointer`].

pub mod module_host;
#[cfg(feature = "pyo3")]
mod module_host_python_capsule;
pub mod module_host_table_abi;
#[cfg(feature = "tracing")]
pub mod module_host_tracing;

pub use module_host::{
    ModuleHost, ModuleHostError, ModuleHostInputPort, ModuleHostLogLevel, ModuleHostOutputPort,
    ModuleHostReadMessage, refuse_a_module_host_table_this_module_cannot_read,
};
pub use module_host_table_abi::{
    MODULE_HOST_TABLE_ABI_MAJOR, MODULE_HOST_TABLE_ABI_MINOR,
    MODULE_HOST_TABLE_PYTHON_CAPSULE_NAME, ModuleHostTable,
};
