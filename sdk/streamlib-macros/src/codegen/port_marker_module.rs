// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


use proc_macro2::TokenStream;
use quote::quote;

use crate::grammar::{ParsedPort, PortDirection};

/// Generate a port-marker module (`InputLink` / `OutputLink`): one marker per
/// declared port, named by its declared spelling and carrying its cast name.
pub(super) fn generate_port_marker_module(
    direction: PortDirection,
    declared_ports: &[ParsedPort],
    module_visibility: &TokenStream,
    marker_visibility: &TokenStream,
) -> TokenStream {
    let module_name = direction.port_marker_module_ident();
    let port_marker_trait = direction.port_marker_trait_ident();
    let port_markers: Vec<TokenStream> = declared_ports
        .iter()
        .map(|port| {
            let marker_name = &port.declared_marker_ident;
            let cast_name = &port.cast_name;
            quote! {
                #marker_visibility struct #marker_name;
                impl super::__streamlib_sdk::processors::#port_marker_trait for #marker_name {
                    const PORT_NAME: &'static str = #cast_name;
                    type Processor = super::Processor;
                }
            }
        })
        .collect();

    // A marker is named by the author's spelling, `video_in` as often as
    // `VideoIn`, and its span is the author's literal, so rustc would lint it.
    quote! {
        #[allow(non_camel_case_types, reason = "the generated module is named after the author's port names")]
        #module_visibility mod #module_name {
            #(#port_markers)*
        }
    }
}
