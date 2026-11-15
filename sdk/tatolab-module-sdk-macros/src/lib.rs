// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! `#[function_node]` and `#[derive(FunctionNodeRecord)]`, re-exported by
//! `tatolab-module-sdk`'s `function-node` feature, whose `function_node` module
//! carries the grammar.

mod codegen;
mod grammar;

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use syn::{DeriveInput, ItemFn, parse_macro_input};

use crate::codegen::{expand_function_node, expand_function_node_record};
use crate::grammar::{DeclaredNodeMeta, refuse_an_argument_to_function_node};

/// Export a function as a function node: its evaluate entry and a
/// `<function>_function_node_declaration()` its module's Python half hands
/// `node.rust_function(...)`.
#[proc_macro_attribute]
pub fn function_node(attribute: TokenStream, item: TokenStream) -> TokenStream {
    if let Err(refusal) = refuse_an_argument_to_function_node(TokenStream2::from(attribute)) {
        return refusal.to_compile_error().into();
    }
    let mut function = parse_macro_input!(item as ItemFn);
    let node_meta = match DeclaredNodeMeta::take_from_attributes(&mut function.attrs) {
        Ok(node_meta) => node_meta,
        Err(refusal) => return refusal.to_compile_error().into(),
    };
    match expand_function_node(function, node_meta) {
        Ok(expanded) => expanded.into(),
        Err(refusal) => refusal.to_compile_error().into(),
    }
}

/// Implement `FunctionNodeRecord` and `FunctionNodeOutput` for a struct of
/// `f64`, `i64` and `bool` fields, or only `FunctionNodeOutput` when a field is
/// an `Option` or an `UnreadMessage`.
#[proc_macro_derive(FunctionNodeRecord)]
pub fn derive_function_node_record(item: TokenStream) -> TokenStream {
    let record = parse_macro_input!(item as DeriveInput);
    match expand_function_node_record(record) {
        Ok(expanded) => expanded.into(),
        Err(refusal) => refusal.to_compile_error().into(),
    }
}
