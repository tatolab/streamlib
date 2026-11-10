// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `#[processor_config]`: a config key's `#[description = …]` becomes the
//! description its config schema serves. Each described field's schema goes
//! through `ProcessorConfigKeyDescribedBy<FieldType, Marker>`, whose generated
//! `Marker` holds the text as a `const`, so a literal and a `const` path read
//! alike.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{Attribute, Expr, Fields, Ident, ItemStruct, Meta};

const DESCRIPTION_ATTRIBUTE: &str = "description";

/// Expand `#[processor_config]` on `item`.
pub fn expand_processor_config(
    attribute: TokenStream,
    mut item: ItemStruct,
    sdk_root: TokenStream,
) -> syn::Result<TokenStream> {
    if !attribute.is_empty() {
        return Err(syn::Error::new(
            attribute.span(),
            "#[processor_config] takes no arguments; describe each key with \
             `#[description = \"…\"]` on its field",
        ));
    }
    refuse_a_struct_whose_json_schema_is_not_derived_below(&item)?;
    let Fields::Named(named_fields) = &mut item.fields else {
        return Err(syn::Error::new(
            item.fields.span(),
            "#[processor_config] describes the keys of a struct with named fields",
        ));
    };

    let struct_name = item.ident.to_string();
    let mut description_markers = Vec::new();
    for field in named_fields.named.iter_mut() {
        let Some(description) = take_the_declared_description(&mut field.attrs)? else {
            continue;
        };
        refuse_a_described_field_whose_schema_is_already_replaced(&field.attrs, &description)?;
        let field_name = field
            .ident
            .as_ref()
            .map(|ident| ident.to_string())
            .unwrap_or_default();
        let marker = format_ident!(
            "{}{}ConfigKeyDescription",
            struct_name,
            upper_camel_case(&field_name),
            span = Span::call_site()
        );
        let field_type = &field.ty;
        let described_schema_type = quote! {
            #sdk_root::descriptors::ProcessorConfigKeyDescribedBy<#field_type, #marker>
        }
        .to_string();
        field
            .attrs
            .push(syn::parse_quote! { #[schemars(with = #described_schema_type)] });
        description_markers.push(description_marker(&sdk_root, &marker, &description));
    }

    Ok(quote! {
        #item
        #(#description_markers)*
    })
}

fn description_marker(sdk_root: &TokenStream, marker: &Ident, description: &Expr) -> TokenStream {
    quote! {
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        struct #marker;

        impl #sdk_root::descriptors::ProcessorConfigKeyDescription for #marker {
            const DESCRIPTION: &'static str = #description;
        }
    }
}

/// Removes the field's `#[description = …]` and returns its value, refusing
/// any spelling but a string literal or the path of a `const`.
fn take_the_declared_description(attributes: &mut Vec<Attribute>) -> syn::Result<Option<Expr>> {
    let mut description = None;
    let mut kept_attributes = Vec::with_capacity(attributes.len());
    for attribute in attributes.drain(..) {
        if !attribute.path().is_ident(DESCRIPTION_ATTRIBUTE) {
            kept_attributes.push(attribute);
            continue;
        }
        if description.is_some() {
            return Err(syn::Error::new(
                attribute.span(),
                "a config key declares one `#[description = …]`",
            ));
        }
        let Meta::NameValue(name_value) = &attribute.meta else {
            return Err(description_spelling_refusal(attribute.span()));
        };
        match &name_value.value {
            Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(_),
                ..
            })
            | Expr::Path(_) => description = Some(name_value.value.clone()),
            other => return Err(description_spelling_refusal(other.span())),
        }
    }
    *attributes = kept_attributes;
    Ok(description)
}

fn description_spelling_refusal(span: Span) -> syn::Error {
    syn::Error::new(
        span,
        "`#[description = …]` takes a string literal or the path of a `const` holding one — \
         `#[description = \"…\"]` or `#[description = MY_KEY_DESCRIPTION]`",
    )
}

fn refuse_a_described_field_whose_schema_is_already_replaced(
    attributes: &[Attribute],
    description: &Expr,
) -> syn::Result<()> {
    for attribute in attributes {
        let replaces_the_schema = |nested_keys: &[&str]| -> bool {
            let mut found = false;
            let _ = attribute.parse_nested_meta(|nested| {
                if nested_keys.iter().any(|key| nested.path.is_ident(key)) {
                    found = true;
                }
                if nested.input.peek(syn::Token![=]) {
                    nested.value()?.parse::<Expr>()?;
                } else if nested.input.peek(syn::token::Paren) {
                    nested.parse_nested_meta(|_| Ok(()))?;
                }
                Ok(())
            });
            found
        };
        let refused = (attribute.path().is_ident("schemars")
            && replaces_the_schema(&["with", "schema_with"]))
            || (attribute.path().is_ident("serde") && replaces_the_schema(&["flatten"]));
        if refused {
            return Err(syn::Error::new(
                description.span(),
                "a described config key carries its own type's schema, so it cannot also be \
                 `#[schemars(with = …)]`, `#[schemars(schema_with = …)]` or \
                 `#[serde(flatten)]`",
            ));
        }
    }
    Ok(())
}

fn refuse_a_struct_whose_json_schema_is_not_derived_below(item: &ItemStruct) -> syn::Result<()> {
    let derives_json_schema = item
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("derive"))
        .any(|attribute| {
            let mut derives_it = false;
            let _ = attribute.parse_nested_meta(|derived| {
                if derived
                    .path
                    .segments
                    .last()
                    .is_some_and(|segment| segment.ident == "JsonSchema")
                {
                    derives_it = true;
                }
                Ok(())
            });
            derives_it
        });
    if derives_json_schema {
        return Ok(());
    }
    Err(syn::Error::new(
        item.ident.span(),
        "#[processor_config] goes above `#[derive(JsonSchema)]`, which builds the schema its \
         key descriptions reach",
    ))
}

fn upper_camel_case(snake_case_name: &str) -> String {
    snake_case_name
        .trim_start_matches("r#")
        .split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut characters = word.chars();
            match characters.next() {
                Some(first) => first.to_uppercase().chain(characters).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expanded(item: TokenStream) -> syn::Result<String> {
        expand_processor_config(
            TokenStream::new(),
            syn::parse2(item).expect("a struct"),
            quote! { ::streamlib::sdk },
        )
        .map(|tokens| tokens.to_string())
    }

    #[test]
    fn a_described_key_takes_its_schema_through_a_marker_carrying_the_text() {
        let rendered = expanded(quote! {
            #[derive(JsonSchema)]
            struct CameraConfig {
                #[description = "Frame width in pixels."]
                width: u32,
                #[description = crate::HEIGHT_DESCRIPTION]
                height: u32,
                fps: u32,
            }
        })
        .expect("expands");
        assert!(
            rendered.contains(
                "ProcessorConfigKeyDescribedBy < u32 , CameraConfigWidthConfigKeyDescription >"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains("const DESCRIPTION : & 'static str = \"Frame width in pixels.\""),
            "{rendered}"
        );
        assert!(
            rendered.contains("const DESCRIPTION : & 'static str = crate :: HEIGHT_DESCRIPTION"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("CameraConfigFpsConfigKeyDescription"),
            "{rendered}"
        );
        assert!(!rendered.contains("# [description"), "{rendered}");
    }

    #[test]
    fn a_description_that_is_neither_a_literal_nor_a_const_path_is_refused() {
        let refusal = expanded(quote! {
            #[derive(JsonSchema)]
            struct CameraConfig {
                #[description = include_str!("width.md")]
                width: u32,
            }
        })
        .expect_err("refused");
        assert!(
            refusal
                .to_string()
                .contains("string literal or the path of a `const`")
        );
    }

    #[test]
    fn a_struct_without_a_json_schema_derive_below_is_refused() {
        let refusal = expanded(quote! {
            struct CameraConfig {
                #[description = "Frame width in pixels."]
                width: u32,
            }
        })
        .expect_err("refused");
        assert!(
            refusal
                .to_string()
                .contains("above `#[derive(JsonSchema)]`")
        );
    }

    #[test]
    fn a_described_key_whose_schema_is_already_replaced_is_refused() {
        let refusal = expanded(quote! {
            #[derive(JsonSchema)]
            struct CameraConfig {
                #[description = "Frame width in pixels."]
                #[schemars(with = "String")]
                width: u32,
            }
        })
        .expect_err("refused");
        assert!(refusal.to_string().contains("cannot also be"));
    }
}
