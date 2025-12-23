// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! LinkPortMessage trait for types that can be sent through link ports.

use std::sync::Arc;

use super::link_buffer_read_mode::LinkBufferReadMode;
use super::LinkPortType;
use crate::core::Schema;

/// Trait for types that can be sent through link ports.
///
/// This is a sealed trait - only types in this crate can implement it.
pub trait LinkPortMessage:
    crate::core::links::LinkPortMessageImplementor + Clone + Send + 'static
{
    /// The type of port this message is sent through.
    fn port_type() -> LinkPortType;

    /// Schema describing this message type.
    fn schema() -> Arc<Schema>;

    /// Example instances for documentation.
    fn examples() -> Vec<(&'static str, serde_json::Value)> {
        Vec::new()
    }

    /// How this frame type should be read from the link buffer.
    fn link_read_behavior() -> LinkBufferReadMode {
        LinkBufferReadMode::SkipToLatest
    }
}
