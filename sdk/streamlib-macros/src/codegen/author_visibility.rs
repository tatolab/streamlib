// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


use std::num::NonZeroUsize;

use proc_macro2::TokenStream;
use quote::quote;

/// The items directly inside the module the macro names after the author's struct.
pub(super) const GENERATED_MODULE_MEMBER_DEPTH: NonZeroUsize = NonZeroUsize::MIN;

/// The port markers inside that module's `InputLink` / `OutputLink`.
pub(super) const GENERATED_PORT_MARKER_DEPTH: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);

pub(super) fn author_visibility_written_from_a_nested_module(
    visibility: &syn::Visibility,
    depth: NonZeroUsize,
) -> TokenStream {
    let supers = std::iter::repeat_n(quote! { super }, depth.get());
    match visibility {
        syn::Visibility::Public(_) => quote! { pub },
        syn::Visibility::Inherited => quote! { pub(in #(#supers)::*) },
        syn::Visibility::Restricted(restricted) => {
            let mut segments = restricted.path.segments.iter();
            let first = segments.next().map(|segment| segment.ident.to_string());
            let rest: Vec<_> = segments.collect();
            match first.as_deref() {
                Some("self") => quote! { pub(in #(#supers)::* #(:: #rest)*) },
                Some("super") => quote! { pub(in #(#supers)::* :: super #(:: #rest)*) },
                _ => quote! { #visibility },
            }
        }
    }
}
