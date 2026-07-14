// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Field analysis for processor attribute macro (legacy - kept for backwards compatibility)
//!
//! Note: New processors should use YAML-based definitions. This module is retained
//! for any existing code that might depend on these types.

use proc_macro2::Ident;
use syn::Type;

/// Direction of a port (legacy)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum PortDirection {
    Input,
    Output,
}

/// Information about a port field (legacy)
#[derive(Debug)]
#[allow(dead_code)]
pub struct PortField {
    pub field_name: Ident,
    pub port_name: String,
    pub direction: PortDirection,
    pub message_type: Type,
    pub is_arc_wrapped: bool,
    pub field_type: Type,
}

/// Information about a state field (legacy)
#[derive(Debug)]
#[allow(dead_code)]
pub struct StateField {
    pub field_name: Ident,
    pub field_type: Type,
}

/// Complete analysis result (legacy)
#[derive(Debug)]
#[allow(dead_code)]
pub struct AnalysisResult {
    pub struct_name: Ident,
    pub port_fields: Vec<PortField>,
    pub state_fields: Vec<StateField>,
    pub config_field_type: Option<Type>,
    pub config_field_name: Option<Ident>,
}
