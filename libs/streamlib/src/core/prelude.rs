// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Commonly used types for `use streamlib::prelude::*`.

pub use crate::core::{
    // Errors
    error::{Result, StreamError},

    // Frames
    frames::{AudioFrame, VideoFrame},

    // Graph
    graph::{LinkUniqueId, ProcessorUniqueId},

    // Processor traits (mode-specific)
    processors::{ContinuousProcessor, ManualProcessor, ReactiveProcessor},

    // Runtime
    runtime::StreamRuntime,
};
