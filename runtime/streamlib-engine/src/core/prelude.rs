// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! Commonly used types for `use streamlib::prelude::*`.

pub use crate::core::{
    // Errors
    error::{Error, Result},

    // Graph
    graph::{LinkUniqueId, ProcessorUniqueId},

    // Processor traits (mode-specific)
    processors::{ContinuousProcessor, ManualProcessor, ReactiveProcessor},

    // Runtime
    runtime::Runner,
};
