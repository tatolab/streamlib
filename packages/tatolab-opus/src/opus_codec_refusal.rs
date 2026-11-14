// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! What this wheel's codec state refuses with, and what it has to say.
//!
//! A helper process has no `tracing` subscriber, so nothing here logs: a
//! refusal reaches Python as a `ValueError`, and a notice is handed back for
//! the node to log through `tatolab.stream.log`.

use pyo3::PyErr;
use pyo3::exceptions::PyValueError;

/// A window, packet or setting the Opus codec state refuses, by name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct OpusCodecRefusal(pub String);

/// The result of a codec operation, refused with an [`OpusCodecRefusal`].
pub type Result<T> = std::result::Result<T, OpusCodecRefusal>;

impl From<OpusCodecRefusal> for PyErr {
    fn from(refusal: OpusCodecRefusal) -> Self {
        PyValueError::new_err(refusal.0)
    }
}

/// How loudly a node logs an [`OpusCodecNotice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpusCodecNoticeLevel {
    Info,
    Warn,
}

impl OpusCodecNoticeLevel {
    /// The level's name as `tatolab.stream.log` spells its function.
    pub fn as_log_function_name(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
        }
    }
}

/// One thing the codec state did that an operator reads in the log: a mint,
/// a re-mint, a re-entry after loss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpusCodecNotice {
    pub level: OpusCodecNoticeLevel,
    pub message: String,
}

impl OpusCodecNotice {
    /// A notice logged at the info level.
    pub fn info(message: String) -> Self {
        Self {
            level: OpusCodecNoticeLevel::Info,
            message,
        }
    }

    /// A notice logged at the warn level.
    pub fn warn(message: String) -> Self {
        Self {
            level: OpusCodecNoticeLevel::Warn,
            message,
        }
    }
}
