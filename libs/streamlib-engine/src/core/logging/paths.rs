// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! XDG state directory resolution for JSONL log files.

use std::path::PathBuf;

/// Base directory for JSONL log files:
/// `$XDG_STATE_HOME/streamlib/logs/`, falling back to
/// `~/.local/state/streamlib/logs/` when the env var is unset or empty.
pub fn log_dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("streamlib").join("logs")
}

/// Path of the JSONL file for one runtime instance, using
/// `<runtime_id>-<started_at_millis>.jsonl`.
pub fn runtime_log_path(runtime_id: &str, started_at_millis: u128) -> PathBuf {
    log_dir().join(format!("{}-{}.jsonl", runtime_id, started_at_millis))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_dir_uses_xdg_when_set() {
        // SAFETY: test modifies env; tests using this key must run serialized.
        unsafe { std::env::set_var("XDG_STATE_HOME", "/tmp/xdg-logging-test") };
        let dir = log_dir();
        assert!(dir.starts_with("/tmp/xdg-logging-test/streamlib/logs"));
        unsafe { std::env::remove_var("XDG_STATE_HOME") };
    }

    #[test]
    fn runtime_log_path_has_stable_shape() {
        let path = runtime_log_path("Rabc123", 1_700_000_000_000);
        let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(file_name, "Rabc123-1700000000000.jsonl");
    }
}
