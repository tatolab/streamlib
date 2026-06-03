// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Types shared between `streamlib` and `streamlib-macros` for code generation.

mod execution_config;
mod process_execution;
mod streamlib_yaml;
mod thread_priority;

pub mod descriptors;
pub mod error;
pub mod processor_schema;
pub mod processor_schema_parser;

pub use execution_config::ExecutionConfig;
pub use process_execution::ProcessExecution;
pub use streamlib_yaml::StreamlibYaml;
pub use thread_priority::ThreadPriority;

// Processor schema re-exports
pub use error::{SchemaError, SchemaResult};
pub use processor_schema::{
    compute_schema_id, to_pascal_case, to_snake_case, PortSchemaSpec, ProcessorConfigSchema,
    ProcessorLanguage, ProcessorPortSchema, ProcessorScheduling, ProcessorSchema,
    ProcessorSchemaExecution, ProcessorStateField, RuntimeConfig, RuntimeOptions,
};
pub use processor_schema_parser::{parse_processor_yaml, parse_processor_yaml_file};

// Re-export structured-identity types so consumers (the macro, runtime
// loaders) reach `SchemaIdent`, `Org`, `Package`, etc. through this crate
// without depending on `streamlib-idents` directly.
pub use streamlib_idents::{
    ModuleIdent, Org, Package, PackageMetadata, PackageRef, SchemaIdent, SemVer, SemVerRange,
    TypeName,
};

/// Minimal project config for the macro: surfaces the `package:` block (so
/// processor short names can be resolved to a structured [`SchemaIdent`])
/// and the `processors:` list. The resolver in `streamlib-idents` handles
/// the full dependency graph; this is a focused view for codegen.
#[derive(serde::Deserialize)]
pub struct ProjectConfigMinimal {
    #[serde(default)]
    pub package: Option<PackageMetadata>,
    #[serde(default)]
    pub processors: Vec<ProcessorSchema>,
}
