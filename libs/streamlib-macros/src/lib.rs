// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Procedural macros for streamlib
//!
//! YAML-based processor definition macro:
//!
//! - `#[streamlib::processor("path/to/schema.yaml")]` - Main processor definition
//!
//! # Example
//!
//! ```ignore
//! use streamlib::prelude::*;
//!
//! #[streamlib::processor("schemas/processors/camera.yaml")]
//! pub struct CameraProcessor;
//!
//! impl CameraProcessor::Processor {
//!     fn setup(&mut self, _ctx: &RuntimeContext) -> Result<()> { Ok(()) }
//!     fn teardown(&mut self) -> Result<()> { Ok(()) }
//!     fn process(&mut self) -> Result<()> { Ok(()) }
//! }
//! ```
//!
//! Where `schemas/processors/camera.yaml` contains:
//!
//! ```yaml
//! name: com.tatolab.camera
//! version: 1.0.0
//! description: "Camera capture processor"
//!
//! config:
//!   name: config
//!   schema: com.tatolab.camera.config@1.0.0
//!
//! outputs:
//!   - name: video
//!     schema: com.tatolab.videoframe@1.0.0
//! ```
//!
//! Generates:
//!
//! ```ignore
//! pub mod CameraProcessor {
//!     pub struct Processor { ... }
//!
//!     pub mod InputLink {}
//!     pub mod OutputLink {
//!         pub struct video;
//!     }
//! }
//! ```

mod analysis;
mod attributes;
mod codegen;
mod config_descriptor;

use proc_macro::TokenStream;
use std::path::Path;
use syn::{parse_macro_input, DeriveInput, ItemStruct, LitStr};

/// Main processor attribute macro.
///
/// Transforms a struct definition into a processor module using a YAML schema.
///
/// # Usage
///
/// ```ignore
/// #[streamlib::processor("schemas/processors/my_processor.yaml")]
/// pub struct MyProcessor;
/// ```
///
/// The YAML file defines:
/// - Processor name and version
/// - Description
/// - Runtime (rust, python, typescript)
/// - Config schema reference
/// - Input/output port schemas
///
/// # Generated Code
///
/// - `Processor` struct with port fields
/// - `InputLink` module with input port markers
/// - `OutputLink` module with output port markers
/// - All necessary trait implementations
#[proc_macro_attribute]
pub fn processor(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_struct = parse_macro_input!(item as ItemStruct);

    // Parse YAML path from attribute
    let yaml_path = match parse_yaml_path(attr) {
        Ok(path) => path,
        Err(err) => return err.to_compile_error().into(),
    };

    // Load and parse the YAML schema
    let schema = match load_processor_schema(&yaml_path, &item_struct) {
        Ok(schema) => schema,
        Err(err) => return err.to_compile_error().into(),
    };

    // Generate code from the schema
    let generated = codegen::generate_from_processor_schema(&item_struct, &schema);

    TokenStream::from(generated)
}

/// Parse the YAML path from the attribute arguments.
fn parse_yaml_path(attr: TokenStream) -> syn::Result<String> {
    let lit: LitStr = syn::parse(attr)?;
    Ok(lit.value())
}

/// Load and parse a processor schema from a YAML file.
fn load_processor_schema(
    yaml_path: &str,
    item: &ItemStruct,
) -> syn::Result<streamlib_codegen_shared::ProcessorSchema> {
    // Get the manifest directory (where Cargo.toml is located)
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| {
        syn::Error::new_spanned(
            item,
            "CARGO_MANIFEST_DIR not set. This macro must be used within a Cargo build.",
        )
    })?;

    let full_path = Path::new(&manifest_dir).join(yaml_path);

    // Check if file exists
    if !full_path.exists() {
        return Err(syn::Error::new_spanned(
            item,
            format!(
                "Processor schema file not found: {}\n\
                 Expected at: {}",
                yaml_path,
                full_path.display()
            ),
        ));
    }

    // Read and parse the YAML
    let yaml_content = std::fs::read_to_string(&full_path).map_err(|e| {
        syn::Error::new_spanned(
            item,
            format!(
                "Failed to read processor schema file '{}': {}",
                yaml_path, e
            ),
        )
    })?;

    streamlib_codegen_shared::parse_processor_yaml(&yaml_content).map_err(|e| {
        syn::Error::new_spanned(
            item,
            format!("Failed to parse processor schema '{}': {}", yaml_path, e),
        )
    })
}

/// Derive macro for ConfigDescriptor trait.
///
/// Generates a `ConfigDescriptor` implementation for config structs,
/// enabling automatic config field metadata extraction for processor descriptors.
///
/// # Field Handling
///
/// - `Option<T>` fields are marked as `required: false`
/// - All other fields are marked as `required: true`
/// - Doc comments on fields become the `description`
///
/// # Example
///
/// ```ignore
/// use streamlib::ConfigDescriptor;
///
/// #[derive(ConfigDescriptor)]
/// pub struct CameraConfig {
///     /// Camera device identifier
///     pub device_id: Option<String>,
///     /// Target width in pixels
///     pub width: u32,
///     /// Target height in pixels
///     pub height: u32,
/// }
/// ```
#[proc_macro_derive(ConfigDescriptor)]
pub fn derive_config_descriptor(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    match config_descriptor::derive_config_descriptor(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}
