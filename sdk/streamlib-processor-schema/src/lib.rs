// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Types shared between `streamlib` and `streamlib-macros` for code generation.

mod execution_config;
mod process_execution;
mod thread_priority;

pub mod descriptors;
pub mod error;
pub mod processor_schema;
pub mod processor_schema_parser;
pub mod schema_ident_output;

pub use execution_config::ExecutionConfig;
pub use process_execution::ProcessExecution;
pub use thread_priority::ThreadPriority;

// Processor schema re-exports
pub use error::{SchemaError, SchemaResult};
pub use processor_schema::{
    DELIVERY_PROFILE_DECLARATION_VALUES, ProcessorConfigSchema, ProcessorLanguage,
    ProcessorPortSchema, ProcessorScheduling, ProcessorSchema, ProcessorSchemaExecution,
    ProcessorStateField, RuntimeConfig, RuntimeOptions, to_pascal_case, to_snake_case,
};
pub use processor_schema_parser::{parse_processor_yaml, parse_processor_yaml_file};
pub use schema_ident_output::{SchemaIdentOutput, SemanticVersionOutput};

// Re-export structured-identity types so consumers (the macro, runtime
// loaders) reach `SchemaIdent`, `Org`, `Package`, etc. through this crate
// without depending on `streamlib-idents` directly.
pub use streamlib_idents::{
    ModuleIdent, Org, Package, PackageRef, SchemaIdent, SemVer, SemVerRange, TypeName,
};
