//! Core traits for stream processors
//!
//! This module defines the trait hierarchy for all stream processors,
//! inspired by GStreamer's GstElement architecture.
//!
//! ## Trait Hierarchy
//!
//! ```text
//! StreamElement (base trait)
//!     ├─ StreamSource (no inputs, only outputs)
//!     ├─ StreamSink (only inputs, no outputs)
//!     └─ StreamTransform (any I/O configuration)
//! ```
//!
//! ## Design Philosophy
//!
//! Following GStreamer's architecture, streamlib separates processor types
//! based on their I/O configuration:
//!
//! - **StreamElement**: Base trait providing lifecycle, introspection, downcasting
//! - **StreamSource**: Data generators (cameras, microphones, test signals)
//! - **StreamSink**: Data consumers (displays, speakers, file writers)
//! - **StreamTransform**: Data processors (effects, mixers, analyzers)
//!
//! ## Usage
//!
//! Users import from the top-level `streamlib` crate, not directly from here:
//!
//! ```rust,ignore
//! use streamlib::{CameraProcessor, DisplayProcessor, EffectProcessor};
//! ```
//!
//! The top-level crate re-exports platform-specific implementations:
//!
//! ```rust,ignore
//! #[cfg(target_os = "macos")]
//! pub use apple::AppleCameraProcessor as CameraProcessor;
//! ```
//!
//! ## For Implementers
//!
//! When creating new processors:
//!
//! 1. Implement `StreamElement` for base functionality
//! 2. Implement specialized trait (`StreamSource`, `StreamSink`, or `StreamTransform`)
//! 3. Use `#[derive(StreamProcessor)]` macro to reduce boilerplate
//!
//! See `libs/streamlib-macros/CLAUDE.md` for macro documentation.

pub mod element;
pub mod source;
pub mod sink;
pub mod transform;
pub mod dyn_element;

// Re-export core traits and types
pub use element::{
    StreamElement,
    ElementType,
};

pub use source::{
    StreamSource,
    SchedulingConfig,
    SchedulingMode,
    ClockSource,
};

pub use sink::{
    StreamSink,
    ClockConfig,
    ClockType,
    SyncMode,
};

pub use transform::StreamTransform;

pub use dyn_element::DynStreamElement;
