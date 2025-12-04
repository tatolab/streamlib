//! Commonly used types for `use streamlib::prelude::*`.

pub use crate::core::{
    // Errors
    error::{Result, StreamError},

    // Frames
    frames::{AudioFrame, DataFrame, VideoFrame},

    // Graph
    graph::ProcessorId,
    links::LinkId,
    // Processors
    processors::Processor,

    // Runtime
    runtime::StreamRuntime,
};
