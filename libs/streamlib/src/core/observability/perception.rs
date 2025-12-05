// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Perception capabilities for AI agents.

use crate::core::error::Result;
use crate::core::graph::ProcessorId;

use super::snapshots::GraphHealth;

/// Configuration for sampling video frames.
#[derive(Debug, Clone)]
pub struct SampleConfig {
    /// Target width for thumbnail.
    pub width: u32,
    /// Target height for thumbnail.
    pub height: u32,
}

impl SampleConfig {
    /// Create a thumbnail sample config.
    pub fn thumbnail(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// A sampled video frame for AI perception.
#[derive(Debug, Clone)]
pub struct SampledFrame {
    /// Frame data (RGB bytes).
    pub data: Vec<u8>,
    /// Frame width.
    pub width: u32,
    /// Frame height.
    pub height: u32,
    /// Timestamp in nanoseconds.
    pub timestamp_ns: i64,
}

/// A sampled audio buffer for AI perception.
#[derive(Debug, Clone)]
pub struct SampledAudio {
    /// Audio samples (f32 interleaved).
    pub samples: Vec<f32>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Number of channels.
    pub channels: u32,
    /// Duration in milliseconds.
    pub duration_ms: u32,
}

/// Current status of a processor.
#[derive(Debug, Clone)]
pub struct ProcessorStatus {
    /// Processor ID.
    pub id: ProcessorId,
    /// Whether the processor is running.
    pub is_running: bool,
    /// Current throughput in FPS.
    pub throughput_fps: f64,
    /// Error message if any.
    pub error: Option<String>,
}

/// Perception capabilities for AI agents.
pub trait AgentPerception: Send + Sync {
    /// Sample a video frame from a processor's output.
    fn sample_video(&self, id: &ProcessorId, config: SampleConfig) -> Option<SampledFrame>;

    /// Sample audio from a processor's output.
    fn sample_audio(&self, id: &ProcessorId, duration_ms: u32) -> Option<SampledAudio>;

    /// Get current status of a processor.
    fn processor_status(&self, id: &ProcessorId) -> Option<ProcessorStatus>;

    /// Get overall graph health.
    fn graph_health(&self) -> GraphHealth;
}

/// Actions an AI agent can take.
pub trait AgentActions: Send + Sync {
    /// Update a processor's config.
    fn update_config(&self, id: &ProcessorId, config: serde_json::Value) -> Result<()>;

    /// Add a processor dynamically.
    fn add_processor(&self, processor_type: &str, config: serde_json::Value)
        -> Result<ProcessorId>;

    /// Remove a processor.
    fn remove_processor(&self, id: &ProcessorId) -> Result<()>;
}
