// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Attribute-macro test for the `TestConfiguredProcessor` fixture
//! declared in `@tatolab/test-fixtures`'s `streamlib.yaml`.

use streamlib::sdk::processors::ContinuousProcessor;
use streamlib::sdk::processors::GeneratedProcessor;
use streamlib::sdk::{context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess}, error::Result};

// Test with config field. The processor macro reads
// `streamlib.yaml`'s schema declaration to derive the config type path
// it emits — for `TestConfiguredProcessor` this is
// `crate::_generated_::tatolab__test_fixtures::TestConfiguredProcessorConfig`.
// Re-export the library's generated type through the local `_generated_`
// module so `crate::_generated_::...` resolves inside this test binary.
pub use streamlib_test_fixtures::_generated_::tatolab__test_fixtures::test_configured_processor_config::TestConfiguredProcessorConfig as ConfiguredProcessorConfig;

#[allow(non_snake_case)]
mod _generated_ {
    pub mod tatolab__test_fixtures {
        pub use streamlib_test_fixtures::_generated_::tatolab__test_fixtures::TestConfiguredProcessorConfig;
    }
}

#[streamlib::sdk::processor("TestConfiguredProcessor")]
pub struct ConfiguredProcessor;

impl ContinuousProcessor for ConfiguredProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn teardown(
        &mut self,
        _ctx: &RuntimeContextFullAccess<'_>,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        std::future::ready(Ok(()))
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        // Access config
        let _threshold = self.config.threshold;
        Ok(())
    }
}

#[test]
fn test_config_field_access() {
    let config = ConfiguredProcessorConfig { threshold: 0.5 };
    let processor = ConfiguredProcessor::Processor::from_config(config).unwrap();

    // Config should be stored
    assert_eq!(processor.config.threshold, 0.5);
}

#[test]
fn test_config_update() {
    let config = ConfiguredProcessorConfig { threshold: 0.5 };
    let mut processor = ConfiguredProcessor::Processor::from_config(config).unwrap();

    // Update config
    let new_config = ConfiguredProcessorConfig { threshold: 0.8 };
    processor.update_config(new_config).unwrap();

    assert_eq!(processor.config.threshold, 0.8);
}
