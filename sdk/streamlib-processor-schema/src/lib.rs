// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Types shared between `streamlib` and `streamlib-macros` for code generation.

mod execution_config;
mod process_execution;
mod thread_priority;

pub mod audio_window_contract;
pub mod descriptors;
pub mod error;
pub mod processor_class_import_path;
pub mod processor_class_short_name;
pub mod processor_schema;
pub mod processor_schema_parser;

pub use execution_config::ExecutionConfig;
pub use process_execution::ProcessExecution;
pub use thread_priority::ThreadPriority;

// Processor schema re-exports
pub use audio_window_contract::{
    AUDIO_WINDOW_CHANNELS_FOLLOWING_THE_SOURCE, AUDIO_WINDOW_DTYPE_DECLARATION_VALUES,
    AudioWindowContract, AudioWindowContractDeclaredValues,
    refuse_audio_window_beside_a_skipping_delivery_profile, render_declaration_values,
};
pub use error::{SchemaError, SchemaResult};
pub use processor_class_import_path::ProcessorClassImportPath;
pub use processor_class_short_name::ProcessorClassShortName;
pub use processor_schema::{
    DELIVERY_PROFILE_DECLARATION_VALUES, ProcessorConfigSchema, ProcessorLanguage,
    ProcessorPortSchema, ProcessorScheduling, ProcessorSchema, ProcessorSchemaExecution,
    ProcessorStateField, RuntimeConfig, RuntimeOptions, to_pascal_case, to_snake_case,
};
pub use processor_schema_parser::{parse_processor_yaml, parse_processor_yaml_file};
