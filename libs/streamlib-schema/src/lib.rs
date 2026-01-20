// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Schema definition, parsing, and code generation for StreamLib.
//!
//! This crate provides:
//! - YAML schema parsing
//! - Rust/Python/TypeScript code generation
//! - Local schema registry with caching
//!
//! # Example
//!
//! ```
//! use streamlib_schema::{parser, codegen};
//!
//! let yaml = r#"
//! name: com.example.myschema
//! version: 1.0.0
//! fields:
//!   - name: value
//!     type: string
//! "#;
//!
//! let schema = parser::parse_yaml(yaml).unwrap();
//! let rust_code = codegen::generate_rust(&schema).unwrap();
//! ```

pub mod codegen;
pub mod definition;
pub mod error;
pub mod parser;
pub mod registry;

pub use definition::{
    compute_schema_id, Field, FieldType, ProcessExecution, ProcessorConfigSchema,
    ProcessorPortSchema, ProcessorRuntime, ProcessorSchema, ProcessorStateField, SchemaDefinition,
};
pub use error::{Result, SchemaError};
pub use parser::{parse_processor_yaml, parse_processor_yaml_file, parse_yaml, parse_yaml_file};
pub use registry::SchemaRegistry;
