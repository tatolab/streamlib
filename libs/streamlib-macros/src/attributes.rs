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

                Err(meta.error("unsupported processor attribute"))
            })?;
        }

        Ok(result)
    }
}

impl PortAttributes {
    /// Parse #[input(...)] or #[output(...)] attribute
    pub fn parse(attrs: &[Attribute], attr_name: &str) -> Result<Self> {
        let mut result = Self::default();

        for attr in attrs {
            if !attr.path().is_ident(attr_name) {
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
    use quote::quote;
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
}
