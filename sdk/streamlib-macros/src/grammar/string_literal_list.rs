// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! A list of string literals, `["a", "b"]`, as a port's or a value input's key takes one.

use syn::LitStr;
use syn::parse::ParseStream;

/// Parse `["a", "b"]`, a list of string literals.
pub(super) fn parse_string_literal_list(
    content: ParseStream<'_>,
    key: &str,
) -> syn::Result<Vec<String>> {
    Ok(parse_string_literals(content, key)?
        .iter()
        .map(LitStr::value)
        .collect())
}

/// Parse `["a", "b"]`, a list of string literals, keeping each literal and its span.
pub(super) fn parse_string_literals(
    content: ParseStream<'_>,
    key: &str,
) -> syn::Result<Vec<LitStr>> {
    let list: syn::ExprArray = content.parse().map_err(|_| {
        syn::Error::new(
            content.span(),
            format!("`{key}` takes a list of string literals, `[\"a\", \"b\"]`"),
        )
    })?;
    list.elems
        .iter()
        .map(|element| match element {
            syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(literal),
                ..
            }) => Ok(literal.clone()),
            other => Err(syn::Error::new_spanned(
                other,
                format!("`{key}` takes a list of string literals"),
            )),
        })
        .collect()
}
