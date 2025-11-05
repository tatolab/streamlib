//! Core traits for stream processors
//!
//! This module defines the unified trait architecture for all stream processors.
//!
//! ## Trait Architecture
//!
//! ```text
//! StreamElement (base trait - lifecycle, introspection)
//!     └─ StreamProcessor (unified trait with optional methods)
//! ```
//!
//! ## Design Philosophy
//!
//! streamlib uses a unified `StreamProcessor` trait with optional methods,
//! allowing processors to implement only the functionality they need:
//!
//! - **StreamElement**: Base trait providing lifecycle, introspection, downcasting
//! - **StreamProcessor**: Unified trait with `process()` method and optional scheduling config
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
//! 2. Implement `StreamProcessor` with your processing logic
//! 3. Use `#[derive(StreamProcessor)]` macro to reduce boilerplate
//!
//! See `libs/streamlib-macros/CLAUDE.md` for macro documentation.

pub mod element;
pub mod processor;
pub mod dyn_element;
mod dyn_element_impl;

mod sealed {
    pub trait Sealed {}
}

pub use sealed::Sealed;

pub use element::{
    StreamElement,
    ElementType,
};

pub use processor::StreamProcessor;

pub use dyn_element::DynStreamElement;
