// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! A Rust processor's identity is its type path, captured by the macro.
//!
//! Asserted as whole literal strings rather than against a helper that
//! rebuilds the same path: the point of capturing at expansion time is that
//! the string is fixed by the source, so a test that recomputes it would
//! agree with any mechanism — including `std::any::type_name`, whose output
//! format rustc is free to change between versions. Spelled out, this test
//! fails on a toolchain bump that moved the string, which is the whole reason
//! identity is a macro concern.

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
        fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
            Ok(())
        }
        fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
            Ok(())
        }
        fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
            Ok(())
        }
    }
}

#[streamlib::sdk::processor("@tatolab/import-path-test/CrateRootProcessor", execution = manual)]
pub struct CrateRootProcessor;

impl streamlib_engine::ManualProcessor for CrateRootProcessor::Processor {
    fn setup(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
    fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        Ok(())
    }
}

fn import_path_of<P: GeneratedProcessor>() -> String {
    P::descriptor()
        .expect("the macro emits a descriptor for every processor")
        .processor_class_import_path
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

/// The identity names the type the author wrote, not the `Processor` struct
/// the macro generates inside the module it wraps them in. Getting this wrong
/// is invisible until a user reads a graph and finds every processor called
/// `Processor`.
#[test]
fn the_captured_path_ends_at_the_authored_type_name() {
    let import_path = import_path_of::<video_filters::BlurProcessor::Processor>();
    assert!(
        import_path.ends_with("::BlurProcessor"),
        "the identity must end at the name the author declared: {import_path}"
    );
}

/// Two processors, two identities. A capture that lost the module or the type
/// name would collapse these into one string, and the migrate ticket keys the
/// registry on it.
#[test]
fn two_processors_never_share_an_import_path() {
    assert_ne!(
        import_path_of::<video_filters::BlurProcessor::Processor>(),
        import_path_of::<CrateRootProcessor::Processor>()
    );
}
