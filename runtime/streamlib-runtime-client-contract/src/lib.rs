// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What a running StreamLib runtime leaves on disk for its clients, defined once
//! for the runtime that writes it and every client that reads it.
//!
//! Links no engine, so a client such as the native `tatolab` CLI reads exactly
//! what the runtime wrote with the code the runtime wrote it with.

pub mod directory_at_an_explicit_mode;
pub mod node_registry;
pub mod runtime_log_event;
pub mod runtime_log_event_pretty_rendering;
pub mod runtime_log_file_paths;
pub mod streamlib_home;
pub mod streamlib_runtime_directory;

#[cfg(test)]
mod test_support;
