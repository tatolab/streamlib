// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Procedural macros for streamlib.
//!
//! - `#[streamlib::processor(execution = …, …)]` — processor definition. The
//!   attribute is the single source of truth: execution mode and input/output
//!   ports are declared in code, read from no file at expansion. See
//!   [`grammar`] for the full grammar. It declares no identity — a processor is
//!   named by the import path of the class it is, which the macro captures from
//!   its expansion site.

mod codegen;
mod config_descriptor;
mod grammar;

use grammar as attribute_grammar;

use proc_macro::TokenStream;
use quote::quote;
use syn::{DeriveInput, ItemStruct, parse_macro_input};

/// Main processor attribute macro.
///
/// The attribute is the single source of truth for a processor's execution mode
/// and ports — see [`grammar`] for the grammar. It reads no file at expansion
/// and declares no identity.
///
/// Authoring contract for struct fields: `#[cfg]`, `///` docs, and the lint
/// controls (`allow` / `warn` / `deny` / `forbid`) are forwarded onto the
/// generated field; `#[cfg]` alone is also forwarded onto its `from_config`
/// initializer. `#[expect]` is not forwarded — the generated field is `pub`
/// where the authored one was private, so a `dead_code` expectation would land
/// unfulfilled and warn. `#[cfg_attr]` on a field is a compile error, because a
/// `cfg_attr` expanding to a `cfg` would gate one of the two emission sites and
/// not the other. Every other field attribute is dropped.
///
/// The macro emits the processor's type, port markers, and descriptor — but
/// does NOT register the processor in the global `PROCESSOR_REGISTRY`. In-process Rust callers invoke
/// `PROCESSOR_REGISTRY.register::<Foo::Processor>()` directly; tests and
/// engine-internal mocks use this path.
#[proc_macro_attribute]
pub fn processor(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_struct = parse_macro_input!(item as ItemStruct);

    let parsed = match attribute_grammar::parse2(attr.into(), &item_struct.ident) {
        Ok(parsed) => parsed,
        Err(err) => return err.to_compile_error().into(),
    };

    let schema = parsed.to_processor_schema();
    let config_field_name = parsed
        .config_type
        .as_ref()
        .map(|_| parsed.config_field_name.clone());

    let generated = codegen::generate_from_processor_schema(
        &item_struct,
        &schema,
        parsed.config_type.as_ref(),
        config_field_name.as_deref(),
        parsed.config_schema_id.as_deref(),
        sdk_root(),
    );

    TokenStream::from(generated)
}

/// Resolve the path to the `sdk` module the emitted code authors against.
///
/// Plugin packages depend on `streamlib-plugin-sdk` (the engine-free SDK) by
/// its real name; hosts depend on the `streamlib` facade. Detected per
/// invocation from the consumer's `Cargo.toml` (the `serde_derive` pattern),
/// so emitted paths use the consumer's real crate name with no `streamlib`
/// aliasing. Falls back to `::streamlib::sdk` for in-engine macro use, which
/// resolves via the engine's `extern crate self as streamlib`.
fn sdk_root() -> proc_macro2::TokenStream {
    use proc_macro_crate::{FoundCrate, crate_name};
    fn as_sdk_path(found: FoundCrate) -> proc_macro2::TokenStream {
        match found {
            FoundCrate::Itself => quote! { crate::sdk },
            FoundCrate::Name(name) => {
                let ident = proc_macro2::Ident::new(&name, proc_macro2::Span::call_site());
                quote! { ::#ident::sdk }
            }
        }
    }
    // Prefer the engine-free plugin SDK — packages depend on it by real name.
    if let Ok(found) = crate_name("streamlib-plugin-sdk") {
        return as_sdk_path(found);
    }
    // Host consumers depend on the `streamlib` facade.
    if let Ok(found) = crate_name("streamlib") {
        return as_sdk_path(found);
    }
    // In-engine macro use: `extern crate self as streamlib` resolves this.
    quote! { ::streamlib::sdk }
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
