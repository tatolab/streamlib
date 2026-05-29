// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! JSONL log directory resolution — collocated in the install's
//! generated working tree.

use std::path::PathBuf;

/// Base directory for JSONL log files: `<STREAMLIB_HOME>/.streamlib/logs/`.
/// Collocated in the install's generated working tree
/// ([`get_streamlib_data_dir`]) so logs live in the same self-contained
/// folder as the rest of a runtime's state, and honor the `STREAMLIB_HOME`
/// override.
///
/// [`get_streamlib_data_dir`]: crate::core::streamlib_home::get_streamlib_data_dir
pub fn log_dir() -> PathBuf {
    crate::core::streamlib_home::get_streamlib_data_dir().join("logs")
}

/// Path of the JSONL file for one runtime instance, using
/// `<runtime_id>-<started_at_millis>.jsonl`.
pub fn runtime_log_path(runtime_id: &str, started_at_millis: u128) -> PathBuf {
    log_dir().join(format!("{}-{}.jsonl", runtime_id, started_at_millis))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn log_dir_under_streamlib_home() {
        // SAFETY: test modifies env; `#[serial]` keeps it off the other
        // STREAMLIB_HOME-mutating tests.
        let prev = std::env::var_os("STREAMLIB_HOME");
        unsafe { std::env::set_var("STREAMLIB_HOME", "/tmp/slh-logging-test") };
        assert_eq!(
            log_dir(),
            PathBuf::from("/tmp/slh-logging-test/.streamlib/logs")
        );
        unsafe {
            match prev {
                Some(v) => std::env::set_var("STREAMLIB_HOME", v),
                None => std::env::remove_var("STREAMLIB_HOME"),
            }
        }
    }

    #[test]
    fn runtime_log_path_has_stable_shape() {
        let path = runtime_log_path("Rabc123", 1_700_000_000_000);
        let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(file_name, "Rabc123-1700000000000.jsonl");
    }
}
