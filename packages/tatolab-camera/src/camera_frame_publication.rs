// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What every capture backend publishes its frames through: the node output
//! pool they are converted into, the log target, and the counts a reporting
//! window keeps.

use std::fmt;

/// The node output pool the converted frames rotate through.
pub(crate) const CONVERTED_FRAME_POOL_KEY: &str = "camera_converted_frames";
pub(crate) const CONVERTED_FRAME_TEXTURE_FORMAT: &str = "rgba8_unorm";
pub(crate) const CONVERTED_FRAME_TEXTURE_USAGE: [&str; 2] = ["storage_binding", "texture_binding"];

/// Frames between two reports of the capture loop's counts and latency.
pub(crate) const FRAMES_BETWEEN_REPORTS: u64 = 600;

/// The log target this module's records carry.
pub(crate) const CAMERA_LOG_TARGET: &str = "tatolab_camera";

/// Why a camera could not be opened, or why its capture ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CameraCaptureRefusal(pub String);

impl fmt::Display for CameraCaptureRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for CameraCaptureRefusal {}

/// The counts and the capture-to-publish latency one reporting window saw.
#[derive(Debug, Default)]
pub(crate) struct CaptureLoopReportingWindow {
    pub(crate) frames_published: u64,
    pub(crate) frames_dropped: u64,
    pub(crate) capture_to_publish_ns: Vec<i64>,
}

impl CaptureLoopReportingWindow {
    pub(crate) fn summary(&mut self) -> String {
        self.capture_to_publish_ns.sort_unstable();
        let percentile = |fraction: f64| -> f64 {
            if self.capture_to_publish_ns.is_empty() {
                return 0.0;
            }
            let index = ((self.capture_to_publish_ns.len() - 1) as f64 * fraction).round();
            self.capture_to_publish_ns[index as usize] as f64 / 1_000_000.0
        };
        format!(
            "frames_published={} frames_dropped={} capture_to_publish_ms p50={:.2} p95={:.2} \
             p99={:.2}",
            self.frames_published,
            self.frames_dropped,
            percentile(0.5),
            percentile(0.95),
            percentile(0.99)
        )
    }
}
