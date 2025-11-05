//! Code generation for #[derive(PortRegistry)]
//!
//! Generates:
//! 1. Separate InputPorts and OutputPorts structs with named accessors
//! 2. Main PortRegistry struct with .inputs() and .outputs() methods
//! 3. Auto-implementation of port introspection methods for DynStreamElement trait

use proc_macro2::TokenStream;
use quote::{quote, format_ident};
use syn::{Data, DeriveInput, Fields, Type, Error, Result};

/// Information about a port field
#[derive(Debug, Clone)]
struct PortField {
    name: syn::Ident,
    ty: Type,
    is_input: bool,
}

/// Analyze the struct and extract port fields
fn analyze_ports(input: &DeriveInput) -> Result<Vec<PortField>> {
    let mut ports = Vec::new();

    let data_struct = match &input.data {
        Data::Struct(s) => s,
        _ => return Err(Error::new_spanned(input, "PortRegistry can only be derived for structs")),
    };

    let fields = match &data_struct.fields {
        Fields::Named(fields) => &fields.named,
        _ => return Err(Error::new_spanned(input, "PortRegistry requires named fields")),
    };

    for field in fields {
        let field_name = field.ident.as_ref()
            .ok_or_else(|| Error::new_spanned(field, "Field must have a name"))?
            .clone();

        let mut is_input = false;
        let mut is_output = false;

        for attr in &field.attrs {
            if attr.path().is_ident("input") {
                is_input = true;
            } else if attr.path().is_ident("output") {
                is_output = true;
            }
        }

        if is_input && is_output {
            return Err(Error::new_spanned(field, "Port cannot be both input and output"));
        }

        if is_input || is_output {
            ports.push(PortField {
                name: field_name,
                ty: field.ty.clone(),
                is_input,
            });
        }
    }

    if ports.is_empty() {
        return Err(Error::new_spanned(input, "PortRegistry requires at least one #[input] or #[output] field"));
    }

    Ok(ports)
}

/// Extract the inner type from StreamInput<T> or StreamOutput<T>
fn extract_port_message_type(ty: &Type) -> Result<Type> {
    match ty {
        Type::Path(type_path) => {
            let last_segment = type_path.path.segments.last()
                .ok_or_else(|| Error::new_spanned(ty, "Empty type path"))?;

            if last_segment.ident != "StreamInput" && last_segment.ident != "StreamOutput" {
                return Err(Error::new_spanned(ty, "Port fields must be StreamInput<T> or StreamOutput<T>"));
            }

            match &last_segment.arguments {
                syn::PathArguments::AngleBracketed(args) => {
                    if args.args.len() != 1 {
                        return Err(Error::new_spanned(ty, "StreamInput/StreamOutput must have exactly one type parameter"));
                    }

                    match args.args.first().unwrap() {
                        syn::GenericArgument::Type(inner_ty) => Ok(inner_ty.clone()),
                        _ => Err(Error::new_spanned(ty, "Expected type parameter")),
                    }
                }
                _ => Err(Error::new_spanned(ty, "StreamInput/StreamOutput must have type parameters")),
            }
        }
        _ => Err(Error::new_spanned(ty, "Expected type path")),
    }
}

/// Generate the complete PortRegistry implementation
pub fn generate_port_registry(input: &DeriveInput) -> Result<TokenStream> {
    let ports = analyze_ports(input)?;
    let struct_name = &input.ident;

    let input_ports: Vec<_> = ports.iter().filter(|p| p.is_input).collect();
    let output_ports: Vec<_> = ports.iter().filter(|p| !p.is_input).collect();

    // Generate struct names
    let input_struct_name = format_ident!("{}InputPorts", struct_name);
    let output_struct_name = format_ident!("{}OutputPorts", struct_name);

    // Generate InputPorts struct
    let input_fields = input_ports.iter().map(|p| {
        let name = &p.name;
        let ty = &p.ty;
        quote! { pub #name: #ty }
    });

    let input_struct = if !input_ports.is_empty() {
        quote! {
            pub struct #input_struct_name {
                #(#input_fields),*
            }
        }
    } else {
        quote! {
            pub struct #input_struct_name;
        }
    };

    // Generate OutputPorts struct
    let output_fields = output_ports.iter().map(|p| {
        let name = &p.name;
        let ty = &p.ty;
        quote! { pub #name: #ty }
    });

    let output_struct = if !output_ports.is_empty() {
        quote! {
            pub struct #output_struct_name {
                #(#output_fields),*
            }
        }
    } else {
        quote! {
            pub struct #output_struct_name;
        }
    };

    // Generate new() method
    let input_field_inits = input_ports.iter().map(|p| {
        let name = &p.name;
        let name_str = name.to_string();
        let ty = &p.ty;
        quote! { #name: <#ty>::new(#name_str) }
    });

    let output_field_inits = output_ports.iter().map(|p| {
        let name = &p.name;
        let name_str = name.to_string();
        let ty = &p.ty;
        quote! { #name: <#ty>::new(#name_str) }
    });

    let new_input_init = if !input_ports.is_empty() {
        quote! { #input_struct_name { #(#input_field_inits),* } }
    } else {
        quote! { #input_struct_name }
    };

    let new_output_init = if !output_ports.is_empty() {
        quote! { #output_struct_name { #(#output_field_inits),* } }
    } else {
        quote! { #output_struct_name }
    };

    // Generate get_input_port_type implementation
    let input_port_type_arms = input_ports.iter().map(|p| {
        let name_str = p.name.to_string();
        let inner_ty = extract_port_message_type(&p.ty)?;
        Ok(quote! {
            #name_str => Some(<#inner_ty>::port_type())
        })
    }).collect::<Result<Vec<_>>>()?;

    let get_input_port_type_impl = if !input_port_type_arms.is_empty() {
        quote! {
            fn get_input_port_type(&self, port_name: &str) -> Option<streamlib::PortType> {
                match port_name {
                    #(#input_port_type_arms,)*
                    _ => None,
                }
            }
        }
    } else {
        quote! {
            fn get_input_port_type(&self, _port_name: &str) -> Option<streamlib::PortType> {
                None
            }
        }
    };

    // Generate get_output_port_type implementation
    let output_port_type_arms = output_ports.iter().map(|p| {
        let name_str = p.name.to_string();
        let inner_ty = extract_port_message_type(&p.ty)?;
        Ok(quote! {
            #name_str => Some(<#inner_ty as streamlib::PortMessage>::port_type())
        })
    }).collect::<Result<Vec<_>>>()?;

    let get_output_port_type_impl = if !output_port_type_arms.is_empty() {
        quote! {
            fn get_output_port_type(&self, port_name: &str) -> Option<streamlib::PortType> {
                match port_name {
                    #(#output_port_type_arms,)*
                    _ => None,
                }
            }
        }
    } else {
        quote! {
            fn get_output_port_type(&self, _port_name: &str) -> Option<streamlib::PortType> {
                None
            }
        }
    };

    // Generate wire_input_connection implementation
    let input_wire_arms = input_ports.iter().map(|p| {
        let name = &p.name;
        let name_str = name.to_string();
        let inner_ty = extract_port_message_type(&p.ty)?;
        Ok(quote! {
            #name_str => {
                if let Ok(typed_conn) = connection.downcast::<std::sync::Arc<streamlib::ProcessorConnection<#inner_ty>>>() {
                    self.inputs.#name.set_connection(std::sync::Arc::clone(&typed_conn));
                    true
                } else {
                    false
                }
            }
        })
    }).collect::<Result<Vec<_>>>()?;

    let wire_input_connection_impl = if !input_wire_arms.is_empty() {
        quote! {
            fn wire_input_connection(&mut self, port_name: &str, connection: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> bool {
                match port_name {
                    #(#input_wire_arms,)*
                    _ => false,
                }
            }
        }
    } else {
        quote! {
            fn wire_input_connection(&mut self, _port_name: &str, _connection: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> bool {
                false
            }
        }
    };

    // Generate wire_output_connection implementation
    let output_wire_arms = output_ports.iter().map(|p| {
        let name = &p.name;
        let name_str = name.to_string();
        let inner_ty = extract_port_message_type(&p.ty)?;
        Ok(quote! {
            #name_str => {
                if let Ok(typed_conn) = connection.downcast::<std::sync::Arc<streamlib::ProcessorConnection<#inner_ty>>>() {
                    self.outputs.#name.add_connection(std::sync::Arc::clone(&typed_conn));
                    true
                } else {
                    false
                }
            }
        })
    }).collect::<Result<Vec<_>>>()?;

    let wire_output_connection_impl = if !output_wire_arms.is_empty() {
        quote! {
            fn wire_output_connection(&mut self, port_name: &str, connection: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> bool {
                match port_name {
                    #(#output_wire_arms,)*
                    _ => false,
                }
            }
        }
    } else {
        quote! {
            fn wire_output_connection(&mut self, _port_name: &str, _connection: std::sync::Arc<dyn std::any::Any + Send + Sync>) -> bool {
                false
            }
        }
    };

    // Generate the main struct with inputs and outputs fields
    let main_struct = quote! {
        pub struct #struct_name {
            inputs: #input_struct_name,
            outputs: #output_struct_name,
        }
    };

    // Generate the complete implementation
    let expanded = quote! {
        #input_struct

        #output_struct

        #main_struct

        impl #struct_name {
            pub fn new() -> Self {
                Self {
                    inputs: #new_input_init,
                    outputs: #new_output_init,
                }
            }

            pub fn inputs(&self) -> &#input_struct_name {
                &self.inputs
            }

            pub fn inputs_mut(&mut self) -> &mut #input_struct_name {
                &mut self.inputs
            }

            pub fn outputs(&self) -> &#output_struct_name {
                &self.outputs
            }

            pub fn outputs_mut(&mut self) -> &mut #output_struct_name {
                &mut self.outputs
            }

            #get_input_port_type_impl

            #get_output_port_type_impl

            #wire_input_connection_impl

            #wire_output_connection_impl
        }

        impl Default for #struct_name {
            fn default() -> Self {
                Self::new()
            }
        }
    };

    Ok(expanded)
}
