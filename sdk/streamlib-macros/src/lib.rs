// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

mod codegen;
mod grammar;

use grammar as attribute_grammar;

use proc_macro::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Item, ItemFn, parse_macro_input};

/// Declares a node type written as one function: its body runs once, when the node is set up,
/// declares the node's `computed(…)` values and returns its per-frame function. Its parameters are the node's value inputs as `Input<T>` handles, each declared with
/// `input("name", value = T, default = …)`, a String chosen from a list stating `options = ["…",
/// …]` and an `Option<PinType>` choosing a bundle its stream declares stating
/// `among_the_bundles_its_stream_declares = true`, and named after its input, its `InputMailboxes`, its
/// `OutputWriter` and the `&RuntimeContextFullAccess<'_>` the body runs with.
#[proc_macro_attribute]
pub fn node(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item = parse_macro_input!(item as Item);
    match item {
        Item::Fn(item_fn) => node_function(attr, item_fn),
        Item::Struct(item_struct) => {
            codegen::refusal_of_a_node_not_written_as_a_function(item_struct.ident.span())
                .to_compile_error()
                .into()
        }
        other => codegen::refusal_of_a_node_not_written_as_a_function(other.span())
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
