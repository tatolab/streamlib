// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Strongly-typed link port address.

use std::borrow::Cow;

use crate::core::graph::ProcessorUniqueId;

/// Strongly-typed link port address combining processor ID and port name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LinkPortAddress {
    pub processor_id: ProcessorUniqueId,
    pub port_name: Cow<'static, str>,
}

impl LinkPortAddress {
    /// Create a new link port address.
    pub fn new(
        processor: impl Into<ProcessorUniqueId>,
        port: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            processor_id: processor.into(),
            port_name: port.into(),
        }
    }

    /// Create a link port address with a static string port name (zero allocation).
    pub fn with_static(processor: impl Into<ProcessorUniqueId>, port: &'static str) -> Self {
        Self {
            processor_id: processor.into(),
            port_name: Cow::Borrowed(port),
        }
    }

    /// Get the full address as "processor_id.port_name".
    pub fn full_address(&self) -> String {
        format!("{}.{}", self.processor_id, self.port_name)
    }
}
