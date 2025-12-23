// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Types shared between `streamlib` and `streamlib-macros` for code generation.

mod execution_config;
mod process_execution;
mod thread_priority;

pub use execution_config::ExecutionConfig;
pub use process_execution::ProcessExecution;
pub use thread_priority::ThreadPriority;
