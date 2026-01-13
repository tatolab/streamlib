// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Subprocess isolation infrastructure for language processors.
//!
//! This module provides the core infrastructure for running language processors
//! (Python, Deno, etc.) in isolated subprocesses, enabling:
//!
//! - True dependency isolation (different versions in different processors)
//! - Crash isolation (subprocess crash doesn't take down runtime)
//! - Own interpreter per subprocess (e.g., separate GIL for Python)
//! - Zero-copy GPU frame sharing via XPC (macOS) or DMA-BUF (Linux)
//! - Minimal-copy CPU frame sharing via xpc_shmem (macOS) or memfd (Linux)

mod process_handle;

pub use process_handle::{ProcessHandle, SubprocessConfig};
