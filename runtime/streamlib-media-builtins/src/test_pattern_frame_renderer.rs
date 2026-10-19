// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The test pattern's frames, rendered on the GPU: SMPTE bars, the potato
//! sprite bouncing over them, and the sample stream's caption panel in the
//! bottom-right corner. A frame's picture is a function of its index alone.

use streamlib::sdk::context::ProcessorOutputPixelBufferPoolOfOneNativeNode;
use streamlib::sdk::context::{GpuContextFullAccess, OneOffAcquisitionCountedForItsNode};
use streamlib::sdk::engine::host_rhi::{
    RhiCommandRecorder, VulkanAccess, VulkanComputeKernel, VulkanStage,
};
use streamlib::sdk::error::{Error, Result};
use streamlib::sdk::rhi::{
    ComputeBindingSpec, ComputeKernelDescriptor, PixelBuffer, PixelFormat,
    PublishedPixelBufferFrameId, StorageBuffer, TensorStorageBufferLayout,
};

/// A picture baked into the binary as tightly packed RGBA rows.
struct BakedRgbaOverlay {
    width: u32,
    height: u32,
    rgba: &'static [u8],
}

impl BakedRgbaOverlay {
    const fn holds_exactly_its_extent(&self) -> bool {
        self.rgba.len() == (self.width * self.height * 4) as usize
    }
}

const POTATO_SPRITE: BakedRgbaOverlay = BakedRgbaOverlay {
    width: 160,
    height: 160,
    rgba: include_bytes!("test_pattern_assets/potato_sprite_160x160.rgba"),
};

/// "Tatolab stream setup successfully" over "sample stream", right-aligned,
/// set in Press Start 2P white on an opaque dark panel.
const SAMPLE_STREAM_CAPTION_PANEL: BakedRgbaOverlay = BakedRgbaOverlay {
    width: 548,
    height: 62,
    rgba: include_bytes!("test_pattern_assets/sample_stream_captions_548x62.rgba"),
};

const _: () = assert!(POTATO_SPRITE.holds_exactly_its_extent());
const _: () = assert!(SAMPLE_STREAM_CAPTION_PANEL.holds_exactly_its_extent());

/// How far the potato travels along each axis per frame.
const POTATO_SPRITE_PIXELS_PER_FRAME: u64 = 4;

/// The gap between the caption panel and the frame's right and bottom edges.
const CAPTION_PANEL_MARGIN_PIXELS: i32 = 24;

/// The shader's `local_size_x` and `local_size_y`.
const TEST_PATTERN_FRAME_WORKGROUP_SIZE: u32 = 16;

const TEST_PATTERN_FRAME_KERNEL_BINDINGS: &[ComputeBindingSpec] = &[
    ComputeBindingSpec::storage_buffer(0),
    ComputeBindingSpec::storage_buffer(1),
    ComputeBindingSpec::storage_buffer(2),
];

/// The shader's `TestPatternFramePushConstants` block, field for field.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TestPatternFramePushConstants {
    frame_width: u32,
    frame_height: u32,
    potato_sprite_left: i32,
    potato_sprite_top: i32,
    potato_sprite_width: u32,
    potato_sprite_height: u32,
    caption_panel_left: i32,
    caption_panel_top: i32,
    caption_panel_width: u32,
    caption_panel_height: u32,
}

const TEST_PATTERN_FRAME_PUSH_CONSTANT_SIZE: u32 =
    std::mem::size_of::<TestPatternFramePushConstants>() as u32;

const _: () = assert!(TEST_PATTERN_FRAME_PUSH_CONSTANT_SIZE == 40);

/// Where an overlay's top-left pixel lands in the frame; negative where the
/// frame is smaller than the overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayTopLeftInTheFrame {
    /// Columns from the frame's left edge.
    pub left: i32,
    /// Rows from the frame's top edge.
    pub top: i32,
}

/// The position along a span of `travel` pixels after `distance` pixels of
/// travel, bouncing between its two ends.
fn bounced_position_along(distance: u64, travel: u32) -> i32 {
    let travel = u64::from(travel);
    if travel == 0 {
        return 0;
    }
    let distance_into_the_round_trip = distance % (2 * travel);
    let position = if distance_into_the_round_trip <= travel {
        distance_into_the_round_trip
    } else {
        2 * travel - distance_into_the_round_trip
    };
    position as i32
}

/// Where the potato sits in frame `frame_index`: it leaves the top-left corner
/// at frame zero and bounces off every edge it reaches.
pub fn potato_sprite_top_left_at_frame(
    frame_index: u64,
    frame_width: u32,
    frame_height: u32,
) -> OverlayTopLeftInTheFrame {
    let distance = frame_index.wrapping_mul(POTATO_SPRITE_PIXELS_PER_FRAME);
    OverlayTopLeftInTheFrame {
        left: bounced_position_along(distance, frame_width.saturating_sub(POTATO_SPRITE.width)),
        top: bounced_position_along(distance, frame_height.saturating_sub(POTATO_SPRITE.height)),
    }
}

/// Where the caption panel sits: the bottom-right corner, inset by its margin.
pub fn caption_panel_top_left(frame_width: u32, frame_height: u32) -> OverlayTopLeftInTheFrame {
    OverlayTopLeftInTheFrame {
        left: frame_width as i32
            - SAMPLE_STREAM_CAPTION_PANEL.width as i32
            - CAPTION_PANEL_MARGIN_PIXELS,
        top: frame_height as i32
            - SAMPLE_STREAM_CAPTION_PANEL.height as i32
            - CAPTION_PANEL_MARGIN_PIXELS,
    }
}

/// The potato sprite's extent in pixels.
pub fn potato_sprite_extent() -> (u32, u32) {
    (POTATO_SPRITE.width, POTATO_SPRITE.height)
}

/// The caption panel's extent in pixels.
pub fn caption_panel_extent() -> (u32, u32) {
    (
        SAMPLE_STREAM_CAPTION_PANEL.width,
        SAMPLE_STREAM_CAPTION_PANEL.height,
    )
}

fn push_constants_for_frame(
    frame_index: u64,
    frame_width: u32,
    frame_height: u32,
) -> TestPatternFramePushConstants {
    let potato_sprite = potato_sprite_top_left_at_frame(frame_index, frame_width, frame_height);
    let caption_panel = caption_panel_top_left(frame_width, frame_height);
    TestPatternFramePushConstants {
        frame_width,
        frame_height,
        potato_sprite_left: potato_sprite.left,
        potato_sprite_top: potato_sprite.top,
        potato_sprite_width: POTATO_SPRITE.width,
        potato_sprite_height: POTATO_SPRITE.height,
        caption_panel_left: caption_panel.left,
        caption_panel_top: caption_panel.top,
        caption_panel_width: SAMPLE_STREAM_CAPTION_PANEL.width,
        caption_panel_height: SAMPLE_STREAM_CAPTION_PANEL.height,
    }
}

/// An overlay's pixels uploaded once into a storage buffer the kernel reads.
fn upload_overlay_into_a_storage_buffer(
    full_access: &GpuContextFullAccess,
    overlay: &BakedRgbaOverlay,
) -> Result<OneOffAcquisitionCountedForItsNode<StorageBuffer>> {
    let storage_buffer = full_access.acquire_storage_buffer(
        &TensorStorageBufferLayout::of_bytes(overlay.rgba.len() as u64)?,
    )?;
    let mapped = storage_buffer.mapped_ptr();
    if mapped.is_null() || storage_buffer.byte_size() < overlay.rgba.len() as u64 {
        return Err(Error::GpuError(format!(
            "test pattern: overlay storage buffer unusable (mapped: {}, {} bytes < {})",
            !mapped.is_null(),
            storage_buffer.byte_size(),
            overlay.rgba.len()
        )));
    }
    // SAFETY: `mapped` is the host-visible mapping of a buffer of at least
    // `overlay.rgba.len()` bytes, checked above, and nothing else writes it.
    unsafe {
        std::ptr::copy_nonoverlapping(overlay.rgba.as_ptr(), mapped, overlay.rgba.len());
    }
    Ok(storage_buffer)
}

/// Renders the test pattern's frames of one extent into a native node's own
/// output pool.
pub struct TestPatternFrameRenderer {
    frame_width: u32,
    frame_height: u32,
    kernel: VulkanComputeKernel,
    recorder: RhiCommandRecorder,
    processor_output_pixel_buffer_pool: ProcessorOutputPixelBufferPoolOfOneNativeNode,
    potato_sprite_pixels: OneOffAcquisitionCountedForItsNode<StorageBuffer>,
    caption_panel_pixels: OneOffAcquisitionCountedForItsNode<StorageBuffer>,
}

impl TestPatternFrameRenderer {
    /// A renderer of `frame_width` × `frame_height` frames, published from the
    /// output pool of the node `processor_id_of_the_owning_node`.
    pub fn create(
        full_access: &GpuContextFullAccess,
        processor_id_of_the_owning_node: &str,
        frame_width: u32,
        frame_height: u32,
    ) -> Result<Self> {
        if frame_width == 0 || frame_height == 0 {
            return Err(Error::Configuration(format!(
                "test pattern: a {frame_width}x{frame_height} frame has no pixels to render"
            )));
        }
        let kernel = full_access.create_compute_kernel(&ComputeKernelDescriptor {
            entry_point: "main",
            label: "test_pattern_frame",
            spv: include_bytes!(concat!(env!("OUT_DIR"), "/test_pattern_frame.spv")),
            bindings: TEST_PATTERN_FRAME_KERNEL_BINDINGS,
            push_constant_size: TEST_PATTERN_FRAME_PUSH_CONSTANT_SIZE,
        })?;
        let potato_sprite_pixels =
            upload_overlay_into_a_storage_buffer(full_access, &POTATO_SPRITE)?;
        let caption_panel_pixels =
            upload_overlay_into_a_storage_buffer(full_access, &SAMPLE_STREAM_CAPTION_PANEL)?;
        Ok(Self {
            frame_width,
            frame_height,
            kernel,
            recorder: full_access.create_command_recorder("test_pattern_frame")?,
            processor_output_pixel_buffer_pool: full_access
                .create_processor_output_pixel_buffer_pool_for_a_native_node(
                    processor_id_of_the_owning_node,
                ),
            potato_sprite_pixels,
            caption_panel_pixels,
        })
    }

    /// Render frame `frame_index` into the next `Rgba32` pixel buffer of the
    /// node's output pool, returning once the buffer is host-readable.
    ///
    /// An `Err` is a dropped frame — the pool's own refusal when downstream
    /// holds every frame it may — and the renderer stays usable.
    pub fn render_frame(
        &mut self,
        frame_index: u64,
    ) -> Result<(PublishedPixelBufferFrameId, PixelBuffer)> {
        let rendered = self.record_submit_and_wait(frame_index);
        if rendered.is_err() {
            self.recorder.abort_recording();
        }
        rendered
    }

    fn record_submit_and_wait(
        &mut self,
        frame_index: u64,
    ) -> Result<(PublishedPixelBufferFrameId, PixelBuffer)> {
        let (published_pixel_buffer_frame_id, pooled_pixel_buffer) = self
            .processor_output_pixel_buffer_pool
            .acquire_the_next_output_pixel_buffer(
                self.frame_width,
                self.frame_height,
                PixelFormat::Rgba32,
            )?;
        self.kernel
            .set_storage_buffer_pixel(0, &pooled_pixel_buffer)?;
        self.kernel
            .set_storage_buffer_storage(1, &self.potato_sprite_pixels)?;
        self.kernel
            .set_storage_buffer_storage(2, &self.caption_panel_pixels)?;
        self.kernel
            .set_push_constants_value(&push_constants_for_frame(
                frame_index,
                self.frame_width,
                self.frame_height,
            ))?;

        self.recorder.begin()?;
        self.recorder.record_dispatch(
            &self.kernel,
            self.frame_width.div_ceil(TEST_PATTERN_FRAME_WORKGROUP_SIZE),
            self.frame_height
                .div_ceil(TEST_PATTERN_FRAME_WORKGROUP_SIZE),
            1,
        )?;
        self.recorder.record_buffer_barrier(
            &pooled_pixel_buffer,
            VulkanStage::COMPUTE_SHADER,
            VulkanStage::HOST,
            VulkanAccess::SHADER_WRITE,
            VulkanAccess::HOST_READ,
        )?;
        self.recorder.submit_and_wait()?;
        Ok((published_pixel_buffer_frame_id, pooled_pixel_buffer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_baked_overlays_are_the_extents_their_names_say() {
        assert_eq!(potato_sprite_extent(), (160, 160));
        assert_eq!(caption_panel_extent(), (548, 62));
    }

    #[test]
    fn the_potato_leaves_the_top_left_corner_at_frame_zero() {
        assert_eq!(
            potato_sprite_top_left_at_frame(0, 1280, 720),
            OverlayTopLeftInTheFrame { left: 0, top: 0 }
        );
    }

    #[test]
    fn the_potato_bounces_off_each_edge_it_reaches() {
        // 1280 - 160 = 1120 px of horizontal travel at 4 px a frame is 280
        // frames; 720 - 160 = 560 px of vertical travel is 140.
        let at = |frame_index| potato_sprite_top_left_at_frame(frame_index, 1280, 720);
        assert_eq!(
            at(140),
            OverlayTopLeftInTheFrame {
                left: 560,
                top: 560
            }
        );
        assert_eq!(
            at(141),
            OverlayTopLeftInTheFrame {
                left: 564,
                top: 556
            }
        );
        assert_eq!(at(280), OverlayTopLeftInTheFrame { left: 1120, top: 0 });
        assert_eq!(at(281), OverlayTopLeftInTheFrame { left: 1116, top: 4 });
        assert_eq!(at(560), OverlayTopLeftInTheFrame { left: 0, top: 0 });
    }

    #[test]
    fn the_potato_never_leaves_the_frame() {
        for frame_index in 0..2000 {
            let at = potato_sprite_top_left_at_frame(frame_index, 1280, 720);
            assert!((0..=1120).contains(&at.left), "frame {frame_index}: {at:?}");
            assert!((0..=560).contains(&at.top), "frame {frame_index}: {at:?}");
        }
    }

    #[test]
    fn a_frame_no_larger_than_the_potato_holds_it_still_at_the_origin() {
        for frame_index in [0, 1, 99, 12_345] {
            assert_eq!(
                potato_sprite_top_left_at_frame(frame_index, 64, 64),
                OverlayTopLeftInTheFrame { left: 0, top: 0 }
            );
        }
    }

    #[test]
    fn the_caption_panel_sits_inset_from_the_bottom_right_corner() {
        assert_eq!(
            caption_panel_top_left(1280, 720),
            OverlayTopLeftInTheFrame {
                left: 1280 - 548 - 24,
                top: 720 - 62 - 24,
            }
        );
    }

    #[test]
    fn a_frame_index_past_any_run_length_still_places_the_potato() {
        let at = potato_sprite_top_left_at_frame(u64::MAX, 1280, 720);
        assert!((0..=1120).contains(&at.left) && (0..=560).contains(&at.top));
    }
}
