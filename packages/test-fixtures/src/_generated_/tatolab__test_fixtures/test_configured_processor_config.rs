// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Generated from JTD schema using jtd-codegen. DO NOT EDIT.


use serde::{Deserialize, Serialize};

/// Test config schema for the attribute macro tests.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestConfiguredProcessorConfig {
    /// Threshold value for the test processor.
    #[serde(rename = "threshold")]
    pub threshold: f32,
}
