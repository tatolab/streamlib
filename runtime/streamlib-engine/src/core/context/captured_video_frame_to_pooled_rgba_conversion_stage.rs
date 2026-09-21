// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The GPU stage every video capture arm lands its frames through: the
//! device's pixels converted into the stage's own scratch texture, copied into
//! a pooled `Rgba32` pixel buffer, and waited on host-side so the pixel buffer
//! is readable before the arm hands the frame off.

use std::sync::Arc;

use super::{GpuContextFullAccess, GpuContextLimitedAccess};
use crate::core::color::{ResolvedColorInfo, TransferId};
use crate::core::rhi::{
    PixelBuffer, PixelFormat, PublishedPixelBufferFrameId, RhiColorConverter, SourceLayoutInfo,
    StorageBuffer, Texture, TextureDescriptor, TextureFormat, TextureUsages, VulkanLayout,
};
use crate::core::{Error, Result};
use crate::vulkan::rhi::{
    COLOR_CONVERTER_WORKGROUP_SIZE, HostVulkanTimelineSemaphore, ImageCopyRegion,
    RhiCommandRecorder, VulkanAccess, VulkanStage,
};

/// Bound on the wait for the previous frame's submission before its scratch
/// texture is reused. Already satisfied unless that frame's own wait timed
/// out; a stalled GPU degrades to dropped frames, never a hung capture thread.
const PREVIOUS_SUBMISSION_WAIT_TIMEOUT_NS: u64 = 2_000_000_000;

/// Bound on the host wait for this frame's own submit. The signal is certain
/// after a successful submit unless the device is lost.
const HOST_READBACK_WAIT_TIMEOUT_NS: u64 = 5_000_000_000;

/// A run of dropped frames is reported at its first and then at every this
/// many, so a device that stops delivering is said once, not every frame.
const DROPPED_FRAMES_BETWEEN_REPORTS: u64 = 300;

/// A captured frame's device pixels as the stage reads them: NV12 or YUYV
/// bytes in a storage buffer.
pub(crate) struct CapturedVideoFrameBytesInAStorageBuffer<'a> {
    /// The buffer holding the frame's bytes.
    pub(crate) storage_buffer: &'a StorageBuffer,
    /// Where the frame's planes sit in `storage_buffer`, and their strides.
    pub(crate) layout: SourceLayoutInfo,
    /// Whether a device other than the GPU wrote the buffer — a camera's
    /// imported DMA-BUF or IOSurface — which needs a read-availability barrier
    /// before the kernel reads it. A host-visible buffer the CPU filled needs
    /// none beyond the implicit submit-time one.
    pub(crate) written_by_another_device: bool,
}

/// One capture stream's conversion from device pixels into pooled `Rgba32`
/// pixel buffers.
///
/// Owned by the thread that captures, which is the only one that converts.
pub(crate) struct CapturedVideoFrameToPooledRgbaConversionStage {
    width: u32,
    height: u32,
    color_converter: RhiColorConverter,
    recorder: RhiCommandRecorder,
    conversion_timeline: Arc<HostVulkanTimelineSemaphore>,
    scratch_texture: Texture,
    next_timeline_signal_value: u64,
}

impl CapturedVideoFrameToPooledRgbaConversionStage {
    /// Create the stage for frames of `width` × `height` whose device pixels
    /// are in `source_pixel_format`.
    ///
    /// The converter is the stream's own: two cameras of one source format
    /// each dispatch from their own capture thread, and a shared converter's
    /// kernel stages bindings both would race.
    pub(crate) fn create(
        full: &GpuContextFullAccess,
        source_pixel_format: PixelFormat,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let color_converter =
            full.create_color_converter(source_pixel_format, PixelFormat::Rgba32)?;
        let recorder = full.create_command_recorder("camera_capture")?;
        let conversion_timeline = full.create_timeline_semaphore(0)?;
        // Never read outside this stage, so it is allocated local rather than
        // exportable — NVIDIA caps DMA-BUF-exportable allocations once a
        // swapchain exists.
        let scratch_texture = full.device().create_texture_local(
            &TextureDescriptor::new(width, height, TextureFormat::Rgba8Unorm)
                .with_usage(TextureUsages::STORAGE_BINDING | TextureUsages::COPY_SRC),
        )?;
        Ok(Self {
            width,
            height,
            color_converter,
            recorder,
            conversion_timeline,
            scratch_texture,
            next_timeline_signal_value: 1,
        })
    }

    /// Convert one frame into a freshly acquired pooled pixel buffer, and
    /// return once that buffer is host-readable.
    ///
    /// The device's pixels are read by the time this returns, so an arm may
    /// hand its device buffer back as soon as it does, success or failure.
    /// An `Err` is a dropped frame; the stage stays usable for the next one.
    pub(crate) fn convert_into_pooled_pixel_buffer(
        &mut self,
        gpu_context: &GpuContextLimitedAccess,
        device_bytes: CapturedVideoFrameBytesInAStorageBuffer<'_>,
        color: &ResolvedColorInfo,
    ) -> Result<(PublishedPixelBufferFrameId, PixelBuffer)> {
        let converted = self.record_submit_and_wait(gpu_context, device_bytes, color);
        if converted.is_err() {
            // A begun-but-unsubmitted recording would fail the next begin();
            // harmless when nothing is recording.
            self.recorder.abort_recording();
        }
        converted
    }

    /// Wait, bounded, for the stage's last submission to finish — already
    /// done unless that frame's own wait timed out — so whatever it read can
    /// be released or reused.
    pub(crate) fn wait_for_the_previous_submission(&self) -> Result<()> {
        // The previous submission signalled one below the next value, and the
        // counter advances only on a successful submit, so this names a signal
        // genuinely in flight — or none, before the first frame.
        let previous_submission = self.next_timeline_signal_value - 1;
        if previous_submission == 0 {
            return Ok(());
        }
        self.conversion_timeline
            .wait(previous_submission, PREVIOUS_SUBMISSION_WAIT_TIMEOUT_NS)
            .map_err(|e| Error::GpuError(format!("previous-submission wait: {e}")))
    }

    fn record_submit_and_wait(
        &mut self,
        gpu_context: &GpuContextLimitedAccess,
        device_bytes: CapturedVideoFrameBytesInAStorageBuffer<'_>,
        color: &ResolvedColorInfo,
    ) -> Result<(PublishedPixelBufferFrameId, PixelBuffer)> {
        self.wait_for_the_previous_submission()?;
        let scratch_texture = &self.scratch_texture;

        // The pooled buffer's id is the frame's surface id — the one key a
        // same-process consumer, the surface-share service and a CPU readback
        // all resolve the frame through.
        let (published_pixel_buffer_frame_id, pooled_pixel_buffer) = gpu_context
            .acquire_pixel_buffer(self.width, self.height, PixelFormat::Rgba32)
            .map_err(|e| Error::GpuError(format!("acquire pixel buffer: {e}")))?;

        // Display path consumes RGBA8_UNORM treated as sRGB-encoded by the
        // swapchain; #817 replaces this with the negotiated VkColorSpaceKHR.
        let output_transfer = TransferId::Srgb;
        let kernel = self
            .color_converter
            .prepare_buffer_to_image_storage(
                device_bytes.storage_buffer,
                device_bytes.layout,
                scratch_texture,
                color,
                output_transfer,
            )
            .map_err(|e| Error::GpuError(format!("color-converter prepare: {e}")))?;

        let recorder = &mut self.recorder;
        recorder
            .begin()
            .map_err(|e| Error::GpuError(format!("recorder begin: {e}")))?;
        recorder
            .record_image_barrier(
                scratch_texture,
                VulkanLayout::UNDEFINED,
                VulkanLayout::GENERAL,
                VulkanStage::NONE,
                VulkanStage::COMPUTE_SHADER,
                VulkanAccess::NONE,
                VulkanAccess::SHADER_WRITE,
            )
            .map_err(|e| Error::GpuError(format!("pre-compute image barrier: {e}")))?;
        if device_bytes.written_by_another_device {
            recorder
                .record_buffer_barrier(
                    device_bytes.storage_buffer,
                    VulkanStage::NONE,
                    VulkanStage::COMPUTE_SHADER,
                    VulkanAccess::NONE,
                    VulkanAccess::SHADER_READ,
                )
                .map_err(|e| Error::GpuError(format!("pre-compute buffer barrier: {e}")))?;
        }
        recorder
            .record_dispatch(
                &kernel,
                self.width.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                self.height.div_ceil(COLOR_CONVERTER_WORKGROUP_SIZE),
                1,
            )
            .map_err(|e| Error::GpuError(format!("record dispatch: {e}")))?;
        recorder
            .record_image_barrier(
                scratch_texture,
                VulkanLayout::GENERAL,
                VulkanLayout::TRANSFER_SRC_OPTIMAL,
                VulkanStage::COMPUTE_SHADER,
                VulkanStage::ALL_TRANSFER,
                VulkanAccess::SHADER_WRITE,
                VulkanAccess::TRANSFER_READ,
            )
            .map_err(|e| Error::GpuError(format!("post-compute image barrier: {e}")))?;
        recorder
            .record_copy_image_to_buffer(
                scratch_texture,
                VulkanLayout::TRANSFER_SRC_OPTIMAL,
                &pooled_pixel_buffer,
                ImageCopyRegion::tightly_packed(self.width, self.height),
            )
            .map_err(|e| Error::GpuError(format!("copy image to pixel buffer: {e}")))?;
        recorder
            .record_buffer_barrier(
                &pooled_pixel_buffer,
                VulkanStage::ALL_TRANSFER,
                VulkanStage::HOST,
                VulkanAccess::TRANSFER_WRITE,
                VulkanAccess::HOST_READ,
            )
            .map_err(|e| Error::GpuError(format!("pixel-buffer host-read barrier: {e}")))?;

        // A timeline value must never be signalled twice, and this one is in
        // flight the moment the submit lands — even if the wait below times
        // out — so the counter advances before the wait.
        let signalled_value = self.next_timeline_signal_value;
        recorder
            .submit_signaling_timeline(&self.conversion_timeline, signalled_value)
            .map_err(|e| Error::GpuError(format!("submit compute dispatch: {e}")))?;
        self.next_timeline_signal_value += 1;
        self.conversion_timeline
            .wait(signalled_value, HOST_READBACK_WAIT_TIMEOUT_NS)
            .map_err(|e| Error::GpuError(format!("host-readback timeline wait: {e}")))?;

        Ok((published_pixel_buffer_frame_id, pooled_pixel_buffer))
    }
}

/// A capture stream's count of delivered frames and of frames dropped in a
/// row, reporting drops at a bounded rate.
#[derive(Debug, Default)]
pub(crate) struct CapturedVideoFrameDeliveryTally {
    consecutive_dropped_frames: u64,
    delivered_frames: u64,
}

impl CapturedVideoFrameDeliveryTally {
    /// Count a dropped frame, reporting the first of a run and every
    /// [`DROPPED_FRAMES_BETWEEN_REPORTS`]th after it.
    pub(crate) fn record_a_dropped_frame(&mut self, camera_name: &str, why: &Error) {
        self.consecutive_dropped_frames += 1;
        if self.consecutive_dropped_frames == 1
            || self
                .consecutive_dropped_frames
                .is_multiple_of(DROPPED_FRAMES_BETWEEN_REPORTS)
        {
            tracing::warn!(
                camera = camera_name,
                consecutive_dropped = self.consecutive_dropped_frames,
                error = %why,
                "frame dropped"
            );
        }
    }

    /// Count a delivered frame, ending any run of drops, and answer how many
    /// the stream has delivered — 1 for its first.
    pub(crate) fn record_a_delivered_frame(&mut self) -> u64 {
        self.consecutive_dropped_frames = 0;
        self.delivered_frames += 1;
        self.delivered_frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivered_frames_are_counted_from_one() {
        let mut tally = CapturedVideoFrameDeliveryTally::default();
        assert_eq!(tally.record_a_delivered_frame(), 1);
        assert_eq!(tally.record_a_delivered_frame(), 2);
    }

    #[test]
    fn a_delivered_frame_ends_a_run_of_drops() {
        let mut tally = CapturedVideoFrameDeliveryTally::default();
        let why = Error::Runtime("a test drop".into());
        tally.record_a_dropped_frame("a camera", &why);
        tally.record_a_dropped_frame("a camera", &why);
        assert_eq!(tally.consecutive_dropped_frames, 2);
        tally.record_a_delivered_frame();
        assert_eq!(tally.consecutive_dropped_frames, 0);
    }
}
