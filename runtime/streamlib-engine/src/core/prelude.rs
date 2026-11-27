// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! Commonly used types for `use streamlib::prelude::*`.

pub use crate::core::{
    // Errors
    error::{Error, Result},

    // Graph
    graph::{LinkUniqueId, NodeRunId},

    nodes::{ContinuousNode, ManualNode, ReactiveNode},

    // Runtime
    runtime::Runner,
};

// A node written as one function: its value inputs and the values it computes from them.
pub use streamlib_scope::{Computed, Input, computed};
