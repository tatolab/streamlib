// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A Rust processor's identity is its type path, captured by the macro.
//!
//! Asserted as whole literal strings rather than against a helper that rebuilds
//! the same path: a test that recomputed the identity would agree with any
//! mechanism that produced one, including a reflected one. Spelled out, these
//! fail on a toolchain bump that moved the string.

use streamlib_engine::core::GeneratedProcessor;
use streamlib_engine::core::{Result, RuntimeContextFullAccess};

/// Declared in a nested module, because that is the part the crate name alone
/// cannot prove: the capture has to carry the author's module path, not just
/// the crate.
mod video_filters {
    use super::*;

    #[streamlib::sdk::processor("@tatolab/import-path-test/BlurProcessor", execution = manual)]
    pub struct BlurProcessor;

    impl streamlib_engine::ManualProcessor for BlurProcessor::Processor {
        fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
            Ok(())
        }
    }
}

#[streamlib::sdk::processor("@tatolab/import-path-test/CrateRootProcessor", execution = manual)]
pub struct CrateRootProcessor;

impl streamlib_engine::ManualProcessor for CrateRootProcessor::Processor {
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

fn import_path_of<P: GeneratedProcessor>() -> String {
    P::descriptor()
        .expect("the macro emits a descriptor for every processor")
        .processor_class_import_path
        .as_str()
        .to_string()
}

#[test]
fn a_processor_in_a_nested_module_carries_crate_module_and_type() {
    assert_eq!(
        import_path_of::<video_filters::BlurProcessor::Processor>(),
        "processor_class_import_path_test::video_filters::BlurProcessor"
    );
}

#[test]
fn a_processor_at_the_crate_root_carries_crate_and_type() {
    assert_eq!(
        import_path_of::<CrateRootProcessor::Processor>(),
        "processor_class_import_path_test::CrateRootProcessor"
    );
}
