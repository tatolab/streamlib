// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Frame Boost Processor - Increases effective frame rate by running continuously.
//!
//! Runs at 120fps (8ms intervals) and passes through the most recent video frame.
//! This can help smooth out frame delivery to downstream processors.

use serde::{Deserialize, Serialize};
use streamlib::core::{LinkInput, LinkOutput, Result, RuntimeContext, VideoFrame};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, streamlib::ConfigDescriptor)]
pub struct FrameBoostConfig {
    /// Target frame interval in milliseconds (default: 8 for ~120fps).
    pub interval_ms: u32,
}

impl Default for FrameBoostConfig {
    fn default() -> Self {
        Self { interval_ms: 8 }
    }
}

#[streamlib::processor(
    name = "FrameBoost",
    execution = Continuous,
    execution_interval_ms = 8,
    description = "Boosts frame rate to 120fps by continuously emitting the latest frame"
)]
pub struct FrameBoostProcessor {
    #[streamlib::input(description = "Video frames input")]
    video_in: LinkInput<VideoFrame>,

    #[streamlib::output(description = "Video frames output (boosted rate)")]
    video_out: LinkOutput<VideoFrame>,

    #[streamlib::config]
    config: FrameBoostConfig,

    last_frame: Option<VideoFrame>,
    frames_received: u64,
    frames_emitted: u64,
}

impl streamlib::core::ContinuousProcessor for FrameBoostProcessor::Processor {
    fn setup(
        &mut self,
        _ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!(
            "FrameBoost: Starting (interval={}ms, ~{}fps)",
            self.config.interval_ms,
            1000 / self.config.interval_ms.max(1)
        );
        std::future::ready(Ok(()))
    }

    fn teardown(&mut self) -> impl std::future::Future<Output = Result<()>> + Send {
        tracing::info!(
            "FrameBoost: Shutdown (received={}, emitted={}, boost={:.1}x)",
            self.frames_received,
            self.frames_emitted,
            self.frames_emitted as f64 / self.frames_received.max(1) as f64
        );
        std::future::ready(Ok(()))
    }

    fn process(&mut self) -> Result<()> {
        // Check for new frame
        if let Some(frame) = self.video_in.read() {
            self.last_frame = Some(frame);
            self.frames_received += 1;
        }

        // Emit the last frame if we have one
        if let Some(ref frame) = self.last_frame {
            self.video_out.write(frame.clone());
            self.frames_emitted += 1;
        }

        Ok(())
    }
}
