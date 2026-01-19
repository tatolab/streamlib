// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Procedural macros for streamlib
//!
//! Attribute macros for defining processors:
//!
//! - `#[streamlib::processor]` - Main processor definition
//! - `#[streamlib::input]` - Input port marker
//! - `#[streamlib::output]` - Output port marker
//! - `#[streamlib::config]` - Config field marker
//!
//! # Example
//!
//! ```ignore
//! use streamlib::prelude::*;
//!
//! #[streamlib::processor(execution = Manual)]
//! pub struct CameraProcessor {
//!     #[streamlib::output]
//!     video: LinkOutput<VideoFrame>,
//!
//!     #[streamlib::config]
//!     config: CameraConfig,
//! }
//!
//! impl CameraProcessor::Processor {
//!     fn setup(&mut self, _ctx: &RuntimeContext) -> Result<()> { Ok(()) }
//!     fn teardown(&mut self) -> Result<()> { Ok(()) }
//!     fn process(&mut self) -> Result<()> { Ok(()) }
//! }
//! ```
//!
//! Generates:
//!
//! ```ignore
//! pub mod CameraProcessor {
//!     pub struct Processor { ... }
//!
//!     pub mod InputLink {}
//!     pub mod OutputLink {
//!         pub struct video;
//!     }
//! }
//! ```

mod analysis;
mod attributes;
mod codegen;
mod config_descriptor;

use proc_macro::TokenStream;
use syn::{parse_macro_input, DeriveInput, ItemStruct};

/// Main processor attribute macro.
///
/// Transforms a struct definition into a processor module containing:
/// - `Processor` struct with port fields
/// - `InputLink` module with input port markers
/// - `OutputLink` module with output port markers
/// - All necessary trait implementations
///
/// # Attributes
///
/// ## Execution Mode (determines when `process()` is called)
///
/// - `execution = Continuous` - Runtime loops, calling process() repeatedly (for polling sources)
/// - `execution = Reactive` - Called when upstream writes to any input port (default)
/// - `execution = Manual` - Called once, then you control timing via callbacks/external systems
///
/// ### Execution Mode with Interval
///
/// - `execution = Continuous, execution_interval_ms = 100` - Sleep 100ms between process() calls
///
/// ## Other Attributes
///
/// - `description = "..."` - Processor description
/// - `unsafe_send` - Generate `unsafe impl Send`
///
/// # Example
///
/// ```ignore
/// #[streamlib::processor(execution = Reactive)]
/// pub struct MyProcessor {
///     #[streamlib::input]"
///     audio_in: LinkInput<AudioFrame>,
///
///     #[streamlib::output]
///     audio_out: LinkOutput<AudioFrame>,
///
///     #[streamlib::config]
///     config: MyConfig,
/// }
/// ```
#[proc_macro_attribute]
pub fn processor(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_struct = parse_macro_input!(item as ItemStruct);
    let attr_tokens: proc_macro2::TokenStream = attr.into();

    let analysis = match analysis::AnalysisResult::analyze(&item_struct, attr_tokens) {
        Ok(result) => result,
        Err(err) => return err.to_compile_error().into(),
    };

    let generated = codegen::generate_processor_module(&analysis);

    TokenStream::from(generated)
}

/// Input port marker attribute.
///
/// Marks a field as an input port. Used within `#[streamlib::processor]`.
///
/// # Attributes
///
/// - `description = "..."` - Port description
/// - `name = "..."` - Custom port name (defaults to field name)
#[proc_macro_attribute]
pub fn input(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Output port marker attribute.
///
/// Marks a field as an output port. Used within `#[streamlib::processor]`.
///
/// # Attributes
///
/// - `description = "..."` - Port description
/// - `name = "..."` - Custom port name (defaults to field name)
#[proc_macro_attribute]
pub fn output(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Config field marker attribute.
///
/// Marks a field as a config field. Used within `#[streamlib::processor]`.
#[proc_macro_attribute]
pub fn config(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Derive macro for ConfigDescriptor trait.
///
/// Generates a `ConfigDescriptor` implementation for config structs,
/// enabling automatic config field metadata extraction for processor descriptors.
///
/// # Field Handling
///
/// - `Option<T>` fields are marked as `required: false`
/// - All other fields are marked as `required: true`
/// - Doc comments on fields become the `description`
///
/// # Example
///
/// ```ignore
/// use streamlib::ConfigDescriptor;
///
/// #[derive(ConfigDescriptor)]
/// pub struct CameraConfig {
///     /// Camera device identifier
///     pub device_id: Option<String>,
///     /// Target width in pixels
///     pub width: u32,
///     /// Target height in pixels
///     pub height: u32,
/// }
/// ```
#[proc_macro_derive(ConfigDescriptor)]
pub fn derive_config_descriptor(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);

    match config_descriptor::derive_config_descriptor(input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

