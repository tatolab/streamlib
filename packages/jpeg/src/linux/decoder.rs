// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

// JPEG Decoder Processor
//
// Thin wrapper around vulkan_jpeg::SimpleJpegDecoder. The primitive owns
// its own texture ring + surface_id registration; this processor just
// translates wire types in / out and forwards bytes to decode().
//
// Construction runs once at setup() — the runtime already calls setup
// inside the processor-setup mutex (privileged), so the caller-side
// `ctx.gpu_full_access()` is the privileged handle. Per-frame `decode()`
// is Limited-safe; no escalation on the hot path.

use crate::_generated_::{EncodedJpegFrame, VideoFrame};
use crate::linux::color_resolved_to_core::resolved_color_info_to_core;
use streamlib::sdk::context::{RuntimeContextFullAccess, RuntimeContextLimitedAccess};
use streamlib::sdk::error::{Error, Result};

use vulkan_jpeg::SimpleJpegDecoder;

/// Default max width when `JpegDecoderConfig::max_width` is unset. 4K
/// covers AGP drone-racing (1280×720 / 1920×1080 typical) and most
/// general-purpose use; lower or raise via config to trade GPU memory
/// for tighter / wider headroom.
const DEFAULT_MAX_WIDTH: u32 = 3840;
/// Default max height when `JpegDecoderConfig::max_height` is unset.
const DEFAULT_MAX_HEIGHT: u32 = 2160;

#[streamlib::sdk::processor("JpegDecoder")]
pub struct JpegDecoderProcessor {
    /// Underlying GPU JPEG decoder primitive. Owns the texture ring +
    /// per-slot surface_id registration internally.
    decoder: Option<SimpleJpegDecoder>,

    /// Frames decoded counter — drives periodic progress logs.
    frames_decoded: u64,
}

impl streamlib::sdk::processors::ReactiveProcessor for JpegDecoderProcessor::Processor {
    fn setup(&mut self, ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        let max_width = self.config.max_width.unwrap_or(DEFAULT_MAX_WIDTH);
        let max_height = self.config.max_height.unwrap_or(DEFAULT_MAX_HEIGHT);

        // setup() runs inside the engine's privileged lifecycle dispatch
        // (`ProcessorInstance::setup`), so `ctx.gpu_full_access()` is
        // already privileged in both cdylib and in-process modes (cdylib
        // bodies see a ScopeToken-shaped FullAccess routed through the
        // FullAccess vtable; in-process bodies see the Boxed FullAccess
        // dispatched directly). Calling `gpu_limited_access().escalate(...)`
        // here would re-enter the escalate gate on the same thread and
        // trip the gate's same-thread re-entry panic (see
        // `EscalateGate`'s type doc — the historical sandbox contract
        // forbids escalate-from-setup).
        let decoder = SimpleJpegDecoder::new(ctx.gpu_full_access(), max_width, max_height)?;

        tracing::info!(
            backend = decoder.backend_kind().as_str(),
            max_width = max_width,
            max_height = max_height,
            "[JpegDecoder] Initialized (GPU SimpleJpegDecoder)"
        );

        self.decoder = Some(decoder);
        Ok(())
    }

    fn teardown(&mut self, _ctx: &RuntimeContextFullAccess<'_>) -> Result<()> {
        tracing::info!(
            frames_decoded = self.frames_decoded,
            "[JpegDecoder] Shutting down"
        );
        self.decoder.take();
        Ok(())
    }

    fn process(&mut self, _ctx: &RuntimeContextLimitedAccess<'_>) -> Result<()> {
        if !self.inputs.has_data("encoded_jpeg_in") {
            return Ok(());
        }
        let encoded: EncodedJpegFrame = self.inputs.read("encoded_jpeg_in")?;

        let decoder = self
            .decoder
            .as_mut()
            .ok_or_else(|| Error::Runtime("JPEG decoder not initialized".into()))?;

        // Surface decode failures as a typed Runtime error. The runtime
        // logs WARN and keeps the processor alive for the next frame
        // (thread_runner.rs reactive drain loop).
        let output = decoder.decode(&encoded.data).map_err(wrap_decode_error)?;

        let log_first = self.frames_decoded == 0;
        let color_source = output.color_source;
        let color_info = resolved_color_info_to_core(&output.color_info);

        let video_frame = VideoFrame {
            surface_id: output.surface_id,
            width: output.width,
            height: output.height,
            timestamp_ns: encoded.timestamp_ns.clone(),
            fps: encoded.fps,
            // Per-frame override is opt-in; per-surface
            // `current_image_layout` from surface-share is the default.
            // SimpleJpegDecoder leaves slots in SHADER_READ_ONLY_OPTIMAL
            // / GENERAL and refreshes the registration — downstream
            // consumers resolve via the registration's current_layout.
            texture_layout: None,
            color_info: Some(color_info),
            mastering_display: None,
            content_light: None,
        };

        self.outputs.write("video_out", &video_frame)?;
        self.frames_decoded += 1;

        if log_first {
            tracing::info!(
                width = video_frame.width,
                height = video_frame.height,
                color_source = ?color_source,
                "[JpegDecoder] First frame decoded"
            );
        } else if self.frames_decoded % 300 == 0 {
            tracing::info!(
                frames = self.frames_decoded,
                "[JpegDecoder] Decode progress"
            );
        }

        Ok(())
    }
}

/// Wrap a SimpleJpegDecoder error into the typed `Error::Runtime`
/// variant the processor surfaces from `process()`. Pulled out as a
/// free function so the variant + format-string contract is unit-
/// testable without standing up a real GPU runtime.
fn wrap_decode_error(inner: Error) -> Error {
    Error::Runtime(format!("JPEG decode failed: {inner}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_max_dimensions_are_non_zero() {
        // SimpleJpegDecoder::new hard-rejects max_width=0 || max_height=0
        // (no "size from first frame" idiom). The defaults the wrapper
        // applies when `JpegDecoderConfig::{max_width, max_height}` are
        // unset must produce a valid construction — otherwise an empty
        // config (`{}`) would silently fail at setup.
        assert!(DEFAULT_MAX_WIDTH > 0, "DEFAULT_MAX_WIDTH must be non-zero");
        assert!(DEFAULT_MAX_HEIGHT > 0, "DEFAULT_MAX_HEIGHT must be non-zero");
    }

    #[test]
    fn wrap_decode_error_produces_runtime_variant() {
        // Any inner decoder error must come back out as
        // `Error::Runtime(_)` — that's the variant the issue's
        // error-path exit criterion calls for, and the only variant
        // downstream pattern-matchers can rely on.
        let inner = Error::GpuError("jpeg parse/huffman: missing SOI marker".into());
        let mapped = wrap_decode_error(inner);
        match mapped {
            Error::Runtime(msg) => {
                assert!(
                    msg.contains("JPEG decode failed"),
                    "expected wrap prefix, got: {msg}"
                );
                assert!(
                    msg.contains("missing SOI marker"),
                    "expected inner error message preserved in wrap, got: {msg}"
                );
            }
            other => panic!("expected Error::Runtime, got {other:?}"),
        }
    }
}
