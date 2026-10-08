// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `tatolab` binary run as a user runs it. Integration tests only: the binary's path is
//! known to them alone.

#![allow(dead_code)]

use std::path::Path;
use std::process::{Command, Output, Stdio};

/// Run `tatolab` with `tatolab_arguments`, reading the runtime directory under
/// `xdg_runtime_dir` — which only Linux honours, so a test isolating the registry this way is
/// Linux-only.
pub fn run_tatolab_with_xdg_runtime_dir(
    xdg_runtime_dir: &Path,
    tatolab_arguments: &[&str],
) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tatolab"))
        .args(tatolab_arguments)
        .env("XDG_RUNTIME_DIR", xdg_runtime_dir)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// Run `tatolab` with `tatolab_arguments` for a verb that reads no runtime directory, such as a
/// `--help` or a usage error.
pub fn run_tatolab_reading_no_runtime_directory(tatolab_arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tatolab"))
        .args(tatolab_arguments)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// A finished run's stdout, as text.
pub fn standard_output_text(finished_run: &Output) -> String {
    String::from_utf8(finished_run.stdout.clone()).unwrap()
}

/// A finished run's stderr, as text.
pub fn standard_error_text(finished_run: &Output) -> String {
    String::from_utf8(finished_run.stderr.clone()).unwrap()
}
