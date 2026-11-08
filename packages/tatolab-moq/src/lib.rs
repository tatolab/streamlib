// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The MoQ gateway, track, send and receive nodes' native half.

pub mod error;
pub mod moq_catalog;
pub mod moq_gateway;
pub mod moq_gateway_receive_read;
pub mod moq_gateway_relay_session;
pub mod moq_gateway_relay_settings;
pub mod moq_hang_legacy_container;
pub mod moq_node_ipc;
pub mod moq_node_thread;
mod moq_python_bindings;
pub mod moq_track_receive;
pub mod moq_track_send;

/// The module `tatolab.moq` imports.
#[pyo3::pymodule]
fn _native(module: &pyo3::Bound<'_, pyo3::types::PyModule>) -> pyo3::PyResult<()> {
    moq_python_bindings::add_the_node_classes_to(module)
}
