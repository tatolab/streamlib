// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

#![cfg(target_os = "linux")]

//! What distinguishes one hardware video codec built-in from another.
//!
//! The H.264 and H.265 pairs share every seam — the same session surface,
//! the same bag convention, the same lazy mint and the same loss doctrine —
//! so they share one encoder body and one decoder body, and differ only in
//! the three facts below. The bodies are generic over this trait rather than
//! carrying a codec field, so a built-in cannot reach a running state with
//! its codec unset.

use streamlib::sdk::engine::video::Codec;

use crate::encoded_video_frame::EncodedVideoCodec;

/// The codec facts one built-in processor is spelled with.
pub trait HardwareVideoCodecProcessorIdentity {
    /// The `codec` an encoder writes on every bag and a decoder refuses any
    /// other spelling of.
    const ENCODED_VIDEO_CODEC: EncodedVideoCodec;

    /// The engine session surface's own spelling of the same elementary
    /// stream, which is what mints the Vulkan Video session.
    const VIDEO_SESSION_CODEC: Codec;

    /// The built-in's name, so a log line says which processor spoke rather
    /// than naming the shared body neither of them is registered as.
    const PROCESSOR_NAME: &'static str;
}
