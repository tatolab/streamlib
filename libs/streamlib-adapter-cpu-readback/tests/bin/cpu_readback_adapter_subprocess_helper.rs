// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Subprocess helper binary for cpu-readback adapter integration
//! tests. Each `tests/*.rs` file that needs a child process spawns
//! this binary with a role argument and a `STREAMLIB_HELPER_SOCKET_FD`
//! env var pointing at the inherited end of a `socketpair`. The role
//! determines what the helper does — read a pattern, write a pattern,
//! crash mid-write, etc.

#![cfg(target_os = "linux")]

fn main() {
    // Placeholder — wired up in `subprocess_crash_mid_write.rs`.
    let role = std::env::args().nth(1).unwrap_or_else(|| "noop".into());
    eprintln!("cpu-readback subprocess helper started: role={role}");
}
