// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! ABI-stable plugin interface for StreamLib dynamic processor loading.
//!
//! This crate provides the minimal interface for plugins to register their
//! processors with the StreamLib runtime. Plugins use the same `#[streamlib::processor]`
//! macro as built-in processors - the only difference is how they're registered.
//!
//! # Example Plugin
//!
//! ```ignore
//! use streamlib::prelude::*;
//! use streamlib_plugin_abi::export_plugin;
//!
//! #[streamlib::processor(execution = Continuous)]
//! pub struct MyProcessor {
//!     #[streamlib::input(description = "Video input")]
//!     video_in: LinkInput<VideoFrame>,
//!
//!     #[streamlib::output(description = "Video output")]
//!     video_out: Arc<LinkOutput<VideoFrame>>,
//! }
//!
//! impl ContinuousProcessor for MyProcessor::Processor {
//!     fn process(&mut self) -> Result<()> {
//!         if let Some(frame) = self.video_in.read() {
//!             // Process frame...
//!             self.video_out.write(frame);
//!         }
//!         Ok(())
//!     }
//! }
//!
//! // Export for dynamic loading
//! export_plugin!(MyProcessor);
//! ```
//!
//! # Plugin Cargo.toml
//!
//! ```toml
//! [lib]
//! crate-type = ["cdylib"]
//!
//! [dependencies]
//! streamlib = "0.2"
//! streamlib-plugin-abi = "0.2"
//! ```

use streamlib::core::processors::ProcessorInstanceFactory;

/// Current ABI version. Plugins must match this exactly.
///
/// Increment when making breaking changes to the plugin interface.
pub const STREAMLIB_ABI_VERSION: u32 = 1;

/// Function signature for plugin registration.
///
/// The host passes its `ProcessorInstanceFactory` reference to ensure processors
/// register with the host's registry, not a duplicate in the plugin's address space.
pub type PluginRegisterFn = extern "C" fn(&'static ProcessorInstanceFactory);

/// Plugin declaration exported by dynamic libraries.
///
/// Plugins must export a static symbol named `STREAMLIB_PLUGIN` of this type.
/// Use the [`export_plugin!`] macro to generate this correctly.
#[repr(C)]
pub struct PluginDeclaration {
    /// ABI version - must match [`STREAMLIB_ABI_VERSION`].
    pub abi_version: u32,

    /// Registration function called by the CLI to register processors.
    ///
    /// The host passes its [`ProcessorInstanceFactory`] reference to ensure
    /// processors register with the host's registry, not a duplicate static
    /// in the plugin's address space.
    pub register: PluginRegisterFn,
}

// Safety: PluginDeclaration contains only a version number and function pointer,
// both of which are Send + Sync.
unsafe impl Send for PluginDeclaration {}
unsafe impl Sync for PluginDeclaration {}

/// Export processors for dynamic loading.
///
/// This macro generates the `STREAMLIB_PLUGIN` symbol that the CLI looks for
/// when loading plugin libraries. It creates a registration function that
/// registers each processor with the host's `ProcessorInstanceFactory`.
///
/// # Example
///
/// ```ignore
/// use streamlib_plugin_abi::export_plugin;
///
/// // Export a single processor
/// export_plugin!(MyProcessor);
///
/// // Export multiple processors
/// export_plugin!(ProcessorA, ProcessorB, ProcessorC);
/// ```
///
/// # Requirements
///
/// - Each processor must be defined using `#[streamlib::processor]`
/// - The processor's `Processor` type must implement the appropriate trait
///   (`ContinuousProcessor`, `ReactiveProcessor`, or `ManualProcessor`)
#[macro_export]
macro_rules! export_plugin {
    ($($processor:ty),* $(,)?) => {
        #[allow(non_snake_case)]
        extern "C" fn __streamlib_plugin_register(
            registry: &'static ::streamlib::core::processors::ProcessorInstanceFactory
        ) {
            $(
                registry.register::<$processor>();
            )*
        }

        #[no_mangle]
        pub static STREAMLIB_PLUGIN: $crate::PluginDeclaration = $crate::PluginDeclaration {
            abi_version: $crate::STREAMLIB_ABI_VERSION,
            register: __streamlib_plugin_register,
        };
    };
}
