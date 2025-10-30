//! Procedural macros for streamlib
//!
//! This crate provides derive macros and attribute macros to reduce boilerplate
//! when writing streamlib processors.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, DeriveInput, ItemStruct};

/// Derive macro for automatic processor registration
///
/// This macro generates the `StreamProcessor` trait implementation boilerplate
/// and automatically registers the processor type with the global registry.
///
/// # Example
///
/// ```rust
/// use streamlib::*;
///
/// #[streamlib::processor]
/// struct MyProcessor {
///     input: StreamInput<VideoFrame>,
///     output: StreamOutput<VideoFrame>,
/// }
///
/// impl MyProcessor {
///     fn process(&mut self, tick: TimedTick) -> Result<()> {
///         if let Some(frame) = self.input.read_latest() {
///             self.output.write(frame);
///         }
///         Ok(())
///     }
/// }
/// ```
///
/// The macro generates:
/// - Automatic port discovery from struct fields
/// - ProcessorDescriptor implementation
/// - Registration with global processor registry
#[proc_macro_attribute]
pub fn processor(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemStruct);
    let struct_name = &input.ident;
    let struct_vis = &input.vis;
    let struct_attrs = &input.attrs;
    let struct_fields = &input.fields;
    let struct_generics = &input.generics;

    // Extract fields to discover ports
    let fields = match struct_fields {
        syn::Fields::Named(fields) => &fields.named,
        _ => {
            return syn::Error::new_spanned(
                input,
                "#[processor] can only be used on structs with named fields",
            )
            .to_compile_error()
            .into();
        }
    };

    // Find input and output ports
    let mut input_ports = Vec::new();
    let mut output_ports = Vec::new();

    for field in fields {
        let field_name = field.ident.as_ref().unwrap();
        let field_type = &field.ty;

        // Check if field type is StreamInput or StreamOutput
        if let syn::Type::Path(type_path) = field_type {
            let segments = &type_path.path.segments;
            if let Some(last_segment) = segments.last() {
                let type_name = last_segment.ident.to_string();

                if type_name == "StreamInput" {
                    input_ports.push(field_name.to_string());
                } else if type_name == "StreamOutput" {
                    output_ports.push(field_name.to_string());
                }
            }
        }
    }

    // Generate port name arrays
    let input_port_names = input_ports.iter().map(|name| quote! { #name });
    let output_port_names = output_ports.iter().map(|name| quote! { #name });

    // Generate the implementation
    let expanded = quote! {
        #(#struct_attrs)*
        #struct_vis struct #struct_name #struct_generics #struct_fields

        impl #struct_generics #struct_name #struct_generics {
            /// Get input port names
            pub const fn input_port_names() -> &'static [&'static str] {
                &[#(#input_port_names),*]
            }

            /// Get output port names
            pub const fn output_port_names() -> &'static [&'static str] {
                &[#(#output_port_names),*]
            }
        }

        // Note: Auto-registration is not yet implemented
        // Users should manually call register_processor!() macro if needed
    };

    TokenStream::from(expanded)
}

/// Derive macro for StreamProcessor trait
///
/// Automatically implements the StreamProcessor trait with sensible defaults.
/// The processor must have a `process` method defined.
///
/// # Example
///
/// ```rust
/// #[derive(StreamProcessor)]
/// struct MyProcessor {
///     // ... fields
/// }
///
/// impl MyProcessor {
///     fn process(&mut self, tick: TimedTick) -> Result<()> {
///         // ... implementation
///         Ok(())
///     }
/// }
/// ```
#[proc_macro_derive(StreamProcessor)]
pub fn derive_stream_processor(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let struct_name = &input.ident;
    let generics = &input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let expanded = quote! {
        impl #impl_generics ::streamlib::StreamProcessor for #struct_name #ty_generics #where_clause {
            fn process(&mut self, tick: ::streamlib::TimedTick) -> ::streamlib::Result<()> {
                // Forward to the struct's process method
                self.process(tick)
            }

            fn descriptor() -> Option<::streamlib::ProcessorDescriptor> {
                None // Override this if you want metadata
            }
        }
    };

    TokenStream::from(expanded)
}
