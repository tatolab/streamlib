// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use serde_json::Value as JsonValue;

use super::JsonComponent;

/// Runtime metrics for a processor.
#[derive(Default, Clone)]
pub struct ProcessorMetrics {
    /// Frames per second throughput.
    pub throughput_fps: f64,
    /// 50th percentile latency in milliseconds.
    pub latency_p50_ms: f64,
    /// 99th percentile latency in milliseconds.
    pub latency_p99_ms: f64,
    /// Total frames processed.
    pub frames_processed: u64,
    /// Total frames dropped.
    pub frames_dropped: u64,
}

impl JsonComponent for ProcessorMetrics {
    fn json_key(&self) -> &'static str {
        "metrics"
    }

    fn to_json(&self) -> JsonValue {
        serde_json::json!({
            "throughput_fps": self.throughput_fps,
            "latency_p50_ms": self.latency_p50_ms,
            "latency_p99_ms": self.latency_p99_ms,
            "frames_processed": self.frames_processed,
            "frames_dropped": self.frames_dropped
        })
    }
}
