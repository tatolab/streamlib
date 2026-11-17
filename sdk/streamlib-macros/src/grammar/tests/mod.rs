// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

mod node_attribute_and_node_meta_tests;
mod pin_type_grammar_tests;
mod port_grammar_tests;

fn ident(name: &str) -> Ident {
    Ident::new(name, proc_macro2::Span::call_site())
}

fn parse_ok(tokens: proc_macro2::TokenStream) -> ParsedNodeAttr {
    parse2(tokens, DeclaredNodeMeta::default(), &ident("MyNode")).expect("attribute should parse")
}

fn parse_err(tokens: proc_macro2::TokenStream) -> String {
    match parse2(tokens, DeclaredNodeMeta::default(), &ident("MyNode")) {
        Ok(_) => panic!("attribute should fail to parse"),
        Err(err) => err.to_string(),
    }
}

fn node_struct_with(attributes: proc_macro2::TokenStream) -> syn::ItemStruct {
    syn::parse_quote! {
        #attributes
        struct MyNode;
    }
}

fn node_meta_ok(attributes: proc_macro2::TokenStream) -> DeclaredNodeMeta {
    let mut node_struct = node_struct_with(attributes);
    DeclaredNodeMeta::take_from_attributes(&mut node_struct.attrs)
        .expect("the node's meta should read")
}

fn node_meta_err(attributes: proc_macro2::TokenStream) -> String {
    let mut node_struct = node_struct_with(attributes);
    match DeclaredNodeMeta::take_from_attributes(&mut node_struct.attrs) {
        Ok(_) => panic!("the node's meta should be refused"),
        Err(refusal) => refusal.to_string(),
    }
}

fn parse_ok_with_meta(
    attributes: proc_macro2::TokenStream,
    tokens: proc_macro2::TokenStream,
) -> ParsedNodeAttr {
    parse2(tokens, node_meta_ok(attributes), &ident("MyNode")).expect("attribute should parse")
}
