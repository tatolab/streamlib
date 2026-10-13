// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The system's available memory, as the kernel estimates it in
//! `/proc/meminfo`.

use crate::core::{Error, Result};

const PROC_MEMINFO_PATH: &str = "/proc/meminfo";

/// The system's available memory in bytes, from `/proc/meminfo`'s `MemAvailable`.
pub(crate) fn read_system_memory_available_bytes_from_proc_meminfo() -> Result<u64> {
    let proc_meminfo_text = std::fs::read_to_string(PROC_MEMINFO_PATH)?;
    mem_available_bytes_in_proc_meminfo_text(&proc_meminfo_text).ok_or_else(|| {
        Error::NotFound(format!(
            "{PROC_MEMINFO_PATH} carries no `MemAvailable: <n> kB` line"
        ))
    })
}

/// `MemAvailable` from `/proc/meminfo` text, in bytes; `None` when the line is
/// absent or not in the kernel's `kB` (KiB) unit.
fn mem_available_bytes_in_proc_meminfo_text(proc_meminfo_text: &str) -> Option<u64> {
    let mem_available = proc_meminfo_text
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))?;
    let mut value_and_unit = mem_available.split_whitespace();
    let kibibytes: u64 = value_and_unit.next()?.parse().ok()?;
    match value_and_unit.next() {
        Some("kB") => kibibytes.checked_mul(1024),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mem_available_is_read_from_proc_meminfo_in_kibibytes() {
        let proc_meminfo_text = "MemTotal:       65771092 kB\n\
                                 MemFree:         1203348 kB\n\
                                 MemAvailable:   41234560 kB\n\
                                 Buffers:          912340 kB\n";
        assert_eq!(
            mem_available_bytes_in_proc_meminfo_text(proc_meminfo_text),
            Some(41_234_560 * 1024)
        );
    }

    #[test]
    fn a_proc_meminfo_without_mem_available_reads_as_unknown() {
        assert_eq!(
            mem_available_bytes_in_proc_meminfo_text(
                "MemTotal:       65771092 kB\nMemFree:         1203348 kB\n"
            ),
            None
        );
        assert_eq!(
            mem_available_bytes_in_proc_meminfo_text("MemAvailable:   41234560 MB\n"),
            None,
            "a unit the kernel never prints is not guessed at"
        );
        assert_eq!(mem_available_bytes_in_proc_meminfo_text(""), None);
    }

    #[test]
    fn this_hosts_proc_meminfo_reads_a_nonzero_mem_available() {
        let available = read_system_memory_available_bytes_from_proc_meminfo()
            .expect("every kernel since 3.14 prints MemAvailable");
        assert!(available > 0);
    }
}
