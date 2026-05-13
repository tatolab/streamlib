// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! `@tatolab/test-fixtures` — test and example fixture processors for streamlib.
//!
//! - [`BgraFileSourceProcessor`] streams raw BGRA frames from disk
//!   (Linux-only today; lifts the platform constraint when a portable
//!   pixel-buffer acquire path lands).
//! - [`SimplePassthroughProcessor`] is a one-port-in, one-port-out
//!   processor used by runtime tests exercising graph plumbing and by
//!   `api-server-demo` to exercise the dynamic processor registry.
//! - [`TestConfiguredProcessor`] (defined in
//!   `tests/configured_processor_test.rs`) is an attribute-macro test
//!   fixture verifying the `#[streamlib::sdk::processor("...")]` macro
//!   emits the right schema/processor declarations.

pub mod _generated_;

#[cfg(target_os = "linux")]
pub mod bgra_file_source;

pub mod simple_passthrough;

#[cfg(target_os = "linux")]
pub use bgra_file_source::BgraFileSourceProcessor;

pub use simple_passthrough::SimplePassthroughProcessor;

pub use _generated_::{BgraFileSourceConfig, SimplePassthroughConfig};
