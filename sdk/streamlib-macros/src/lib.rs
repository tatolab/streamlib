// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Procedural macros for streamlib.
//!
//! - `#[streamlib::processor(execution = …, …)]` — processor definition. The
//!   attribute is the single source of truth: execution mode and input/output
//!   ports are declared in code, read from no file at expansion. See
//!   [`grammar`] for the full grammar. What describes the node type, its
//!   `category` and `description`, is the `#[meta(…)]` helper attribute
//!   written under it. It declares no identity — a processor is
//!   named by the import path of the class it is, which the macro captures from
//!   its expansion site.
//! - `#[streamlib::sdk::processor_config]` — a config type whose keys'
//!   `#[description = …]` attributes are the descriptions its schema serves.

mod codegen;
mod grammar;
mod processor_config;

use grammar as attribute_grammar;

use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemStruct, parse_macro_input};

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
    let mut item_struct = parse_macro_input!(item as ItemStruct);

    let node_meta =
        match attribute_grammar::DeclaredNodeMeta::take_from_attributes(&mut item_struct.attrs) {
            Ok(node_meta) => node_meta,
            Err(err) => return err.to_compile_error().into(),
        };
    let parsed = match attribute_grammar::parse2(attr.into(), node_meta, &item_struct.ident) {
        Ok(parsed) => parsed,
        Err(err) => return err.to_compile_error().into(),
    };

    TokenStream::from(codegen::generate_from_parsed_processor_attr(
        &item_struct,
        &parsed,
        sdk_root(),
    ))
}

/// Describe a processor config type's keys: each field's `#[description = …]`,
/// a string literal or the path of a `const`, is its key's description. Goes
/// above `#[derive(JsonSchema)]`.
#[proc_macro_attribute]
pub fn processor_config(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_struct = parse_macro_input!(item as ItemStruct);
    match processor_config::expand_processor_config(attr.into(), item_struct, sdk_root()) {
        Ok(expanded) => expanded.into(),
        Err(refusal) => refusal.to_compile_error().into(),
    }
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
