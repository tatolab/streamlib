// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A node registry of a test's own, so no test sees — or prunes — a real runtime's entry, and the
//! entries a runtime writes into it. Shared by the integration tests and, through `#[path]`, the
//! unit tests.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use streamlib_runtime_client_contract::node_registry::{
    NODE_REGISTRY_SCHEMA_VERSION, NodeRegistryEntry, write_entry,
};

/// The name of the folder the runtime directory keeps inside `$XDG_RUNTIME_DIR`.
const STREAMLIB_FOLDER_INSIDE_XDG_RUNTIME_DIR: &str = "streamlib";

/// A pid that names no process: the largest `pid_t`, which neither Linux (`PID_MAX_LIMIT` is
/// 2^22) nor macOS hands out, so `kill(pid, 0)` answers `ESRCH`.
pub const PID_NO_PROCESS_HAS: u32 = i32::MAX as u32;

/// A pid outside `pid_t`, which only a corrupt entry could carry.
pub const PID_OUTSIDE_PID_T: u32 = 4_000_000_000;

/// A local API socket path nothing listens on: its directory does not exist, so a connect fails
/// at once rather than waiting on a slow answer.
pub const NOTHING_LISTENS_LOCAL_API_SOCKET_PATH: &str =
    "/nonexistent-tatolab-test/local-api-Rnone.sock";

/// An `$XDG_RUNTIME_DIR` of a test's own, holding the runtime directory and its node registry.
pub struct IsolatedNodeRegistry {
    xdg_runtime_dir: tempfile::TempDir,
}

impl IsolatedNodeRegistry {
    /// A fresh, empty one; the registry directory itself is not created until an entry is written.
    pub fn new() -> Self {
        Self {
            xdg_runtime_dir: tempfile::tempdir().unwrap(),
        }
    }

    /// The directory to hand a runtime or `tatolab` as `XDG_RUNTIME_DIR`.
    pub fn xdg_runtime_dir(&self) -> &Path {
        self.xdg_runtime_dir.path()
    }

    /// The registry directory a reader resolves under that `XDG_RUNTIME_DIR` on Linux.
    pub fn node_registry_directory(&self) -> PathBuf {
        self.xdg_runtime_dir
            .path()
            .join(STREAMLIB_FOLDER_INSIDE_XDG_RUNTIME_DIR)
            .join("nodes")
    }

    /// Write `node_registry_entry` the way its runtime does, answering the entry file's path.
    pub fn write_registry_entry(&self, node_registry_entry: &NodeRegistryEntry) -> PathBuf {
        write_entry(&self.node_registry_directory(), node_registry_entry).unwrap()
    }

    /// Write `entry_file_contents` verbatim as `<file_stem>.json`, for an entry no runtime would
    /// write.
    pub fn write_registry_entry_file(
        &self,
        file_stem: &str,
        entry_file_contents: &[u8],
    ) -> PathBuf {
        let node_registry_directory = self.node_registry_directory();
        std::fs::create_dir_all(&node_registry_directory).unwrap();
        let entry_file_path = node_registry_directory.join(format!("{file_stem}.json"));
        std::fs::write(&entry_file_path, entry_file_contents).unwrap();
        entry_file_path
    }
}

/// An entry for a runtime with `runtime_id` answering on `local_api_socket_path`, named
/// `rig-app-<runtime_id>` and hosted by this test's own process, which is unambiguously alive.
pub fn a_registry_entry(runtime_id: &str, local_api_socket_path: &Path) -> NodeRegistryEntry {
    NodeRegistryEntry {
        schema_version: NODE_REGISTRY_SCHEMA_VERSION,
        runtime_id: runtime_id.to_owned(),
        runtime_name: format!("rig-app-{runtime_id}"),
        local_api_socket_path: local_api_socket_path.to_path_buf(),
        pid: std::process::id(),
        hint: "tatolabd (/tmp/app)".to_owned(),
    }
}

/// [`a_registry_entry`], named `runtime_name`.
pub fn a_registry_entry_named(
    runtime_id: &str,
    runtime_name: &str,
    local_api_socket_path: &Path,
) -> NodeRegistryEntry {
    NodeRegistryEntry {
        runtime_name: runtime_name.to_owned(),
        ..a_registry_entry(runtime_id, local_api_socket_path)
    }
}

/// [`a_registry_entry`], hosted by `pid`.
pub fn a_registry_entry_hosted_by(
    runtime_id: &str,
    local_api_socket_path: &Path,
    pid: u32,
) -> NodeRegistryEntry {
    NodeRegistryEntry {
        pid,
        ..a_registry_entry(runtime_id, local_api_socket_path)
    }
}
