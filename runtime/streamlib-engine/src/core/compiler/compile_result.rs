// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::fmt;

/// Result of a successful compilation.
#[derive(Debug, Clone, Default)]
pub(super) struct CompileResult {
    pub nodes_created: usize,
    pub nodes_removed: usize,
    /// Number of links wired in this compile cycle.
    pub links_wired: usize,
    /// Number of links unwired in this compile cycle.
    pub links_unwired: usize,
    pub configs_updated: usize,
    /// Whether the compile went on to take something out of the running stream or build into
    /// it, so a failure from here leaves the stream changed.
    pub began_changing_what_runs: bool,
}

impl fmt::Display for CompileResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CompileResult {{ +{} -{} nodes, +{} -{} links, {} configs }}",
            self.nodes_created,
            self.nodes_removed,
            self.links_wired,
            self.links_unwired,
            self.configs_updated
        )
    }
}
