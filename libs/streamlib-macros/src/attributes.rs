//! Attribute parsing for StreamProcessor derive macro
//!
//! Parses #[processor(...)] and #[input(...)]/#[output(...)] attributes
//! into structured data for code generation.

use proc_macro2::TokenStream;
use syn::{
    Attribute, Error, Expr, ExprLit, Lit, Result, Type,
    punctuated::Punctuated, Token,
};

/// Parsed attributes from #[processor(...)]
#[derive(Debug, Default)]
pub struct ProcessorAttributes {
    /// Custom config type: `config = MyConfig`
    pub config_type: Option<Type>,

    /// Description: `description = "..."`
    pub description: Option<String>,

    /// Usage context: `usage = "..."`
    pub usage_context: Option<String>,

    /// Tags: `tags = ["tag1", "tag2"]`
    pub tags: Vec<String>,

    /// Audio requirements expression: `audio_requirements = {...}`
    pub audio_requirements: Option<TokenStream>,

    /// Custom process method name: `process = "my_process"`
    /// Defaults to "process" if not specified
    pub process_method: Option<String>,

    /// Custom on_start method name: `on_start = "my_start"`
    /// If not specified, looks for "on_start" method
    pub on_start_method: Option<String>,

    /// Custom on_stop method name: `on_stop = "my_stop"`
    /// If not specified, looks for "on_stop" method
    pub on_stop_method: Option<String>,

    /// Custom processor name: `name = "MyProcessor"`
    /// If not specified, uses struct name
    pub processor_name: Option<String>,

    /// Scheduling mode: `mode = Pull` or `mode = Push`
    /// Defaults to Pull if not specified
    pub scheduling_mode: Option<String>,
}

/// Parsed attributes from #[input(...)] or #[output(...)]
#[derive(Debug, Default)]
pub struct PortAttributes {
    /// Custom port name: `name = "custom_name"`
    pub custom_name: Option<String>,

    /// Port description: `description = "..."`
    pub description: Option<String>,

    /// Required flag (inputs only): `required = true`
    pub required: Option<bool>,
}

impl ProcessorAttributes {
    /// Parse #[processor(...)] attribute
    pub fn parse(attrs: &[Attribute]) -> Result<Self> {
        let mut result = Self::default();

        for attr in attrs {
            if !attr.path().is_ident("processor") {
                continue;
            }

            // Parse the attribute content
            attr.parse_nested_meta(|meta| {
                // config = MyType
                if meta.path.is_ident("config") {
                    let value: Type = meta.value()?.parse()?;
                    result.config_type = Some(value);
                    return Ok(());
                }

                // description = "..."
                if meta.path.is_ident("description") {
                    let value = parse_string_value(&meta)?;
                    result.description = Some(value);
                    return Ok(());
                }

                // usage = "..."
                if meta.path.is_ident("usage") {
                    let value = parse_string_value(&meta)?;
                    result.usage_context = Some(value);
                    return Ok(());
                }

                // tags = ["tag1", "tag2"]
                if meta.path.is_ident("tags") {
                    let content;
                    syn::bracketed!(content in meta.input);
                    let tags: Punctuated<Expr, Token![,]> =
                        content.parse_terminated(|input| input.parse::<Expr>(), Token![,])?;

                    for expr in tags {
                        if let Expr::Lit(ExprLit { lit: Lit::Str(s), .. }) = expr {
                            result.tags.push(s.value());
                        }
                    }
                    return Ok(());
                }

                // audio_requirements = {...}
                if meta.path.is_ident("audio_requirements") {
                    let content;
                    syn::braced!(content in meta.input);
                    let tokens: TokenStream = content.parse()?;
                    result.audio_requirements = Some(tokens);
                    return Ok(());
                }

                // process = "method_name"
                if meta.path.is_ident("process") {
                    let value = parse_string_value(&meta)?;
                    result.process_method = Some(value);
                    return Ok(());
                }

                // on_start = "method_name"
                if meta.path.is_ident("on_start") {
                    let value = parse_string_value(&meta)?;
                    result.on_start_method = Some(value);
                    return Ok(());
                }

                // on_stop = "method_name"
                if meta.path.is_ident("on_stop") {
                    let value = parse_string_value(&meta)?;
                    result.on_stop_method = Some(value);
                    return Ok(());
                }

                // name = "ProcessorName"
                if meta.path.is_ident("name") {
                    let value = parse_string_value(&meta)?;
                    result.processor_name = Some(value);
                    return Ok(());
                }

                // mode = Pull or mode = Push
                if meta.path.is_ident("mode") {
                    let ident: syn::Ident = meta.value()?.parse()?;
                    let mode = ident.to_string();
                    if mode != "Pull" && mode != "Push" {
                        return Err(Error::new_spanned(
                            ident,
                            "mode must be either Pull or Push"
                        ));
                    }
                    result.scheduling_mode = Some(mode);
                    return Ok(());
                }

                Err(meta.error("unsupported processor attribute"))
            })?;
        }

        Ok(result)
    }
}

impl PortAttributes {
    /// Parse #[input(...)] or #[output(...)] attribute
    ///
    /// Supports both bare attributes (#[input]) and attributes with parameters (#[input(name = "foo")])
    pub fn parse(attrs: &[Attribute], attr_name: &str) -> Result<Self> {
        let mut result = Self::default();

        for attr in attrs {
            if !attr.path().is_ident(attr_name) {
                continue;
            }

            // Check if attribute has content (tokens)
            if attr.meta.require_path_only().is_ok() {
                // Bare attribute like #[input] - no parameters to parse
                continue;
            }

            // Parse the attribute content
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

                // required = true/false (inputs only)
                if meta.path.is_ident("required") {
                    let value: Lit = meta.value()?.parse()?;
                    if let Lit::Bool(b) = value {
                        result.required = Some(b.value);
                    }
                    return Ok(());
                }

                Err(meta.error("unsupported port attribute"))
            })?;
        }

        Ok(result)
    }
}

/// Helper to parse string value from meta
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
        let attrs: Vec<Attribute> = vec![
            parse_quote! { #[processor(description = "Test processor")] }
        ];

        let result = ProcessorAttributes::parse(&attrs).unwrap();
        assert_eq!(result.description, Some("Test processor".to_string()));
    }

    #[test]
    fn test_parse_processor_tags() {
        let attrs: Vec<Attribute> = vec![
            parse_quote! { #[processor(tags = ["video", "effect"])] }
        ];

        let result = ProcessorAttributes::parse(&attrs).unwrap();
        assert_eq!(result.tags, vec!["video", "effect"]);
    }

    #[test]
    fn test_parse_port_attributes() {
        let attrs: Vec<Attribute> = vec![
            parse_quote! { #[input(name = "video_in", description = "Video input")] }
        ];

        let result = PortAttributes::parse(&attrs, "input").unwrap();
        assert_eq!(result.custom_name, Some("video_in".to_string()));
        assert_eq!(result.description, Some("Video input".to_string()));
    }

    #[test]
    fn test_parse_process_method() {
        let attrs: Vec<Attribute> = vec![
            parse_quote! { #[processor(process = "my_process")] }
        ];

        let result = ProcessorAttributes::parse(&attrs).unwrap();
        assert_eq!(result.process_method, Some("my_process".to_string()));
    }

    #[test]
    fn test_parse_lifecycle_methods() {
        let attrs: Vec<Attribute> = vec![
            parse_quote! { #[processor(on_start = "init", on_stop = "cleanup")] }
        ];

        let result = ProcessorAttributes::parse(&attrs).unwrap();
        assert_eq!(result.on_start_method, Some("init".to_string()));
        assert_eq!(result.on_stop_method, Some("cleanup".to_string()));
    }

    #[test]
    fn test_parse_processor_name() {
        let attrs: Vec<Attribute> = vec![
            parse_quote! { #[processor(name = "CustomProcessor")] }
        ];

        let result = ProcessorAttributes::parse(&attrs).unwrap();
        assert_eq!(result.processor_name, Some("CustomProcessor".to_string()));
    }

    #[test]
    fn test_parse_scheduling_mode_pull() {
        let attrs: Vec<Attribute> = vec![
            parse_quote! { #[processor(mode = Pull)] }
        ];

        let result = ProcessorAttributes::parse(&attrs).unwrap();
        assert_eq!(result.scheduling_mode, Some("Pull".to_string()));
    }

    #[test]
    fn test_parse_scheduling_mode_push() {
        let attrs: Vec<Attribute> = vec![
            parse_quote! { #[processor(mode = Push)] }
        ];

        let result = ProcessorAttributes::parse(&attrs).unwrap();
        assert_eq!(result.scheduling_mode, Some("Push".to_string()));
    }

    #[test]
    fn test_parse_multiple_processor_attributes() {
        let attrs: Vec<Attribute> = vec![
            parse_quote! {
                #[processor(
                    name = "MyProcessor",
                    process = "do_process",
                    mode = Pull,
                    description = "Test processor"
                )]
            }
        ];

        let result = ProcessorAttributes::parse(&attrs).unwrap();
        assert_eq!(result.processor_name, Some("MyProcessor".to_string()));
        assert_eq!(result.process_method, Some("do_process".to_string()));
        assert_eq!(result.scheduling_mode, Some("Pull".to_string()));
        assert_eq!(result.description, Some("Test processor".to_string()));
    }

    #[test]
    fn test_invalid_scheduling_mode() {
        let attrs: Vec<Attribute> = vec![
            parse_quote! { #[processor(mode = Invalid)] }
        ];

        let result = ProcessorAttributes::parse(&attrs);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("mode must be either Pull or Push"));
    }
}
