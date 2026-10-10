// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A machine of a test's own: the runtime directory the machine runtime's local API socket sits
//! in, the state directory and the machine runtime lock, all under one short root, so no test
//! sees or drives a real runtime. A build with `machine-directories-under-a-test-root` keeps them
//! under `TATOLAB_TEST_MACHINE_ROOT`; one without it is isolated through `XDG_RUNTIME_DIR` and
//! `XDG_STATE_HOME`, which only Linux honours. Integration tests only: it runs the `tatolab`
//! binary, whose path is known to them alone.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use super::stub_local_api_server::{StubLocalApiScript, StubLocalApiServer};

/// The variable a test build of `tatolab` reads its machine root from.
const TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE: &str = "TATOLAB_TEST_MACHINE_ROOT";

/// A socket path is capped near 104 bytes, so the root sits directly under `/tmp`.
const SHORT_MACHINE_ROOT_PARENT: &str = "/tmp";

/// A machine root of a test's own, removed when dropped.
pub struct IsolatedMachineDirectories {
    machine_root: tempfile::TempDir,
}

impl IsolatedMachineDirectories {
    /// A fresh, empty machine: nothing answers at its local API socket.
    pub fn new() -> Self {
        Self {
            machine_root: tempfile::Builder::new()
                .prefix("tl-")
                .tempdir_in(SHORT_MACHINE_ROOT_PARENT)
                .unwrap(),
        }
    }

    /// The root every machine directory sits under.
    pub fn machine_root(&self) -> &Path {
        self.machine_root.path()
    }

    /// The runtime directory `tatolab` resolves on this machine.
    pub fn runtime_directory(&self) -> PathBuf {
        if cfg!(feature = "machine-directories-under-a-test-root") {
            self.machine_root().join("run")
        } else {
            self.xdg_runtime_dir().join("streamlib")
        }
    }

    /// The fixed path the machine's runtime serves its local API on.
    pub fn local_api_socket_path(&self) -> PathBuf {
        self.runtime_directory().join("local-api.sock")
    }

    /// The state directory `tatolab` names the runtime's log under.
    pub fn state_directory(&self) -> PathBuf {
        if cfg!(feature = "machine-directories-under-a-test-root") {
            self.machine_root().join("state")
        } else {
            self.xdg_state_home().join("tatolab")
        }
    }

    fn xdg_runtime_dir(&self) -> PathBuf {
        self.machine_root().join("xdg-run")
    }

    fn xdg_state_home(&self) -> PathBuf {
        self.machine_root().join("xdg-state")
    }

    /// Serve a stub local API answering `stub_local_api_script` at this machine's fixed socket, as
    /// the machine's runtime does.
    pub fn serve_stub_local_api(
        &self,
        stub_local_api_script: StubLocalApiScript,
    ) -> StubLocalApiServer {
        streamlib_runtime_client_contract::directory_at_an_explicit_mode::create_directory_and_its_missing_parents_at_mode(
            &self.runtime_directory(),
            0o700,
        )
        .unwrap();
        StubLocalApiServer::serve_at(&self.local_api_socket_path(), stub_local_api_script)
    }

    /// `tatolab` with `tatolab_arguments`, reading this machine's directories, stdin closed.
    pub fn tatolab_command(&self, tatolab_arguments: &[&str]) -> Command {
        let mut tatolab_command = Command::new(env!("CARGO_BIN_EXE_tatolab"));
        tatolab_command
            .args(tatolab_arguments)
            .env(TEST_MACHINE_ROOT_ENVIRONMENT_VARIABLE, self.machine_root())
            .env("XDG_RUNTIME_DIR", self.xdg_runtime_dir())
            .env("XDG_STATE_HOME", self.xdg_state_home())
            .stdin(Stdio::null());
        tatolab_command
    }

    /// Run `tatolab` with `tatolab_arguments` to its exit on this machine.
    pub fn run_tatolab(&self, tatolab_arguments: &[&str]) -> Output {
        self.tatolab_command(tatolab_arguments).output().unwrap()
    }
}
