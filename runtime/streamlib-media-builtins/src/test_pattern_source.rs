// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Built-in test-pattern source: SMPTE-style color bars at ~30 fps, no
//! hardware required — the camera-free way to see a pipeline produce.

use serde::{Deserialize, Serialize};
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::Result;
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

/// Fill a tightly-packed RGBA plane with vertical SMPTE bars. Rows are
/// `width * 4` bytes; a slice shorter than a whole row count fills fewer rows.
pub(crate) fn fill_smpte_bars_rgba(plane: &mut [u8], width: u32) {
    let bar_count = SMPTE_BAR_COLORS_RGBA.len() as u32;
    for row in plane.chunks_exact_mut(width as usize * 4) {
        for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
            let bar = ((x as u32 * bar_count) / width).min(bar_count - 1) as usize;
            pixel.copy_from_slice(&SMPTE_BAR_COLORS_RGBA[bar]);
        }
    }
}

const TEST_PATTERN_FPS: u32 = 30;

/// The one-shot acquisition of the pattern surface, as a single state.
enum TestPatternSurfaceState {
    NotYetAcquired,
    Ready {
        pool_id: PixelBufferPoolId,
        /// Held for the processor's lifetime: the [`PixelBuffer`] keeps the
        /// pool slot (and thus the surface id) alive.
        _pixel_buffer: PixelBuffer,
    },
    /// Acquisition failed once, loudly; every later tick stays quiet instead
    /// of re-erroring at 30 Hz.
    AcquireFailedPermanently,
}

impl Default for TestPatternSurfaceState {
    fn default() -> Self {
        Self::NotYetAcquired
    }
}

#[streamlib::sdk::processor(
    "@tatolab/media-builtins/TestPatternSource",
    description = "Synthetic SMPTE-style color-bar source — demos a pipeline with no camera attached",
    execution = continuous(interval_ms = 33),
    config = crate::test_pattern_source::TestPatternSourceConfig,
    output("video", any, description = "Test-pattern video frames"),
)]
pub struct TestPatternSource {
    surface_state: TestPatternSurfaceState,
}

impl TestPatternSource::Processor {
    fn acquire_and_fill_pattern_surface(
        &self,
        ctx: &RuntimeContextLimitedAccess<'_>,
    ) -> Result<TestPatternSurfaceState> {
        let width = self.config.width;
        let height = self.config.height;
        let (pool_id, pixel_buffer) =
            ctx.gpu_limited_access()
                .acquire_pixel_buffer(width, height, PixelFormat::Rgba32)?;

        let plane_pointer = pixel_buffer.plane_base_address(0);
        let plane_size = pixel_buffer.plane_size(0) as usize;
        let expected_size = (width as usize) * (height as usize) * 4;
        if plane_pointer.is_null() || plane_size < expected_size {
            return Err(streamlib::sdk::error::Error::Runtime(format!(
                "TestPatternSource: pixel-buffer plane unusable (ptr null: {}, size {} < \
                 expected {})",
                plane_pointer.is_null(),
                plane_size,
                expected_size
            )));
        }
        // SAFETY: `plane_pointer` is the mapped base address of plane 0 of a
        // freshly-acquired HOST_VISIBLE pixel buffer, valid for `plane_size`
        // bytes for the buffer's lifetime; the buffer is held in the returned
        // state until teardown.
        let plane = unsafe { std::slice::from_raw_parts_mut(plane_pointer, expected_size) };
        fill_smpte_bars_rgba(plane, width);

        tracing::info!(
            width,
            height,
            surface_id = %pool_id,
            "TestPatternSource: pattern surface ready"
        );
        Ok(TestPatternSurfaceState::Ready {
            pool_id,
            _pixel_buffer: pixel_buffer,
        })
    }
}

impl ContinuousProcessor for TestPatternSource::Processor {
    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        self.surface_state = TestPatternSurfaceState::NotYetAcquired;
        Ok(())
    }

    fn process(&mut self, ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if let TestPatternSurfaceState::NotYetAcquired = self.surface_state {
            match self.acquire_and_fill_pattern_surface(ctx) {
                Ok(ready) => self.surface_state = ready,
                Err(acquire_error) => {
                    self.surface_state = TestPatternSurfaceState::AcquireFailedPermanently;
                    return Err(acquire_error);
                }
            }
        }
        let pool_id = match &self.surface_state {
            TestPatternSurfaceState::Ready { pool_id, .. } => pool_id,
            TestPatternSurfaceState::AcquireFailedPermanently => return Ok(()),
            TestPatternSurfaceState::NotYetAcquired => unreachable!("acquired or failed above"),
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
        fill_smpte_bars_rgba(&mut plane, width);

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
        fill_smpte_bars_rgba(&mut plane, width);
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
