// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The test pattern's kernel, run on whatever device the machine has —
//! lavapipe in CI: every frame is exactly the bars, the potato and the caption
//! panel where frame N puts them, and two renderers render frame N to the same
//! bytes. The codec check's drift baseline is taken on this source, so a frame
//! that varied with anything but its index would move the baseline with no
//! codec change.

use std::sync::{Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

use crate::test_pattern_frame_renderer::{
    BakedRgbaOverlay, OverlayTopLeftInTheFrame, POTATO_SPRITE, SAMPLE_STREAM_CAPTION_PANEL,
    TestPatternFrameRenderOutcome, TestPatternFrameRenderer, caption_panel_top_left,
    potato_sprite_top_left_at_frame,
};
use streamlib::sdk::App;
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::processors::{ContinuousProcessor, PROCESSOR_REGISTRY};

const FRAME_WIDTH: u32 = 640;
const FRAME_HEIGHT: u32 = 360;

/// Frame zero, the next one, one mid-sweep, one at the bottom edge (200 px of
/// vertical travel at 4 px a frame), and one just past a bounce.
const FRAME_INDICES_RENDERED: [u64; 5] = [0, 1, 37, 50, 53];

const RESULT_TIMEOUT: Duration = Duration::from_secs(60);

const SMPTE_BAR_COLORS_RGB: [[u8; 3]; 8] = [
    [255, 255, 255],
    [255, 255, 0],
    [0, 255, 255],
    [0, 255, 0],
    [255, 0, 255],
    [255, 0, 0],
    [0, 0, 255],
    [0, 0, 0],
];

/// Each renderer's frames, in `FRAME_INDICES_RENDERED` order.
type FramesOfEachRenderer = [Vec<Vec<u8>>; 2];

fn rendered_frames() -> &'static Mutex<Option<std::result::Result<FramesOfEachRenderer, String>>> {
    static RENDERED_FRAMES: OnceLock<
        Mutex<Option<std::result::Result<FramesOfEachRenderer, String>>>,
    > = OnceLock::new();
    RENDERED_FRAMES.get_or_init(|| Mutex::new(None))
}

fn render_every_frame_index_with_a_fresh_renderer(
    ctx: &RuntimeContextFullAccess<'_>,
) -> Result<Vec<Vec<u8>>> {
    let processor_id = ctx
        .processor_id()
        .ok_or_else(|| Error::Configuration("the probe has no processor id".into()))?;
    let mut renderer = TestPatternFrameRenderer::create(
        ctx.gpu_full_access(),
        &processor_id,
        FRAME_WIDTH,
        FRAME_HEIGHT,
    )?;
    let frame_byte_count = (FRAME_WIDTH * FRAME_HEIGHT * 4) as usize;
    FRAME_INDICES_RENDERED
        .iter()
        .map(|&frame_index| {
            let pixel_buffer = match renderer.render_frame(frame_index)? {
                TestPatternFrameRenderOutcome::Rendered {
                    pooled_pixel_buffer,
                    ..
                } => pooled_pixel_buffer,
                TestPatternFrameRenderOutcome::DroppedByTheOutputPool { refusal } => {
                    return Err(refusal);
                }
            };
            let plane = pixel_buffer.plane_base_address(0);
            assert!(!plane.is_null(), "the pooled frame is host-mapped");
            assert!(pixel_buffer.plane_size(0) as usize >= frame_byte_count);
            // SAFETY: `plane` maps at least `frame_byte_count` bytes, checked
            // above, and the renderer waited for the kernel before returning.
            Ok(unsafe { std::slice::from_raw_parts(plane, frame_byte_count) }.to_vec())
        })
        .collect()
}

/// Renders every frame index twice, through two renderers, in its setup.
#[streamlib::sdk::processor(
    description = "Renders test-pattern frames through two renderers for the test's assertions",
    execution = continuous(interval_ms = 1000)
)]
pub struct TestPatternFrameRenderingProbe;

impl ContinuousProcessor for TestPatternFrameRenderingProbe::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let rendered = render_every_frame_index_with_a_fresh_renderer(ctx).and_then(|first| {
            render_every_frame_index_with_a_fresh_renderer(ctx).map(|second| [first, second])
        });
        *rendered_frames().lock().unwrap() = Some(rendered.map_err(|error| error.to_string()));
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        Ok(())
    }
}

fn run_the_probe() -> FramesOfEachRenderer {
    static REGISTER: Once = Once::new();
    REGISTER.call_once(|| {
        PROCESSOR_REGISTRY.register::<TestPatternFrameRenderingProbe::Processor>();
    });

    let project_directory = tempfile::tempdir().expect("a project directory");
    let app = App::new_in_project_directory(project_directory.path()).expect("a runtime");
    app.add(
        TestPatternFrameRenderingProbe::Processor::processor_class_import_path(),
        serde_json::json!({}),
        Some("probe"),
    )
    .expect("the probe");
    app.stream().start().expect("the graph starts");

    let waiting_since = Instant::now();
    let rendered = loop {
        if let Some(rendered) = rendered_frames().lock().unwrap().take() {
            break rendered;
        }
        assert!(
            waiting_since.elapsed() < RESULT_TIMEOUT,
            "the probe rendered nothing within {RESULT_TIMEOUT:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    app.stream().stop().expect("the graph stops cleanly");
    rendered.expect("both renderers render every frame")
}

fn composite_over_opaque(above: &[u8], below: [u8; 3]) -> [u8; 3] {
    let above_alpha = u32::from(above[3]);
    std::array::from_fn(|channel| {
        ((u32::from(above[channel]) * above_alpha
            + u32::from(below[channel]) * (255 - above_alpha)
            + 127)
            / 255) as u8
    })
}

fn overlay_pixel_at(
    x: u32,
    y: u32,
    top_left: OverlayTopLeftInTheFrame,
    overlay: &BakedRgbaOverlay,
) -> Option<&'static [u8]> {
    let within_x = x as i32 - top_left.left;
    let within_y = y as i32 - top_left.top;
    if within_x < 0
        || within_y < 0
        || within_x >= overlay.width as i32
        || within_y >= overlay.height as i32
    {
        return None;
    }
    let offset = ((within_y as u32 * overlay.width + within_x as u32) * 4) as usize;
    Some(&overlay.rgba[offset..offset + 4])
}

/// The frame the kernel is held to, computed on the CPU from the same
/// placement and the same rounding.
fn expected_frame(frame_index: u64) -> Vec<u8> {
    let potato_sprite_top_left =
        potato_sprite_top_left_at_frame(frame_index, FRAME_WIDTH, FRAME_HEIGHT);
    let caption_panel = caption_panel_top_left(FRAME_WIDTH, FRAME_HEIGHT);
    let mut frame = Vec::with_capacity((FRAME_WIDTH * FRAME_HEIGHT * 4) as usize);
    for y in 0..FRAME_HEIGHT {
        for x in 0..FRAME_WIDTH {
            let bar = ((x * 8) / FRAME_WIDTH).min(7) as usize;
            let mut pixel = SMPTE_BAR_COLORS_RGB[bar];
            for overlay in [
                overlay_pixel_at(x, y, potato_sprite_top_left, &POTATO_SPRITE),
                overlay_pixel_at(x, y, caption_panel, &SAMPLE_STREAM_CAPTION_PANEL),
            ]
            .into_iter()
            .flatten()
            {
                pixel = composite_over_opaque(overlay, pixel);
            }
            frame.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
        }
    }
    frame
}

fn first_differing_pixel(actual: &[u8], expected: &[u8]) -> Option<(u32, u32, [u8; 4], [u8; 4])> {
    actual
        .chunks_exact(4)
        .zip(expected.chunks_exact(4))
        .position(|(actual_pixel, expected_pixel)| actual_pixel != expected_pixel)
        .map(|pixel_index| {
            let offset = pixel_index * 4;
            (
                pixel_index as u32 % FRAME_WIDTH,
                pixel_index as u32 / FRAME_WIDTH,
                actual[offset..offset + 4].try_into().unwrap(),
                expected[offset..offset + 4].try_into().unwrap(),
            )
        })
}

#[test]
#[cfg_attr(
    not(feature = "hardware-tests"),
    ignore = "needs a Vulkan device (lavapipe will do). Run with --features streamlib-media-builtins/hardware-tests. See docs/testing-hardware.md"
)]
fn every_frame_is_its_index_s_bars_potato_and_captions_and_renders_identically_twice() {
    let [first_renderer_frames, second_renderer_frames] = run_the_probe();

    for (position, &frame_index) in FRAME_INDICES_RENDERED.iter().enumerate() {
        let expected = expected_frame(frame_index);
        if let Some((x, y, actual, expected)) =
            first_differing_pixel(&first_renderer_frames[position], &expected)
        {
            panic!(
                "frame {frame_index} at ({x}, {y}) is {actual:?}, where the bars, the potato \
                 and the captions put {expected:?}"
            );
        }
        assert_eq!(
            first_renderer_frames[position], second_renderer_frames[position],
            "frame {frame_index} renders to different bytes in a second renderer"
        );
    }
    assert_ne!(
        first_renderer_frames[0], first_renderer_frames[1],
        "the potato moves between consecutive frames"
    );
}
