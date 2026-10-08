// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The contract a running StreamLib runtime offers the clients on its machine:
//! the files it leaves on disk and the names its local API speaks, defined once
//! for the runtime and every client.
//!
//! Links no engine, so a client such as the native `tatolab` CLI reads exactly
//! what the runtime wrote, and calls exactly what it serves, with the code the
//! runtime itself uses.

pub mod directory_at_an_explicit_mode;
pub mod local_api_wire_contract;
pub mod node_registry;
pub mod runtime_log_event;
pub mod runtime_log_event_pretty_rendering;
pub mod runtime_log_file_paths;
pub mod streamlib_home;
pub mod streamlib_runtime_directory;

#[cfg(test)]
mod test_support;
