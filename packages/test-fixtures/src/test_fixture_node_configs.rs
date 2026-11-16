// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use serde::{Deserialize, Serialize};
use streamlib::sdk::schemars::JsonSchema;

/// Compute-kernel CPU-reference fixture: buffer length and where to write the
/// comparison result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct ComputeKernelTestNodeConfig {
    pub element_count: u32,
    pub output_path: String,
}

/// Concurrent-escalate fixture: how many threads contend and how long each
/// holds the gate.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct ConcurrentEscalateTestNodeConfig {
    pub hold_ms: u32,
    pub output_path: String,
    pub thread_count: u32,
}

/// Escalate smoke fixture: where to record that the round trip completed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct EscalateSmokeTestNodeConfig {
    pub output_path: String,
}

/// GPU-acquire fixture: the pixel-buffer dimensions to acquire.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct GpuAcquireTestNodeConfig {
    pub height: u32,
    pub output_path: String,
    pub width: u32,
}

/// Graphics-kernel smoke fixture: where to record that the render completed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct GraphicsKernelSmokeTestNodeConfig {
    pub output_path: String,
}

/// Lifecycle-probe fixture: how many process iterations to run and where to
/// append the per-hook markers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct LifecycleProbeNodeConfig {
    pub max_iterations: u32,
    pub output_path: String,
}

/// Panic-injection Continuous fixture: which lifecycle hook panics.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct PanickingContinuousLifecycleNodeConfig {
    pub panic_at_hook: String,
}

/// Panic-injection Manual fixture: which lifecycle hook panics.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct PanickingManualLifecycleNodeConfig {
    pub panic_at_hook: String,
}

/// Ray-tracing-kernel smoke fixture: where to record that the trace completed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct RayTracingKernelSmokeTestNodeConfig {
    pub output_path: String,
}

/// Attribute-macro config-emit fixture: one scalar field to round-trip.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
pub struct TestConfiguredNodeConfig {
    pub threshold: f32,
}

#[cfg(test)]
mod fixture_config_encoding_tests {
    use super::*;

    /// Each fixture config decodes from, and re-encodes to, its golden document.
    macro_rules! assert_config_round_trips {
        ($ty:ty, $golden:expr) => {{
            let decoded: $ty = serde_json::from_str($golden).unwrap();
            assert_eq!(serde_json::to_string(&decoded).unwrap(), $golden);
        }};
    }

    #[test]
    fn every_fixture_config_round_trips() {
        assert_config_round_trips!(
            ComputeKernelTestNodeConfig,
            r#"{"element_count":16,"output_path":"/tmp/compute.txt"}"#
        );
        assert_config_round_trips!(
            ConcurrentEscalateTestNodeConfig,
            r#"{"hold_ms":5,"output_path":"/tmp/concurrent.txt","thread_count":4}"#
        );
        assert_config_round_trips!(
            EscalateSmokeTestNodeConfig,
            r#"{"output_path":"/tmp/escalate.txt"}"#
        );
        assert_config_round_trips!(
            GpuAcquireTestNodeConfig,
            r#"{"height":480,"output_path":"/tmp/acquire.txt","width":640}"#
        );
        assert_config_round_trips!(
            GraphicsKernelSmokeTestNodeConfig,
            r#"{"output_path":"/tmp/graphics.txt"}"#
        );
        assert_config_round_trips!(
            LifecycleProbeNodeConfig,
            r#"{"max_iterations":3,"output_path":"/tmp/lifecycle.txt"}"#
        );
        assert_config_round_trips!(
            PanickingContinuousLifecycleNodeConfig,
            r#"{"panic_at_hook":"process"}"#
        );
        assert_config_round_trips!(
            PanickingManualLifecycleNodeConfig,
            r#"{"panic_at_hook":"setup"}"#
        );
        assert_config_round_trips!(
            RayTracingKernelSmokeTestNodeConfig,
            r#"{"output_path":"/tmp/raytracing.txt"}"#
        );
        assert_config_round_trips!(TestConfiguredNodeConfig, r#"{"threshold":0.5}"#);
    }
}
