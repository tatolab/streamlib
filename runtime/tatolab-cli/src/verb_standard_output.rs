// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A verb's standard output and standard error: written whole and flushed, a reader that closed
//! the output pipe ending the verb quietly rather than panicking it (Rust ignores SIGPIPE, so a
//! closed pipe is a write error).

use std::io::{self, Write};

use crate::TatolabCommandFailure;

/// Write `verb_output` to the locked standard output and flush it; exit code 0 when written or
/// when the reader had already closed the pipe.
pub(crate) fn write_verb_standard_output(verb_output: &str) -> Result<u8, TatolabCommandFailure> {
    let mut locked_standard_output = io::stdout().lock();
    match locked_standard_output
        .write_all(verb_output.as_bytes())
        .and_then(|()| locked_standard_output.flush())
    {
        Ok(()) => Ok(0),
        Err(write_failure) => standard_output_closed_or_failed(write_failure),
    }
}

/// Write `standard_error_text` — a note or a warning — to the locked standard error and flush
/// it. Text that cannot be written there has nowhere else to go, so it never changes the verb's
/// outcome.
pub(crate) fn write_verb_standard_error(standard_error_text: &str) {
    let mut locked_standard_error = io::stderr().lock();
    let _written_or_nowhere_to_report_it = locked_standard_error
        .write_all(standard_error_text.as_bytes())
        .and_then(|()| locked_standard_error.flush());
}

/// A reader that closed its end of the pipe has seen all it wanted, which ends the verb quietly;
/// any other failure to write is the verb's.
pub(crate) fn standard_output_closed_or_failed(
    write_failure: io::Error,
) -> Result<u8, TatolabCommandFailure> {
    if write_failure.kind() == io::ErrorKind::BrokenPipe {
        return Ok(0);
    }
    Err(TatolabCommandFailure::refused(format!(
        "cannot write to standard output: {write_failure}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_pipe_ends_the_verb_with_exit_zero() {
        assert_eq!(
            standard_output_closed_or_failed(io::Error::from(io::ErrorKind::BrokenPipe)).unwrap(),
            0
        );
    }

    #[test]
    fn any_other_write_failure_is_a_refusal_naming_it() {
        let refusal = standard_output_closed_or_failed(io::Error::other("disk full")).unwrap_err();
        assert_eq!(refusal.exit_code, 1);
        assert_eq!(
            refusal.message_for_the_user.as_deref(),
            Some("cannot write to standard output: disk full")
        );
    }
}
