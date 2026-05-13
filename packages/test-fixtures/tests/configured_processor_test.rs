// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Attribute-macro test: verifies that `#[streamlib::sdk::processor(...)]`
//! against a `streamlib.yaml`-declared config schema instantiates and
//! round-trips through `from_config` / `update_config`.

use streamlib::sdk::processors::GeneratedProcessor;
use streamlib_test_fixtures::_generated_::TestConfiguredProcessorConfig;
use streamlib_test_fixtures::ConfiguredProcessor;

#[test]
fn test_config_field_access() {
    let config = TestConfiguredProcessorConfig { threshold: 0.5 };
    let processor = ConfiguredProcessor::Processor::from_config(config).unwrap();
    assert_eq!(processor.config.threshold, 0.5);
}

#[test]
fn test_config_update() {
    let config = TestConfiguredProcessorConfig { threshold: 0.5 };
    let mut processor = ConfiguredProcessor::Processor::from_config(config).unwrap();

    let new_config = TestConfiguredProcessorConfig { threshold: 0.8 };
    processor.update_config(new_config).unwrap();

    assert_eq!(processor.config.threshold, 0.8);
}
