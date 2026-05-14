// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Attribute-macro test fixtures (TestConfiguredProcessor) for streamlib SDK macro contract tests.

#[allow(non_snake_case, unused_imports, clippy::all)]
pub mod _generated_ {
    include!(concat!(env!("OUT_DIR"), "/_generated_shim.rs"));
}

pub mod test_configured_processor;

pub use test_configured_processor::ConfiguredProcessor;
