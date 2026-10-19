// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in test-pattern source: the sample stream's animation at ~30 fps,
//! rendered on the GPU — the camera-free way to see a pipeline produce.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::media_clock::MediaClock;
use streamlib::sdk::processors::ContinuousProcessor;
use streamlib::sdk::schemars::JsonSchema;

use crate::consecutive_failure_report_schedule::ConsecutiveFailureReportSchedule;
use crate::test_pattern_frame_renderer::{TestPatternFrameRenderOutcome, TestPatternFrameRenderer};
use crate::video_frame::{ColorInfo, Primaries, Range, Transfer, VideoFrame};

/// Configuration for [`TestPatternSource`]: frame size only — the animation,
/// rate, and pixel format are fixed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(crate = "streamlib::sdk::schemars")]
#[serde(deny_unknown_fields)]
pub struct TestPatternSourceConfig {
    /// Frame width in pixels.
    #[serde(default = "default_width")]
    pub width: u32,
    /// Frame height in pixels.
    #[serde(default = "default_height")]
    pub height: u32,
}

fn default_width() -> u32 {
    1280
}

fn default_height() -> u32 {
    720
}

impl Default for TestPatternSourceConfig {
    fn default() -> Self {
        Self {
            width: default_width(),
            height: default_height(),
        }
    }
}

const TEST_PATTERN_FPS: u32 = 30;

/// A run of dropped frames is reported at its first and then at every this
/// many, so a pool downstream holds full is said once a minute, not per tick.
const DROPPED_FRAMES_BETWEEN_REPORTS: u64 = 1800;

#[streamlib::sdk::processor(
    built_in_node,
    description = "Synthetic SMPTE-style color bars with a bouncing potato and the sample stream's captions, rendered on the GPU — demos a pipeline with no camera attached",
    execution = continuous(interval_ms = 33),
    config = crate::test_pattern_source::TestPatternSourceConfig,
    output(
        "video",
        description = "Test-pattern video frames",
        catalog = BinaryEntry { mode: Snapshot, mime: "application/msgpack" }
    ),
)]
pub struct TestPatternSource {
    rendering: Option<TestPatternRendering>,
}

/// The renderer and where the animation has reached, from `setup` to
/// `teardown`.
struct TestPatternRendering {
    frame_renderer: TestPatternFrameRenderer,
    /// The index the next published frame renders; advances only on a
    /// published frame, so frame N's picture never depends on a dropped one.
    next_frame_index: u64,
    dropped_frame_report_schedule: ConsecutiveFailureReportSchedule,
}

impl ContinuousProcessor for TestPatternSource::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let processor_id = ctx.processor_id().ok_or_else(|| {
            Error::Configuration(
                "TestPatternSource needs its node's processor id to give its frames a node \
                 output pool; this context carries none"
                    .into(),
            )
        })?;
        self.rendering = Some(TestPatternRendering {
            frame_renderer: TestPatternFrameRenderer::create(
                ctx.gpu_full_access(),
                &processor_id,
                self.config.width,
                self.config.height,
            )?,
            next_frame_index: 0,
            dropped_frame_report_schedule: ConsecutiveFailureReportSchedule::reporting_every(
                DROPPED_FRAMES_BETWEEN_REPORTS,
            ),
        });
        tracing::info!(
            width = self.config.width,
            height = self.config.height,
            "TestPatternSource: rendering on the GPU"
        );
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.rendering = None;
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        let Some(rendering) = self.rendering.as_mut() else {
            return Ok(());
        };
        let (published_frame_id, _pooled_pixel_buffer_held_until_written) = match rendering
            .frame_renderer
            .render_frame(rendering.next_frame_index)?
        {
            TestPatternFrameRenderOutcome::Rendered {
                published_pixel_buffer_frame_id,
                pooled_pixel_buffer,
            } => (published_pixel_buffer_frame_id, pooled_pixel_buffer),
            TestPatternFrameRenderOutcome::DroppedByTheOutputPool { refusal } => {
                if rendering
                    .dropped_frame_report_schedule
                    .note_failure_and_say_whether_to_report()
                {
                    tracing::warn!(
                        consecutive_dropped_frame_report_schedule = rendering
                            .dropped_frame_report_schedule
                            .consecutive_failures(),
                        frame_index = rendering.next_frame_index,
                        "TestPatternSource: dropped a frame: {refusal}"
                    );
                }
                return Ok(());
            }
        };
        rendering.dropped_frame_report_schedule.note_success();
        rendering.next_frame_index += 1;

        let frame = VideoFrame {
            surface_id: published_frame_id.to_string(),
            width: self.config.width,
            height: self.config.height,
            timestamp_ns: MediaClock::now().as_nanos() as i64,
            fps: Some(TEST_PATTERN_FPS),
            color_info: Some(ColorInfo {
                primaries: Some(Primaries::Bt709),
                transfer: Some(Transfer::Srgb),
                matrix: None,
                range: Some(Range::Full),
            }),
            content_light: None,
            mastering_display: None,
            texture_layout: None,
        };
        self.outputs.write_msgpack("video", &frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib::sdk::processors::GeneratedProcessor;

    #[test]
    fn config_defaults_to_720p() {
        let config: TestPatternSourceConfig = serde_json::from_str("{}").expect("empty config");
        assert_eq!((config.width, config.height), (1280, 720));
    }

    /// What an agent reads before it adds this processor. The defaults live in
    /// two `#[serde(default = …)]` functions and nowhere else, so a document
    /// that does not carry them is one the agent has to guess against.
    #[test]
    fn the_descriptor_publishes_each_config_fields_type_description_and_default() {
        let descriptor = <TestPatternSource::Processor as GeneratedProcessor>::descriptor()
            .expect("a descriptor");
        let config_schema = descriptor.config_schema.expect("a config schema");

        for (field, default, description) in [
            ("width", 1280, "Frame width in pixels."),
            ("height", 720, "Frame height in pixels."),
        ] {
            let rendered = &config_schema["properties"][field];
            assert_eq!(rendered["type"], "integer", "{field}");
            assert_eq!(rendered["default"], default, "{field}");
            assert_eq!(rendered["description"], description, "{field}");
        }
    }
}
