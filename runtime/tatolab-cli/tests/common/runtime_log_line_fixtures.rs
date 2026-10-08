// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! JSONL records as a runtime writes them into its log files, for the `logs` tests. Shared by the
//! integration tests and, through `#[path]`, the unit tests.

#![allow(dead_code)]

use std::io::Write;
use std::path::Path;

use serde_json::{Value, json};

/// A record as a runtime writes it — `Rabc`'s, from Rust, at info, saying `Creating Runner` — with
/// each field `overrides` names laid over it.
pub fn a_log_record(overrides: Value) -> Value {
    let mut record = json!({
        "schema_version": 1,
        "host_ts": 1_786_136_667_573_387_556_u64,
        "runtime_id": "Rabc",
        "source": "rust",
        "level": "info",
        "message": "Creating Runner",
        "target": "tatolabd",
        "intercepted": false,
    });
    for (field_name, field_value) in overrides.as_object().unwrap() {
        record[field_name] = field_value.clone();
    }
    record
}

/// [`a_log_record`] saying `message`, as one JSONL line.
pub fn a_log_line_with_message(message: &str) -> String {
    log_lines_of(&[a_log_record(json!({"message": message}))])
}

/// [`a_log_record`] saying `message` at `level`, as one JSONL line.
pub fn a_log_line_with_message_at_level(message: &str, level: &str) -> String {
    log_lines_of(&[a_log_record(json!({"message": message, "level": level}))])
}

/// `records`, one JSONL line each.
pub fn log_lines_of(records: &[Value]) -> String {
    records.iter().map(|record| format!("{record}\n")).collect()
}

/// Append `appended_text` to the log file at `log_file_path`, as a runtime's writer does.
pub fn append_to_log_file(log_file_path: &Path, appended_text: &str) {
    std::fs::OpenOptions::new()
        .append(true)
        .open(log_file_path)
        .unwrap()
        .write_all(appended_text.as_bytes())
        .unwrap();
}
