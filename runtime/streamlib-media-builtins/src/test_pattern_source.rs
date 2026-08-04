// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in test-pattern source: SMPTE-style color bars at ~30 fps, no
//! hardware required — the camera-free way to see a pipeline produce.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{
    GpuContextLimitedAccess, RuntimeContextFullAccess, RuntimeContextLimitedAccess,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::media_clock::MediaClock;
use streamlib::sdk::processors::ContinuousProcessor;
use streamlib::sdk::rhi::{PixelBuffer, PixelBufferPoolId, PixelFormat};

use crate::video_frame::{ColorInfo, Primaries, Range, Transfer, VideoFrame};

/// Configuration for [`TestPatternSource`]: frame size only — the pattern,
/// rate, and pixel format are fixed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// The eight full-saturation SMPTE bars, left to right, as RGBA bytes.
const SMPTE_BAR_COLORS_RGBA: [[u8; 4]; 8] = [
    [255, 255, 255, 255], // white
    [255, 255, 0, 255],   // yellow
    [0, 255, 255, 255],   // cyan
    [0, 255, 0, 255],     // green
    [255, 0, 255, 255],   // magenta
    [255, 0, 0, 255],     // red
    [0, 0, 255, 255],     // blue
    [0, 0, 0, 255],       // black
];

/// Fill a tightly-packed RGBA plane with vertical SMPTE bars.
pub(crate) fn fill_smpte_bars_rgba(plane: &mut [u8], width: u32, height: u32) {
    let bar_count = SMPTE_BAR_COLORS_RGBA.len() as u32;
    for y in 0..height {
        for x in 0..width {
            let bar = ((x * bar_count) / width).min(bar_count - 1) as usize;
            let offset = ((y * width + x) * 4) as usize;
            plane[offset..offset + 4].copy_from_slice(&SMPTE_BAR_COLORS_RGBA[bar]);
        }
    }
}

const TEST_PATTERN_FPS: u32 = 30;

#[streamlib::sdk::processor(
    "@tatolab/media-builtins/TestPatternSource",
    description = "Synthetic SMPTE-style color-bar source — demos a pipeline with no camera attached",
    execution = continuous(interval_ms = 33),
    config = crate::test_pattern_source::TestPatternSourceConfig,
    output("video", any, description = "Test-pattern video frames"),
)]
pub struct TestPatternSource {
    gpu_context: Option<GpuContextLimitedAccess>,
    /// The pattern surface, acquired once and republished every tick. The
    /// held [`PixelBuffer`] keeps the pool slot (and thus the surface id)
    /// alive for the processor's lifetime.
    pattern_surface: Option<(PixelBufferPoolId, PixelBuffer)>,
    pattern_acquire_failed: bool,
}

impl TestPatternSource::Processor {
    fn acquire_and_fill_pattern_surface(&mut self) -> Result<PixelBufferPoolId> {
        if self.pattern_surface.is_none() {
            let gpu_context = self.gpu_context.as_ref().ok_or_else(|| {
                Error::Runtime("TestPatternSource: setup() did not stash the GPU context".into())
            })?;
            let width = self.config.width;
            let height = self.config.height;
            let (pool_id, pixel_buffer) =
                gpu_context.acquire_pixel_buffer(width, height, PixelFormat::Rgba32)?;

            let plane_pointer = pixel_buffer.plane_base_address(0);
            let plane_size = pixel_buffer.plane_size(0) as usize;
            let expected_size = (width as usize) * (height as usize) * 4;
            if plane_pointer.is_null() || plane_size < expected_size {
                return Err(Error::Runtime(format!(
                    "TestPatternSource: pixel-buffer plane unusable (ptr null: {}, size {} < \
                     expected {})",
                    plane_pointer.is_null(),
                    plane_size,
                    expected_size
                )));
            }
            // SAFETY: `plane_pointer` is the mapped base address of plane 0
            // of a freshly-acquired HOST_VISIBLE pixel buffer, valid for
            // `plane_size` bytes for the buffer's lifetime; the buffer is
            // held on `self` until teardown.
            let plane = unsafe { std::slice::from_raw_parts_mut(plane_pointer, expected_size) };
            fill_smpte_bars_rgba(plane, width, height);

            tracing::info!(
                width,
                height,
                surface_id = %pool_id,
                "TestPatternSource: pattern surface ready"
            );
            self.pattern_surface = Some((pool_id, pixel_buffer));
        }
        let (pool_id, _pixel_buffer) = self.pattern_surface.as_ref().expect("just filled");
        Ok(pool_id.clone())
    }
}

impl ContinuousProcessor for TestPatternSource::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.gpu_context = Some(ctx.gpu_limited_access().clone());
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.pattern_surface = None;
        self.gpu_context = None;
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if self.pattern_acquire_failed {
            return Ok(());
        }
        let pool_id = match self.acquire_and_fill_pattern_surface() {
            Ok(surface_pool_id) => surface_pool_id,
            Err(acquire_error) => {
                // Fail once, loudly; stay quiet afterwards instead of
                // re-erroring every 33 ms tick.
                self.pattern_acquire_failed = true;
                return Err(acquire_error);
            }
        };

        let frame = VideoFrame {
            surface_id: pool_id.to_string(),
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
        self.outputs.write("video", &frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bars_cover_the_full_width_in_order() {
        let (width, height) = (64u32, 4u32);
        let mut plane = vec![0u8; (width * height * 4) as usize];
        fill_smpte_bars_rgba(&mut plane, width, height);

        let pixel = |x: u32, y: u32| {
            let offset = ((y * width + x) * 4) as usize;
            [
                plane[offset],
                plane[offset + 1],
                plane[offset + 2],
                plane[offset + 3],
            ]
        };
        // 64px / 8 bars = 8px per bar; sample each bar's center on two rows.
        for (bar, expected) in SMPTE_BAR_COLORS_RGBA.iter().enumerate() {
            let x = (bar as u32) * 8 + 4;
            assert_eq!(pixel(x, 0), *expected, "bar {bar} row 0");
            assert_eq!(pixel(x, 3), *expected, "bar {bar} row 3");
        }
    }

    #[test]
    fn bars_fill_odd_widths_without_gaps() {
        let (width, height) = (61u32, 2u32);
        let mut plane = vec![7u8; (width * height * 4) as usize];
        fill_smpte_bars_rgba(&mut plane, width, height);
        // Every alpha byte written → no pixel skipped.
        for x in 0..width {
            let offset = ((x * 4) + 3) as usize;
            assert_eq!(plane[offset], 255, "pixel {x} alpha written");
        }
    }

    #[test]
    fn config_defaults_to_720p() {
        let config: TestPatternSourceConfig = serde_json::from_str("{}").expect("empty config");
        assert_eq!((config.width, config.height), (1280, 720));
    }
}
