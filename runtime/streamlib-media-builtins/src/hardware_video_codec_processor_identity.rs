// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What distinguishes one hardware video codec built-in from another.
//!
//! The H.264 and H.265 pairs share every seam — the same codec seam,
//! the same media frame, the same lazy mint and the same loss doctrine —
//! so they share one encoder body and one decoder body, and differ only in
//! the two facts below. The bodies are generic over this trait rather than
//! carrying a codec field, so a built-in cannot reach a running state with
//! its codec unset.

use streamlib::sdk::context::VideoCodecElementaryStream;

/// The codec facts one built-in processor is spelled with.
pub trait HardwareVideoCodecProcessorIdentity {
    /// The elementary stream the session is opened for.
    const VIDEO_CODEC_ELEMENTARY_STREAM: VideoCodecElementaryStream;

    /// The built-in's name, so a log line says which processor spoke rather
    /// than naming the shared body neither of them is registered as.
    const PROCESSOR_NAME: &'static str;
}
