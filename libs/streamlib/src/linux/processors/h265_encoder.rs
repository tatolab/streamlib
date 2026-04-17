// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// H.265 Encoder Processor
//
// Thin wrapper around vulkan_video::SimpleEncoder using the shared RHI
// VulkanDevice. The encoder shares streamlib's Vulkan device, VMA allocator,
// and queues — no separate device creation, no NVIDIA dual-device crash.
//
// The camera's GPU-resident textures are on the same device, so encode_image()
// accepts them directly (zero-copy).

use crate::_generated_::{Encodedvideoframe, Videoframe};
use crate::core::context::GpuContext;
use crate::core::{Result, RuntimeContext, StreamError};

use vulkan_video::{Codec, Preset, SimpleEncoder, SimpleEncoderConfig};

// ============================================================================
// PROCESSOR
// ============================================================================

#[crate::processor("com.streamlib.h265_encoder")]
pub struct H265EncoderProcessor {
    /// Vulkan Video hardware encoder (shares RHI device).
    encoder: Option<SimpleEncoder>,

    /// GPU context for resolving Videoframe textures.
    gpu_context: Option<GpuContext>,

    /// Frames encoded counter.
    frames_encoded: u64,
}

impl crate::core::ReactiveProcessor for H265EncoderProcessor::Processor {
    async fn setup(&mut self, ctx: RuntimeContext) -> Result<()> {
        self.gpu_context = Some(ctx.gpu.clone());

        let width = self.config.width.unwrap_or(1920);
        let height = self.config.height.unwrap_or(1080);

        let encoder_config = SimpleEncoderConfig {
            width,
            height,
            fps: 60,
            codec: Codec::H265,
            preset: Preset::Medium,
            streaming: true,
            idr_interval_secs: self.config.keyframe_interval_seconds.unwrap_or(2.0) as u32,
            bitrate_bps: self.config.bitrate_bps,
            prepend_header_to_idr: Some(true),
            ..Default::default()
        };

        let vulkan_device = &ctx.gpu.device().inner;

        let encode_queue = vulkan_device.video_encode_queue().ok_or_else(|| {
            StreamError::Runtime("GPU does not support Vulkan Video encode".into())
        })?;
        let encode_queue_family = vulkan_device.video_encode_queue_family_index().ok_or_else(|| {
            StreamError::Runtime("No video encode queue family".into())
        })?;

        let encoder = SimpleEncoder::from_device(
            encoder_config,
            vulkan_device.instance().clone(),
            vulkan_device.device().clone(),
            vulkan_device.physical_device(),
            vulkan_device.allocator().clone(),
            encode_queue,
            encode_queue_family,
            vulkan_device.queue(),
            vulkan_device.queue_family_index(),
            vulkan_device.queue(),
            vulkan_device.queue_family_index(),
        ).map_err(|e| {
            StreamError::Runtime(format!("Failed to create H.265 encoder: {e}"))
        })?;

        tracing::info!(
            "[H265Encoder] Initialized ({}x{}, shared RHI device, Vulkan Video hardware)",
            width, height
        );

        self.encoder = Some(encoder);
        Ok(())
    }

    async fn teardown(&mut self) -> Result<()> {
        tracing::info!(
            frames_encoded = self.frames_encoded,
            "[H265Encoder] Shutting down"
        );
        self.encoder.take();
        self.gpu_context.take();
        Ok(())
    }

    fn process(&mut self) -> Result<()> {
        if !self.inputs.has_data("video_in") {
            return Ok(());
        }
        let frame: Videoframe = self.inputs.read("video_in")?;

        let gpu_ctx = self
            .gpu_context
            .as_ref()
            .ok_or_else(|| StreamError::Runtime("GPU context not initialized".into()))?;

        let encoder = self
            .encoder
            .as_mut()
            .ok_or_else(|| StreamError::Runtime("H.265 encoder not initialized".into()))?;

        let texture = gpu_ctx.resolve_videoframe_texture(&frame)?;
        let image_view = texture.inner.image_view().map_err(|e| {
            StreamError::GpuError(format!("Failed to get image view: {e}"))
        })?;

        let timestamp_ns: Option<i64> = frame.timestamp_ns.parse().ok();

        let packets = encoder.encode_image(image_view, timestamp_ns).map_err(|e| {
            StreamError::Runtime(format!("H.265 encode failed: {e}"))
        })?;

        for packet in packets {
            let encoded = Encodedvideoframe {
                data: packet.data,
                is_keyframe: packet.is_keyframe,
                timestamp_ns: packet.timestamp_ns.unwrap_or(0).to_string(),
                frame_number: self.frames_encoded.to_string(),
            };
            self.outputs.write("encoded_video_out", &encoded)?;
        }

        self.frames_encoded += 1;
        if self.frames_encoded == 1 {
            tracing::info!("[H265Encoder] First frame encoded");
        } else if self.frames_encoded % 300 == 0 {
            tracing::info!(frames = self.frames_encoded, "[H265Encoder] Encode progress");
        }

        Ok(())
    }
}
