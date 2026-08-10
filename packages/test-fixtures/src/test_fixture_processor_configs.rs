// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Config types for the fixture processors — the seam their encoding is
//! pinned at.

pub use crate::_generated_::{
    ComputeKernelTestProcessorConfig, ConcurrentEscalateTestProcessorConfig,
    EscalateSmokeTestProcessorConfig, GpuAcquireTestProcessorConfig,
    GraphicsKernelSmokeTestProcessorConfig, LifecycleProbeProcessorConfig,
    PanickingContinuousLifecycleProcessorConfig, PanickingManualLifecycleProcessorConfig,
    RayTracingKernelSmokeTestProcessorConfig, TestConfiguredProcessorConfig,
};

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
