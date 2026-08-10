// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Config types for the fixture processors — the seam their encoding is
//! pinned at.

use serde::{Deserialize, Serialize};

/// Compute-kernel CPU-reference fixture: buffer length and where to write the
/// comparison result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ComputeKernelTestProcessorConfig {
    pub element_count: u32,
    pub output_path: String,
}

/// Concurrent-escalate fixture: how many threads contend and how long each
/// holds the gate.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConcurrentEscalateTestProcessorConfig {
    pub hold_ms: u32,
    pub output_path: String,
    pub thread_count: u32,
}

/// Escalate smoke fixture: where to record that the round trip completed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EscalateSmokeTestProcessorConfig {
    pub output_path: String,
}

/// GPU-acquire fixture: the pixel-buffer dimensions to acquire.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GpuAcquireTestProcessorConfig {
    pub height: u32,
    pub output_path: String,
    pub width: u32,
}

/// Graphics-kernel smoke fixture: where to record that the render completed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GraphicsKernelSmokeTestProcessorConfig {
    pub output_path: String,
}

/// Lifecycle-probe fixture: how many process iterations to run and where to
/// append the per-hook markers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LifecycleProbeProcessorConfig {
    pub max_iterations: u32,
    pub output_path: String,
}

/// Panic-injection Continuous fixture: which lifecycle hook panics.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PanickingContinuousLifecycleProcessorConfig {
    pub panic_at_hook: String,
}

/// Panic-injection Manual fixture: which lifecycle hook panics.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PanickingManualLifecycleProcessorConfig {
    pub panic_at_hook: String,
}

/// Ray-tracing-kernel smoke fixture: where to record that the trace completed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RayTracingKernelSmokeTestProcessorConfig {
    pub output_path: String,
}

/// Attribute-macro config-emit fixture: one scalar field to round-trip.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TestConfiguredProcessorConfig {
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
            ComputeKernelTestProcessorConfig,
            r#"{"element_count":16,"output_path":"/tmp/compute.txt"}"#
        );
        assert_config_round_trips!(
            ConcurrentEscalateTestProcessorConfig,
            r#"{"hold_ms":5,"output_path":"/tmp/concurrent.txt","thread_count":4}"#
        );
        assert_config_round_trips!(
            EscalateSmokeTestProcessorConfig,
            r#"{"output_path":"/tmp/escalate.txt"}"#
        );
        assert_config_round_trips!(
            GpuAcquireTestProcessorConfig,
            r#"{"height":480,"output_path":"/tmp/acquire.txt","width":640}"#
        );
        assert_config_round_trips!(
            GraphicsKernelSmokeTestProcessorConfig,
            r#"{"output_path":"/tmp/graphics.txt"}"#
        );
        assert_config_round_trips!(
            LifecycleProbeProcessorConfig,
            r#"{"max_iterations":3,"output_path":"/tmp/lifecycle.txt"}"#
        );
        assert_config_round_trips!(
            PanickingContinuousLifecycleProcessorConfig,
            r#"{"panic_at_hook":"process"}"#
        );
        assert_config_round_trips!(
            PanickingManualLifecycleProcessorConfig,
            r#"{"panic_at_hook":"setup"}"#
        );
        assert_config_round_trips!(
            RayTracingKernelSmokeTestProcessorConfig,
            r#"{"output_path":"/tmp/raytracing.txt"}"#
        );
        assert_config_round_trips!(TestConfiguredProcessorConfig, r#"{"threshold":0.5}"#);
    }
}
