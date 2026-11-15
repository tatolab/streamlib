// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use super::*;

fn function_with(attributes: TokenStream2) -> ItemFn {
    syn::parse_quote! {
        #attributes
        fn halve(x: f64) -> f64 {
            x / 2.0
        }
    }
}

fn node_meta_refusal(attributes: TokenStream2) -> String {
    let mut function = function_with(attributes);
    match DeclaredNodeMeta::take_from_attributes(&mut function.attrs) {
        Ok(_) => panic!("the node's meta should be refused"),
        Err(refusal) => refusal.to_string(),
    }
}

#[test]
fn reading_a_node_meta_takes_it_off_the_function_and_leaves_every_other_attribute() {
    let mut function = function_with(quote! {
        /// Half of `x`.
        #[meta(category = "Math")]
        #[inline]
        #[meta(description = "Half of `x`.")]
    });
    let declared = DeclaredNodeMeta::take_from_attributes(&mut function.attrs)
        .expect("the node's meta should read");
    assert_eq!(declared.category.as_deref(), Some("Math"));
    assert!(declared.description.is_some());
    let left_on_the_function: Vec<String> = function
        .attrs
        .iter()
        .map(|attribute| attribute.path().to_token_stream().to_string())
        .collect();
    assert_eq!(left_on_the_function, ["doc", "inline"]);
}

#[test]
fn a_function_with_no_meta_declares_no_category_and_no_description() {
    let mut function = function_with(quote! {});
    let declared = DeclaredNodeMeta::take_from_attributes(&mut function.attrs)
        .expect("no meta reads as nothing declared");
    assert!(declared.category.is_none());
    assert!(declared.description.is_none());
}

#[test]
fn an_argument_to_function_node_is_refused_naming_where_a_description_goes() {
    let refusal = refuse_an_argument_to_function_node(quote! { description = "Half of `x`." })
        .expect_err("an argument is refused")
        .to_string();
    assert!(refusal.contains("takes no argument"), "got: {refusal}");
    assert!(refusal.contains("#[meta("), "got: {refusal}");
    assert!(refuse_an_argument_to_function_node(quote! {}).is_ok());
}

#[test]
fn a_meta_key_declared_twice_is_refused_naming_the_key() {
    let refusal = node_meta_refusal(quote! {
        #[meta(category = "Math")]
        #[meta(category = "Logic")]
    });
    assert!(
        refusal.contains("`category` is declared more than once"),
        "got: {refusal}"
    );
}

#[test]
fn a_meta_that_declares_nothing_is_refused() {
    let refusal = node_meta_refusal(quote! { #[meta()] });
    assert!(refusal.contains("declares nothing"), "got: {refusal}");
}

#[test]
fn an_unknown_meta_key_is_refused_naming_it_and_the_keys_there_are() {
    let refusal = node_meta_refusal(quote! { #[meta(tags = "math")] });
    assert!(
        refusal.contains("unknown `#[meta(...)]` key `tags`"),
        "got: {refusal}"
    );
    assert!(
        refusal.contains("`category` or `description`"),
        "got: {refusal}"
    );
}

#[test]
fn a_category_with_a_level_that_has_no_name_is_refused() {
    for category in [
        "",
        "  ",
        "/Math",
        "Math/",
        "Math//Rounding",
        "Math/ /Rounding",
    ] {
        let refusal = node_meta_refusal(quote! { #[meta(category = #category)] });
        assert!(
            refusal.contains("names a group with no name"),
            "category {category:?} got: {refusal}"
        );
    }
}

#[test]
fn a_category_that_is_not_a_string_literal_is_refused() {
    let refusal = node_meta_refusal(quote! { #[meta(category = MATH_CATEGORY)] });
    assert!(
        refusal.contains("`category` takes a string literal"),
        "got: {refusal}"
    );
}

#[test]
fn a_description_that_is_neither_a_literal_nor_a_const_path_is_refused() {
    let refusal = node_meta_refusal(quote! { #[meta(description = include_str!("README.md"))] });
    assert!(
        refusal.contains("string literal or the path of a `const`"),
        "got: {refusal}"
    );
}

#[test]
fn every_meta_that_is_refused_is_reported_and_none_is_left_on_the_function() {
    let mut function = function_with(quote! {
        #[meta(tags = "math")]
        #[inline]
        #[meta(category = "")]
    });
    let refusals: Vec<String> = DeclaredNodeMeta::take_from_attributes(&mut function.attrs)
        .err()
        .expect("both metas are refused")
        .into_iter()
        .map(|refusal| refusal.to_string())
        .collect();
    assert_eq!(refusals.len(), 2, "got: {refusals:?}");
    assert_eq!(function.attrs.len(), 1);
}
