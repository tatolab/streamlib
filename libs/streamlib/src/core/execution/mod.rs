// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Execution configuration and runtime loop.

pub mod thread_runner;

// Re-export from streamlib-codegen-shared (shared with macros crate)
pub use streamlib_codegen_shared::{ExecutionConfig, ProcessExecution, ThreadPriority};
pub use thread_runner::run_processor_loop;
