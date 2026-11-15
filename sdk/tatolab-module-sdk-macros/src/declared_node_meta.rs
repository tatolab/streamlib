// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use proc_macro2::TokenStream as TokenStream2;
use quote::{quote, quote_spanned};
use syn::parse::ParseStream;
use syn::spanned::Spanned;
use syn::{Attribute, Ident, LitStr, Path, Token};

/// The helper attribute a function node's category and description are declared in.
const NODE_META_ATTRIBUTE_NAME: &str = "meta";

/// What a category path's levels are separated by, as `Effects/Stylize`.
const NODE_CATEGORY_PATH_SEPARATOR: char = '/';

/// What the `#[meta(...)]` attributes under `#[function_node]` declare; the
/// twin of `streamlib-macros`' `grammar::DeclaredNodeMeta`, which reads the
/// same grammar under `#[processor]`. The function's doc comment is never its
/// description.
#[derive(Default)]
pub(crate) struct DeclaredNodeMeta {
    /// The group an editor files the node type under, or a path into a group
    /// inside one, exactly as written.
    pub(crate) category: Option<String>,
    /// A `&'static str` expression: a string literal or the path of a `const`.
    pub(crate) description: Option<TokenStream2>,
}

impl DeclaredNodeMeta {
    /// Take every `#[meta(...)]` off `attributes`, since rustc knows no such attribute, and read what they declare.
    pub(crate) fn take_from_attributes(attributes: &mut Vec<Attribute>) -> syn::Result<Self> {
        let mut declared = DeclaredNodeMeta::default();
        let mut refusal: Option<syn::Error> = None;
        attributes.retain(|attribute| {
            if !attribute.path().is_ident(NODE_META_ATTRIBUTE_NAME) {
                return true;
            }
            if let Err(refused) =
                attribute.parse_args_with(|input: ParseStream<'_>| declared.read_meta_keys(input))
            {
                match &mut refusal {
                    Some(earlier) => earlier.combine(refused),
                    None => refusal = Some(refused),
                }
            }
            false
        });
        match refusal {
            Some(refusal) => Err(refusal),
            None => Ok(declared),
        }
    }

    fn read_meta_keys(&mut self, input: ParseStream<'_>) -> syn::Result<()> {
        if input.is_empty() {
            return Err(syn::Error::new(
                input.span(),
                "`#[meta()]` declares nothing — it takes `category`, `description` or both, \
                 `#[meta(category = \"Effects/Stylize\", description = \"…\")]`",
            ));
        }
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![=]>()?;
            match key.to_string().as_str() {
                "category" => {
                    let category: LitStr = input.parse().map_err(|not_a_literal| {
                        syn::Error::new(
                            not_a_literal.span(),
                            "`category` takes a string literal — `category = \"Effects\"`, or a \
                             path into a group inside one, `category = \"Effects/Stylize\"`",
                        )
                    })?;
                    refuse_a_category_with_a_level_that_has_no_name(&category)?;
                    if self.category.replace(category.value()).is_some() {
                        return Err(declared_more_than_once(&key));
                    }
                }
                "description" => {
                    let description = parse_a_declared_description_str_expression(input)?;
                    if self.description.replace(description).is_some() {
                        return Err(declared_more_than_once(&key));
                    }
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown `#[meta(...)]` key `{other}` — expected `category` or \
                             `description`"
                        ),
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(())
    }
}

fn declared_more_than_once(key: &Ident) -> syn::Error {
    syn::Error::new(
        key.span(),
        format!("`{key}` is declared more than once in this node's `#[meta(...)]`"),
    )
}

fn refuse_a_category_with_a_level_that_has_no_name(category: &LitStr) -> syn::Result<()> {
    if category
        .value()
        .split(NODE_CATEGORY_PATH_SEPARATOR)
        .any(|level| level.trim().is_empty())
    {
        return Err(syn::Error::new(
            category.span(),
            "this category names a group with no name — a category is `\"Effects\"`, or a path \
             whose every level is named, `\"Effects/Stylize\"`",
        ));
    }
    Ok(())
}

/// The `&'static str` expression of a `description = …` value. A constant is
/// bound to a `const` first, so a `static` or a non-`&str` is refused where it
/// is named.
fn parse_a_declared_description_str_expression(
    input: ParseStream<'_>,
) -> syn::Result<TokenStream2> {
    if input.peek(LitStr) {
        let literal: LitStr = input.parse()?;
        return Ok(quote! { #literal });
    }
    let fork = input.fork();
    if fork.parse::<Path>().is_ok() && (fork.is_empty() || fork.peek(Token![,])) {
        let constant: Path = input.parse()?;
        return Ok(quote_spanned! { constant.span() =>
            {
                const DECLARED_DESCRIPTION: &str = #constant;
                DECLARED_DESCRIPTION
            }
        });
    }
    Err(syn::Error::new(
        input.span(),
        "`description` takes a string literal or the path of a `const` holding one — \
         `description = \"…\"` or `description = MY_NODE_DESCRIPTION`",
    ))
}
