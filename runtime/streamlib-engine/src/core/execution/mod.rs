// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Execution configuration and runtime loop.

pub mod processor_scheduling_loop;
pub mod thread_runner;

// Re-export from streamlib-processor-schema (shared with macros crate)
pub use streamlib_processor_schema::{ExecutionConfig, ProcessExecution, ThreadPriority};
pub use thread_runner::run_processor_loop;
