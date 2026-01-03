// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Attribute parsing for processor attribute macro
//!
//! Parses `#[processor(...)]` and `#[input(...)]`/`#[output(...)]` attributes.

use proc_macro2::TokenStream;
use streamlib_codegen_shared::ProcessExecution;
use syn::{Attribute, Error, Lit, Result};

/// Parsed attributes from `#[processor(...)]`
#[derive(Debug, Default)]
pub struct ProcessorAttributes {
    /// Execution mode: determines how and when `process()` is called.
    ///
    /// Uses [`ProcessExecution`] from streamlib-codegen-shared (single source of truth).
    pub execution_mode: Option<ProcessExecution>,

    /// Description: `description = "..."`
    pub description: Option<String>,

    /// Generate unsafe impl Send: `unsafe_send`
    pub unsafe_send: bool,

    /// Custom processor name: `name = "..."`
    /// If not specified, defaults to the struct name.
    pub name: Option<String>,

    /// Extract display_name from a config field: `display_name_from_config = "field_name"`
    /// The generated `node()` will call `.with_display_name(config.field_name.clone())`.
    pub display_name_from_config: Option<String>,
}

/// Parsed attributes from `#[input(...)]` or `#[output(...)]`
#[derive(Debug, Default)]
pub struct PortAttributes {
    /// Custom port name: `name = "custom_name"`
    pub custom_name: Option<String>,

    /// Port description: `description = "..."`
    pub description: Option<String>,

    /// DataFrame schema type: `schema = MySchemaType`
    pub schema: Option<syn::Path>,
}

/// Parsed attributes from `#[state]`
#[derive(Debug, Default)]
pub struct StateAttributes {
    /// Custom default expression: `default = "expression"`
    pub default_expr: Option<String>,
}

impl ProcessorAttributes {
    /// Parse from attribute macro args: `#[streamlib::processor(execution = Reactive)]`
    pub fn parse_from_args(args: TokenStream) -> Result<Self> {
        let mut result = Self::default();

        if args.is_empty() {
            return Ok(result);
        }

        // Parse as a synthetic attribute to reuse existing logic
        let attr: Attribute = syn::parse_quote! { #[processor(#args)] };
        Self::parse_single_attr(&attr, &mut result)?;

        Ok(result)
    }

    /// Parse a single processor attribute into the result
    fn parse_single_attr(attr: &Attribute, result: &mut Self) -> Result<()> {
        attr.parse_nested_meta(|meta| {
            // description = "..."
            if meta.path.is_ident("description") {
                let value = parse_string_value(&meta)?;
                result.description = Some(value);
                return Ok(());
            }

            // name = "..." (custom processor name)
            if meta.path.is_ident("name") {
                let value = parse_string_value(&meta)?;
                result.name = Some(value);
                return Ok(());
            }

            // execution = Continuous | Reactive | Manual
            if meta.path.is_ident("execution") {
                let path: syn::Path = meta.value()?.parse()?;
                let mode_str = path
                    .segments
                    .last()
                    .map(|seg| seg.ident.to_string())
                    .ok_or_else(|| Error::new_spanned(&path, "Invalid execution path"))?;

                let execution_mode = match mode_str.as_str() {
                    "Continuous" => ProcessExecution::Continuous { interval_ms: 0 },
                    "Reactive" => ProcessExecution::Reactive,
                    "Manual" => ProcessExecution::Manual,
                    _ => {
                        return Err(Error::new_spanned(
                            path,
                            format!(
                                "execution must be Continuous, Reactive, or Manual (got '{}')\n\
                                 \n\
                                 Help:\n\
                                 - Continuous: Runtime loops, calling process() repeatedly\n\
                                 - Reactive: Called when upstream writes to any input port\n\
                                 - Manual: Called once, then you control timing",
                                mode_str
                            ),
                        ));
                    }
                };

                result.execution_mode = Some(execution_mode);
                return Ok(());
            }

            // execution_interval_ms = N (for Continuous mode)
            if meta.path.is_ident("execution_interval_ms") {
                let value: syn::LitInt = meta.value()?.parse()?;
                let interval_ms: u32 = value.base10_parse()?;

                // Update or create Continuous mode with interval
                match &mut result.execution_mode {
                    Some(ProcessExecution::Continuous {
                        interval_ms: ref mut i,
                    }) => {
                        *i = interval_ms;
                    }
                    None => {
                        result.execution_mode = Some(ProcessExecution::Continuous { interval_ms });
                    }
                    Some(_) => {
                        return Err(Error::new_spanned(
                            value,
                            "execution_interval_ms can only be used with execution = Continuous",
                        ));
                    }
                }
                return Ok(());
            }

            // LEGACY: mode = Pull | Push | Loop (deprecated, maps to new names)
            if meta.path.is_ident("mode") {
                let path: syn::Path = meta.value()?.parse()?;
                let mode = path
                    .segments
                    .last()
                    .map(|seg| seg.ident.to_string())
                    .ok_or_else(|| Error::new_spanned(&path, "Invalid mode path"))?;

                let execution_mode = match mode.as_str() {
                    "Loop" => ProcessExecution::Continuous { interval_ms: 0 },
                    "Push" => ProcessExecution::Reactive,
                    "Pull" => ProcessExecution::Manual,
                    _ => {
                        return Err(Error::new_spanned(
                            path,
                            format!(
                                "mode must be Loop, Push, or Pull (got '{}')\n\
                                 \n\
                                 Note: 'mode' is deprecated. Use 'execution' instead:\n\
                                 - Loop -> execution = Continuous\n\
                                 - Push -> execution = Reactive\n\
                                 - Pull -> execution = Manual",
                                mode
                            ),
                        ));
                    }
                };

                result.execution_mode = Some(execution_mode);
                return Ok(());
            }

            // unsafe_send (flag attribute, no value)
            if meta.path.is_ident("unsafe_send") {
                result.unsafe_send = true;
                return Ok(());
            }

            // display_name_from_config = "field_name"
            if meta.path.is_ident("display_name_from_config") {
                let value = parse_string_value(&meta)?;
                result.display_name_from_config = Some(value);
                return Ok(());
            }

            Err(meta.error("unsupported processor attribute"))
        })
    }
}

impl PortAttributes {
    /// Parse `#[input(...)]` or `#[output(...)]` attribute
    pub fn parse(attrs: &[Attribute], attr_name: &str) -> Result<Self> {
        let mut result = Self::default();

        for attr in attrs {
            // Check `#[name]`, `#[streamlib::name]`, and `#[crate::name]`
            let is_match = attr.path().is_ident(attr_name)
                || (attr.path().segments.len() == 2
                    && (attr.path().segments[0].ident == "streamlib"
                        || attr.path().segments[0].ident == "crate")
                    && attr.path().segments[1].ident == attr_name);

            if !is_match {
                continue;
            }

            // Bare attribute like #[input] - no parameters to parse
            if attr.meta.require_path_only().is_ok() {
                continue;
            }

            attr.parse_nested_meta(|meta| {
                // name = "custom_name"
                if meta.path.is_ident("name") {
                    let value = parse_string_value(&meta)?;
                    result.custom_name = Some(value);
                    return Ok(());
                }

                // description = "..."
                if meta.path.is_ident("description") {
                    let value = parse_string_value(&meta)?;
                    result.description = Some(value);
                    return Ok(());
                }

                // schema = MySchemaType
                if meta.path.is_ident("schema") {
                    let path: syn::Path = meta.value()?.parse()?;
                    result.schema = Some(path);
                    return Ok(());
                }

                Err(meta.error("unsupported port attribute"))
            })?;
        }

        Ok(result)
    }
}

impl StateAttributes {
    /// Parse `#[state(...)]` attribute
    pub fn parse(attrs: &[Attribute]) -> Result<Self> {
        let mut result = Self::default();

        for attr in attrs {
            if !attr.path().is_ident("state") {
                continue;
            }

            // Bare #[state] attribute - use Default::default()
            if attr.meta.require_path_only().is_ok() {
                continue;
            }

            attr.parse_nested_meta(|meta| {
                // default = "expression"
                if meta.path.is_ident("default") {
                    let value = parse_string_value(&meta)?;
                    result.default_expr = Some(value);
                    return Ok(());
                }

                Err(meta.error("unsupported state attribute"))
            })?;
        }

        Ok(result)
    }
}

fn parse_string_value(meta: &syn::meta::ParseNestedMeta) -> Result<String> {
    let value: Lit = meta.value()?.parse()?;
    if let Lit::Str(s) = value {
        Ok(s.value())
    } else {
        Err(Error::new_spanned(value, "expected string literal"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    #[test]
    fn test_parse_processor_description() {
        let args: TokenStream = quote::quote! { description = "Test processor" };
        let result = ProcessorAttributes::parse_from_args(args).unwrap();
        assert_eq!(result.description, Some("Test processor".to_string()));
    }

    // Execution syntax tests
    #[test]
    fn test_parse_execution_continuous() {
        let args: TokenStream = quote::quote! { execution = Continuous };
        let result = ProcessorAttributes::parse_from_args(args).unwrap();
        assert_eq!(
            result.execution_mode,
            Some(ProcessExecution::Continuous { interval_ms: 0 })
        );
    }

    #[test]
    fn test_parse_execution_reactive() {
        let args: TokenStream = quote::quote! { execution = Reactive };
        let result = ProcessorAttributes::parse_from_args(args).unwrap();
        assert_eq!(result.execution_mode, Some(ProcessExecution::Reactive));
    }

    #[test]
    fn test_parse_execution_manual() {
        let args: TokenStream = quote::quote! { execution = Manual };
        let result = ProcessorAttributes::parse_from_args(args).unwrap();
        assert_eq!(result.execution_mode, Some(ProcessExecution::Manual));
    }

    #[test]
    fn test_parse_execution_with_interval() {
        let args: TokenStream =
            quote::quote! { execution = Continuous, execution_interval_ms = 100 };
        let result = ProcessorAttributes::parse_from_args(args).unwrap();
        assert_eq!(
            result.execution_mode,
            Some(ProcessExecution::Continuous { interval_ms: 100 })
        );
    }

    #[test]
    fn test_parse_execution_interval_implies_continuous() {
        let args: TokenStream = quote::quote! { execution_interval_ms = 50 };
        let result = ProcessorAttributes::parse_from_args(args).unwrap();
        assert_eq!(
            result.execution_mode,
            Some(ProcessExecution::Continuous { interval_ms: 50 })
        );
    }

    // Legacy mode syntax tests (backwards compatibility)
    #[test]
    fn test_parse_legacy_mode_loop() {
        let args: TokenStream = quote::quote! { mode = Loop };
        let result = ProcessorAttributes::parse_from_args(args).unwrap();
        assert_eq!(
            result.execution_mode,
            Some(ProcessExecution::Continuous { interval_ms: 0 })
        );
    }

    #[test]
    fn test_parse_legacy_mode_push() {
        let args: TokenStream = quote::quote! { mode = Push };
        let result = ProcessorAttributes::parse_from_args(args).unwrap();
        assert_eq!(result.execution_mode, Some(ProcessExecution::Reactive));
    }

    #[test]
    fn test_parse_legacy_mode_pull() {
        let args: TokenStream = quote::quote! { mode = Pull };
        let result = ProcessorAttributes::parse_from_args(args).unwrap();
        assert_eq!(result.execution_mode, Some(ProcessExecution::Manual));
    }

    #[test]
    fn test_parse_unsafe_send() {
        let args: TokenStream = quote::quote! { execution = Manual, unsafe_send };
        let result = ProcessorAttributes::parse_from_args(args).unwrap();
        assert!(result.unsafe_send);
    }

    #[test]
    fn test_parse_multiple_attributes() {
        let args: TokenStream = quote::quote! {
            execution = Manual,
            description = "Test processor",
            unsafe_send
        };
        let result = ProcessorAttributes::parse_from_args(args).unwrap();
        assert_eq!(result.execution_mode, Some(ProcessExecution::Manual));
        assert_eq!(result.description, Some("Test processor".to_string()));
        assert!(result.unsafe_send);
    }

    #[test]
    fn test_invalid_execution_mode() {
        let args: TokenStream = quote::quote! { execution = Invalid };
        let result = ProcessorAttributes::parse_from_args(args);
        assert!(result.is_err());
        let error_msg = result.unwrap_err().to_string();
        assert!(error_msg.contains("execution must be"));
    }

    #[test]
    fn test_parse_port_attributes() {
        let attrs: Vec<Attribute> =
            vec![parse_quote! { #[input(name = "video_in", description = "Video input")] }];
        let result = PortAttributes::parse(&attrs, "input").unwrap();
        assert_eq!(result.custom_name, Some("video_in".to_string()));
        assert_eq!(result.description, Some("Video input".to_string()));
    }

    #[test]
    fn test_parse_bare_port_attribute() {
        let attrs: Vec<Attribute> = vec![parse_quote! { #[input] }];
        let result = PortAttributes::parse(&attrs, "input").unwrap();
        assert_eq!(result.custom_name, None);
        assert_eq!(result.description, None);
    }
}
