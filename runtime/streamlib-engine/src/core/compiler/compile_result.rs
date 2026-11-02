// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fmt;

/// Result of a successful compilation.
#[derive(Debug, Clone, Default)]
pub struct CompileResult {
    /// Number of processors created in this compile cycle.
    pub processors_created: usize,
    /// Number of processors removed in this compile cycle.
    pub processors_removed: usize,
    /// Number of links wired in this compile cycle.
    pub links_wired: usize,
    /// Number of links unwired in this compile cycle.
    pub links_unwired: usize,
    /// Number of processor configs updated.
    pub configs_updated: usize,
}

impl fmt::Display for CompileResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CompileResult {{ +{} -{} processors, +{} -{} links, {} configs }}",
            self.processors_created,
            self.processors_removed,
            self.links_wired,
            self.links_unwired,
            self.configs_updated
        )
    }
}
