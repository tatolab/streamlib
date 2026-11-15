// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use proc_macro2::TokenStream as TokenStream2;
use quote::{ToTokens, quote, quote_spanned};
use syn::parse::ParseStream;
use syn::spanned::Spanned;
use syn::{
    Attribute, Expr, GenericArgument, Ident, ItemFn, LitStr, Path, PathArguments, ReturnType,
    Token, Type,
};

#[cfg(test)]
mod tests;

const PIN_DEFAULT_ATTRIBUTE: &str = "pin_default";

/// The helper attribute a function node's category and description are declared in.
const NODE_META_ATTRIBUTE_NAME: &str = "meta";

/// What a category path's levels are separated by, as `Effects/Stylize`.
const NODE_CATEGORY_PATH_SEPARATOR: char = '/';

pub(crate) fn refuse_an_argument_to_function_node(attribute: TokenStream2) -> syn::Result<()> {
    if attribute.is_empty() {
        return Ok(());
    }
    Err(syn::Error::new(
        attribute.span(),
        "#[function_node] takes no argument: the node's name is its function's, in \
         UpperCamelCase, and what describes it goes in the `#[meta(...)]` under it, \
         `#[meta(category = \"Effects/Stylize\", description = \"…\")]`",
    ))
}

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

pub(crate) fn refuse_a_signature_that_is_not_a_plain_function(
    function: &ItemFn,
) -> syn::Result<()> {
    let signature = &function.sig;
    if let Some(asyncness) = signature.asyncness {
        return Err(syn::Error::new(
            asyncness.span(),
            "a function node computes its outputs and returns; it is not `async`",
        ));
    }
    if let Some(constness) = signature.constness {
        return Err(syn::Error::new(
            constness.span(),
            "a function node is called through its evaluate entry, so it is not `const`",
        ));
    }
    if let Some(unsafety) = signature.unsafety {
        return Err(syn::Error::new(
            unsafety.span(),
            "a function node's body is safe Rust; it is not `unsafe`",
        ));
    }
    if let Some(abi) = &signature.abi {
        return Err(syn::Error::new(
            abi.span(),
            "#[function_node] writes the C entry itself; the function takes no ABI",
        ));
    }
    if !signature.generics.params.is_empty() || signature.generics.where_clause.is_some() {
        return Err(syn::Error::new(
            signature.generics.span(),
            "a function node is one concrete function; it takes no generics",
        ));
    }
    if let Some(variadic) = &signature.variadic {
        return Err(syn::Error::new(
            variadic.span(),
            "every input of a function node is a named pin",
        ));
    }
    Ok(())
}

pub(crate) fn the_output_type(function: &ItemFn, state_record: Option<&Type>) -> syn::Result<Type> {
    let ReturnType::Type(_, returned_type) = &function.sig.output else {
        return Err(syn::Error::new(
            function.sig.span(),
            "a function node returns its output: `-> f64`, `-> Option<f64>` when it may hand \
             out nothing, or a `FunctionNodeRecord` for several outputs",
        ));
    };
    match (stateful_output_and_state(returned_type), state_record) {
        (Some((output_type, returned_state_record)), Some(state_record)) => {
            if returned_state_record.to_token_stream().to_string()
                != state_record.to_token_stream().to_string()
            {
                return Err(syn::Error::new(
                    returned_state_record.span(),
                    "the state a function node returns is the one its next call takes; name \
                     one record in `State<...>` and `Stateful<..., ...>`",
                ));
            }
            Ok(output_type)
        }
        (Some(_), None) => Err(syn::Error::new(
            returned_type.span(),
            "a function node returning `Stateful<...>` takes `previous: State<Record>`, or the \
             state it returns never reaches its next call",
        )),
        (None, Some(_)) => Err(syn::Error::new(
            returned_type.span(),
            "a function node taking `State<Record>` returns `Stateful<Output, Record>`, or its \
             next call never sees a new state",
        )),
        (None, None) => Ok(returned_type.as_ref().clone()),
    }
}

fn stateful_output_and_state(returned_type: &Type) -> Option<(Type, Type)> {
    let segment = the_last_path_segment(returned_type, "Stateful")?;
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    let mut type_arguments = arguments.args.iter().filter_map(|argument| match argument {
        GenericArgument::Type(type_argument) => Some(type_argument.clone()),
        _ => None,
    });
    let output_type = type_arguments.next()?;
    let state_record = type_arguments.next()?;
    Some((output_type, state_record))
}

fn the_last_path_segment<'parsed>(
    parsed_type: &'parsed Type,
    name: &str,
) -> Option<&'parsed syn::PathSegment> {
    let Type::Path(type_path) = parsed_type else {
        return None;
    };
    if type_path.qself.is_some() {
        return None;
    }
    let segment = type_path.path.segments.last()?;
    (segment.ident == name).then_some(segment)
}

pub(crate) fn is_the_bare_type_named(parsed_type: &Type, name: &str) -> bool {
    the_last_path_segment(parsed_type, name)
        .is_some_and(|segment| matches!(segment.arguments, PathArguments::None))
}

pub(crate) fn the_one_type_argument_of(parsed_type: &Type, name: &str) -> Option<Type> {
    let segment = the_last_path_segment(parsed_type, name)?;
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return None;
    };
    if arguments.args.len() != 1 {
        return None;
    }
    match arguments.args.first()? {
        GenericArgument::Type(type_argument) => Some(type_argument.clone()),
        _ => None,
    }
}

pub(crate) fn take_the_pin_default(attributes: &mut Vec<Attribute>) -> syn::Result<Option<Expr>> {
    let mut pin_default: Option<Expr> = None;
    let mut kept_attributes = Vec::with_capacity(attributes.len());
    for attribute in attributes.drain(..) {
        if attribute.path().is_ident(PIN_DEFAULT_ATTRIBUTE) {
            if pin_default.is_some() {
                return Err(syn::Error::new(
                    attribute.span(),
                    "an input pin takes one `#[pin_default(...)]`",
                ));
            }
            pin_default = Some(attribute.parse_args::<Expr>()?);
        } else {
            kept_attributes.push(attribute);
        }
    }
    *attributes = kept_attributes;
    Ok(pin_default)
}
