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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let role = std::env::args().nth(1).unwrap_or_else(|| "noop".into());
    tracing::info!(%role, "cpu-readback subprocess helper started");
}
