// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Execution configuration and runtime loop.

pub mod node_scheduling_loop;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod readable_wake_descriptor;
pub mod thread_runner;

pub use streamlib_node_schema::{ExecutionConfig, ProcessExecution, ThreadPriority};
pub use thread_runner::run_node_loop;
