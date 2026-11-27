// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

mod codegen;
mod field_attribute;
mod grammar;
mod node_settings;

use grammar as attribute_grammar;

use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Item, ItemFn, ItemStruct, parse_macro_input};

/// Declares a node type, on a struct or on a function.
///
/// On a function, the node is written as one function: its body runs once, when the node is set
/// up, declares the node's `computed(…)` values and returns its per-frame function. Its
/// parameters are the node's value inputs as `Input<T>` handles, each declared with
/// `input("name", value = T, default = …)`, a String chosen from a list stating `options = ["…",
/// …]` and an `Option<PinType>` choosing a bundle its stream declares stating
/// `among_the_bundles_its_stream_declares = true`, and named after its input, its `InputMailboxes`, its
/// `OutputWriter` and the `&RuntimeContextFullAccess<'_>` the body runs with.
#[proc_macro_attribute]
pub fn node(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as Item);
    match item {
        Item::Fn(item_fn) => node_function(attr, item_fn),
        Item::Struct(item_struct) => node_struct(attr, item_struct),
        other => syn::Error::new(
            other.span(),
            "#[node(...)] declares a node on a struct, or on a function written as one function",
        )
        .to_compile_error()
        .into(),
    }
}

fn node_function(attr: TokenStream, mut item_fn: ItemFn) -> TokenStream {
    let node_meta =
        match attribute_grammar::DeclaredNodeMeta::take_from_attributes(&mut item_fn.attrs) {
            Ok(node_meta) => node_meta,
            Err(err) => return err.to_compile_error().into(),
        };
    let node_name = match codegen::node_name_of_a_node_function(&item_fn.sig.ident) {
        Ok(node_name) => node_name,
        Err(err) => return err.to_compile_error().into(),
    };
    let parsed = match attribute_grammar::parse2(attr.into(), node_meta, &node_name) {
        Ok(parsed) => parsed,
        Err(err) => return err.to_compile_error().into(),
    };
    TokenStream::from(codegen::generate_from_a_node_function(
        item_fn,
        &parsed,
        sdk_root(),
    ))
}

fn node_struct(attr: TokenStream, mut item_struct: ItemStruct) -> TokenStream {
    let node_meta =
        match attribute_grammar::DeclaredNodeMeta::take_from_attributes(&mut item_struct.attrs) {
            Ok(node_meta) => node_meta,
            Err(err) => return err.to_compile_error().into(),
        };
    let parsed = match attribute_grammar::parse2(attr.into(), node_meta, &item_struct.ident) {
        Ok(parsed) => parsed,
        Err(err) => return err.to_compile_error().into(),
    };
    if let Some(refusal) = codegen::refusal_of_a_value_input_on_a_struct(&parsed) {
        return refusal.to_compile_error().into();
    }

    TokenStream::from(codegen::generate_from_parsed_node_attr(
        &item_struct,
        &parsed,
        sdk_root(),
    ))
}

/// Declares a struct as a node type's settings: each field an input read once, when the node is
/// set up, from its value or from a Value wire into it, described by `#[description = "…"]` and, for a String chosen
/// from a list, `#[options("…", …)]`. A node takes it with `#[node(settings = …)]`.
#[proc_macro_attribute]
pub fn node_settings(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_struct = parse_macro_input!(item as ItemStruct);
    match node_settings::expand_node_settings(attr.into(), item_struct, sdk_root()) {
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
