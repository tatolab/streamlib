// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The `tatolab` binary run as a user runs it. Integration tests only: the binary's path is
//! known to them alone.

#![allow(dead_code)]

use std::process::{Command, Output, Stdio};

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
