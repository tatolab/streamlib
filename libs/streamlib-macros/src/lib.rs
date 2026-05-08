// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Procedural macros for streamlib.
//!
//! - `#[streamlib::processor("Camera")]` — processor definition by
//!   PascalCase short name. The macro reads `CARGO_MANIFEST_DIR/streamlib.yaml`
//!   and resolves the full structured [`SchemaIdent`] from the package's
//!   `package: { org, name, version }` block plus the matching entry in
//!   the `processors:` list.

mod analysis;
mod attributes;
mod codegen;
mod config_descriptor;

use proc_macro::TokenStream;
use std::path::Path;
use streamlib_processor_schema::{
    PackageMetadata, ProcessorSchema, ProjectConfigMinimal, SchemaIdent, TypeName,
};
use syn::{parse_macro_input, DeriveInput, ItemStruct, LitStr};

/// Main processor attribute macro.
///
/// Transforms a struct definition into a processor module by looking up a
/// PascalCase short name in `streamlib.yaml`'s `processors:` list and
/// composing the full structured `SchemaIdent` from the package's
/// `package: { org, name, version }` block.
///
/// Example: `#[streamlib::processor("Camera")]` resolves to
/// `SchemaIdent { org: tatolab, package: streamlib, type: Camera,
/// version: <package.version> }` when used inside a manifest declaring
/// `package: { org: tatolab, name: streamlib, ... }`.
#[proc_macro_attribute]
pub fn processor(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_struct = parse_macro_input!(item as ItemStruct);

    let short_name = match parse_processor_short_name(attr) {
        Ok(name) => name,
        Err(err) => return err.to_compile_error().into(),
    };

    let (schema, schema_ident) = match load_processor_schema(&short_name, &item_struct) {
        Ok(pair) => pair,
        Err(err) => return err.to_compile_error().into(),
    };

    let generated = codegen::generate_from_processor_schema(&item_struct, &schema, &schema_ident);

    TokenStream::from(generated)
}

/// Parse the processor short name from the attribute argument.
fn parse_processor_short_name(attr: TokenStream) -> syn::Result<String> {
    let lit: LitStr = syn::parse(attr)?;
    Ok(lit.value())
}

/// Locate `CARGO_MANIFEST_DIR/streamlib.yaml`, resolve the package metadata
/// + matching processor entry by short name, and compose the full
/// [`SchemaIdent`].
fn load_processor_schema(
    short_name: &str,
    item: &ItemStruct,
) -> syn::Result<(ProcessorSchema, SchemaIdent)> {
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
                 The #[streamlib::processor(\"<ShortName>\")] macro requires a streamlib.yaml\n\
                 next to Cargo.toml with a `package:` block and a matching processor entry.",
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

    let pkg: PackageMetadata = config.package.ok_or_else(|| {
        syn::Error::new_spanned(
            item,
            format!(
                "streamlib.yaml at {} is missing a `package:` block. The processor macro requires `package: {{ org, name, version }}` to construct a full SchemaIdent for `{}`.",
                config_path.display(),
                short_name
            ),
        )
    })?;

    let available_names: Vec<String> = config.processors.iter().map(|p| p.name.clone()).collect();

    let schema = config
        .processors
        .into_iter()
        .find(|p| p.name == short_name)
        .ok_or_else(|| {
            let mut msg = format!(
                "Processor '{}' not found in streamlib.yaml at {}",
                short_name,
                config_path.display()
            );
            if !available_names.is_empty() {
                msg.push_str("\n  Available processors:");
                for name in &available_names {
                    msg.push_str(&format!("\n    - {}", name));
                }
            }
            syn::Error::new_spanned(item, msg)
        })?;

    let type_name = TypeName::new(short_name).map_err(|e| {
        syn::Error::new_spanned(
            item,
            format!(
                "processor short name `{}` is not valid PascalCase: {}",
                short_name, e
            ),
        )
    })?;

    let ident = SchemaIdent::new(pkg.org, pkg.name, type_name, pkg.version);

    Ok((schema, ident))
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
/// use streamlib::sdk::ConfigDescriptor;
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
