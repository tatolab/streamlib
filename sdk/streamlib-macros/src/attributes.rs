// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Attribute parsing for processor attribute macro (legacy - kept for backwards compatibility)
//!
//! Note: New processors should use YAML-based definitions. This module is retained
//! for any existing code that might depend on these types.

/// Parsed attributes from `#[processor(...)]` (legacy)
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct ProcessorAttributes {
    pub description: Option<String>,
    pub unsafe_send: bool,
    pub name: Option<String>,
}

/// A port declaration from the processor attribute (legacy)
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PortDeclaration {
    pub name: String,
    pub schema: String,
    pub history: Option<usize>,
}

/// Parsed attributes from `#[input(...)]` or `#[output(...)]` (legacy)
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct PortAttributes {
    pub custom_name: Option<String>,
    pub description: Option<String>,
    pub schema: Option<syn::Path>,
}

/// Parsed attributes from `#[state]` (legacy)
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct StateAttributes {
    pub default_expr: Option<String>,
}
