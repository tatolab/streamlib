// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Procedural macros for streamlib
//!
//! YAML-based processor definition macro:
//!
//! - `#[streamlib::processor("com.tatolab.camera")]` - Processor definition by name lookup
//!
//! The macro reads `CARGO_MANIFEST_DIR/streamlib.yaml` and finds the processor entry
//! matching the given name.

mod analysis;
mod attributes;
mod codegen;
mod config_descriptor;

use proc_macro::TokenStream;
use std::path::Path;
use streamlib_codegen_shared::ProjectConfigMinimal;
use syn::{parse_macro_input, DeriveInput, ItemStruct, LitStr};

/// Main processor attribute macro.
///
/// Transforms a struct definition into a processor module by looking up a processor
/// name in `streamlib.yaml`.
///
/// The macro reads `CARGO_MANIFEST_DIR/streamlib.yaml` and finds the processor entry
/// matching the given name.
#[proc_macro_attribute]
pub fn processor(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_struct = parse_macro_input!(item as ItemStruct);

    // Parse processor name from attribute
    let processor_name = match parse_processor_name(attr) {
        Ok(name) => name,
        Err(err) => return err.to_compile_error().into(),
    };

    // Load and parse the YAML schema
    let schema = match load_processor_schema(&processor_name, &item_struct) {
        Ok(schema) => schema,
        Err(err) => return err.to_compile_error().into(),
    };

    // Generate code from the schema
    let generated = codegen::generate_from_processor_schema(&item_struct, &schema);

    TokenStream::from(generated)
}

/// Parse the processor name from the attribute arguments.
fn parse_processor_name(attr: TokenStream) -> syn::Result<String> {
    let lit: LitStr = syn::parse(attr)?;
    Ok(lit.value())
}

/// Load a processor schema by name from `streamlib.yaml`.
fn load_processor_schema(
    processor_name: &str,
    item: &ItemStruct,
) -> syn::Result<streamlib_codegen_shared::ProcessorSchema> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").map_err(|_| {
        syn::Error::new_spanned(
            item,
            "CARGO_MANIFEST_DIR not set. This macro must be used within a Cargo build.",
        )
    })?;

    let config_path = Path::new(&manifest_dir).join("streamlib.yaml");

    if !config_path.exists() {
        return Err(syn::Error::new_spanned(
            item,
            format!(
                "streamlib.yaml not found at {}\n\
                 The #[streamlib::processor(\"name\")] macro requires a streamlib.yaml\n\
                 next to Cargo.toml with processor definitions.",
                config_path.display()
            ),
        ));
    }

    let yaml_content = std::fs::read_to_string(&config_path).map_err(|e| {
        syn::Error::new_spanned(item, format!("Failed to read streamlib.yaml: {}", e))
    })?;

    let config: ProjectConfigMinimal = serde_yaml::from_str(&yaml_content).map_err(|e| {
        syn::Error::new_spanned(item, format!("Failed to parse streamlib.yaml: {}", e))
    })?;

    let available_names: Vec<String> = config.processors.iter().map(|p| p.name.clone()).collect();

    config
        .processors
        .into_iter()
        .find(|p| p.name == processor_name)
        .ok_or_else(|| {
            let mut msg = format!(
                "Processor '{}' not found in streamlib.yaml\n\
                 Expected at: {}",
                processor_name,
                config_path.display()
            );
            if !available_names.is_empty() {
                msg.push_str("\n  Available processors:");
                for name in &available_names {
                    msg.push_str(&format!("\n    - {}", name));
                }
            }
            syn::Error::new_spanned(item, msg)
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
