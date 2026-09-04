// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What this wheel's Rust refuses with, and how a refusal reaches Python.

use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::PyErr;

/// Every way the WHIP and WHEP paths refuse.
#[derive(Debug, thiserror::Error)]
pub enum WebRtcExtensionError {
    /// An RTP payload this depacketiser cannot read as H.264.
    #[error("malformed RTP payload: {what}")]
    MalformedRtpPayload { what: String },

    /// A bitstream the SPS parser cannot read.
    #[error("malformed H.264 bitstream: {what}")]
    MalformedBitstream { what: String },

    /// An Opus packet whose own TOC byte does not describe it.
    #[error("malformed Opus packet: {what}")]
    MalformedOpusPacket { what: String },

    /// WHIP or WHEP signalling that did not complete.
    #[error("{protocol} signalling failed: {what}")]
    Signalling { protocol: &'static str, what: String },

    /// A media operation on a session that has not connected.
    #[error("the {protocol} session is not connected")]
    NotConnected { protocol: &'static str },

    /// The peer connection, a track write, or a track read failed.
    #[error("{what}")]
    Transport { what: String },

    /// A caller-supplied value this wheel cannot act on.
    #[error("{what}")]
    Refused { what: String },
}

pub type Result<T> = std::result::Result<T, WebRtcExtensionError>;

impl From<WebRtcExtensionError> for PyErr {
    fn from(refusal: WebRtcExtensionError) -> Self {
        match refusal {
            // A caller passed something wrong; everything else is the far end
            // or the network, which is a runtime condition and not a bad call.
            WebRtcExtensionError::Refused { .. } => PyValueError::new_err(refusal.to_string()),
            _ => PyRuntimeError::new_err(refusal.to_string()),
        }
    }
}
