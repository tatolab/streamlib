// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! What this wheel's Rust refuses with, and how a refusal reaches Python.

use pyo3::PyErr;
use pyo3::exceptions::{PyRuntimeError, PyValueError};

/// Every way the MoQ publish and subscribe paths refuse.
#[derive(Debug, thiserror::Error)]
pub(crate) enum MoqExtensionError {
    /// A relay URL, broadcast path or track name this wheel cannot act on.
    #[error("{what}")]
    Refused { what: String },

    /// A media operation on a session that has not connected.
    #[error("the MoQ {role} session is not connected")]
    NotConnected { role: &'static str },

    /// The QUIC connection, the MoQ session, or a track read or write failed.
    #[error("{what}")]
    Transport { what: String },

    /// The relay closed the broadcast, or the track this subscription was
    /// reading ended. Distinct from [`MoqExtensionError::Transport`] because a
    /// subscriber reconnects on it rather than treating it as a fault.
    #[error("the MoQ broadcast ended: {what}")]
    BroadcastEnded { what: String },

    /// An object whose bytes are not the container the track declared.
    #[error("malformed {container} object: {what}")]
    MalformedObject {
        container: &'static str,
        what: String,
    },

    /// A bitstream the parameter-set reader cannot describe a track from.
    #[error("malformed H.264/H.265 bitstream: {what}")]
    MalformedBitstream { what: String },
}

pub(crate) type Result<T> = std::result::Result<T, MoqExtensionError>;

impl From<MoqExtensionError> for PyErr {
    fn from(refusal: MoqExtensionError) -> Self {
        match refusal {
            // A caller passed something wrong; everything else is the far end,
            // the network, or the stream, which is a runtime condition and not
            // a bad call.
            MoqExtensionError::Refused { .. } => PyValueError::new_err(refusal.to_string()),
            _ => PyRuntimeError::new_err(refusal.to_string()),
        }
    }
}
