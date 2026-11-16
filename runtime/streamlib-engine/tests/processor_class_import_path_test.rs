// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only


use streamlib_engine::core::GeneratedProcessor;
use streamlib_engine::core::{Result, RuntimeContextFullAccess};

/// Declared in a nested module, because that is the part the crate name alone
/// cannot prove: the capture has to carry the author's module path, not just
/// the crate.
mod video_filters {
    use super::*;

    #[streamlib::sdk::processor(execution = manual)]
    pub struct BlurProcessor;

    impl streamlib_engine::ManualProcessor for BlurProcessor::Processor {
        fn start(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
            Ok(())
        }
    }
}

#[streamlib::sdk::processor(execution = manual)]
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
