// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A refusal written where no engine log carries it: before the engine
//! exists, or after it is gone.

use std::io::Write;
use std::process::ExitCode;

/// The status `tatolabd` exits with when it refuses.
pub(crate) const EXIT_STATUS_OF_A_REFUSAL: u8 = 1;

/// Write `tatolabd: <reason>` to the process's standard error, and return the
/// refusal's exit status.
pub(crate) fn write_refusal_to_standard_error(reason: &str) -> ExitCode {
    let mut standard_error = std::io::stderr().lock();
    let _ = writeln!(standard_error, "tatolabd: {reason}");
    let _ = standard_error.flush();
    ExitCode::from(EXIT_STATUS_OF_A_REFUSAL)
}
