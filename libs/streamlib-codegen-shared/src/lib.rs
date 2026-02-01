// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Types shared between `streamlib` and `streamlib-macros` for code generation.

mod execution_config;
mod process_execution;
mod thread_priority;

pub mod error;
pub mod processor_schema;
pub mod processor_schema_parser;

pub use execution_config::ExecutionConfig;
pub use process_execution::ProcessExecution;
pub use thread_priority::ThreadPriority;

// Processor schema re-exports
pub use error::{SchemaError, SchemaResult};
pub use processor_schema::{
    compute_schema_id, to_pascal_case, to_snake_case, ProcessorConfigSchema, ProcessorLanguage,
    ProcessorPortSchema, ProcessorSchema, ProcessorSchemaExecution, ProcessorStateField,
    RuntimeConfig, RuntimeOptions,
};
pub use processor_schema_parser::{parse_processor_yaml, parse_processor_yaml_file};
