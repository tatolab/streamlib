// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! JSONL log directory resolution — collocated in the install's
//! generated working tree.

use std::path::{Path, PathBuf};

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

/// Path a rotated segment of `active_segment_path` is renamed to:
/// `<runtime_id>-<started_at_millis>.<rotation_sequence>.jsonl`.
///
/// The sequence is dot-separated because a pinned `STREAMLIB_RUNTIME_ID` may
/// carry dashes, and `camera-2-1700000000000.jsonl` would read two ways.
pub(crate) fn rotated_runtime_log_segment_path(
    active_segment_path: &Path,
    rotation_sequence: u64,
) -> PathBuf {
    active_segment_path.with_extension(format!("{rotation_sequence}.jsonl"))
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

    /// The literal `test_cli_observation_verbs.py` parses back out; the two
    /// sides are held together by this string, not by shared code.
    #[test]
    fn a_rotated_segment_is_named_with_a_dot_separated_sequence() {
        let active_segment_path = runtime_log_path("Rabc123", 1_700_000_000_000);
        let rotated = rotated_runtime_log_segment_path(&active_segment_path, 3);

        assert_eq!(rotated.parent(), active_segment_path.parent());
        assert_eq!(
            rotated.file_name().unwrap(),
            "Rabc123-1700000000000.3.jsonl"
        );
    }

    #[test]
    fn a_runtime_id_carrying_dots_and_dashes_keeps_its_whole_name_when_rotated() {
        let active_segment_path = Path::new("/logs/my.node-2-1700000000000.jsonl");

        assert_eq!(
            rotated_runtime_log_segment_path(active_segment_path, 12),
            Path::new("/logs/my.node-2-1700000000000.12.jsonl")
        );
    }
}
