// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! What this wheel's Rust refuses with, and how a refusal reaches Python.

use pyo3::PyErr;
use pyo3::exceptions::{PyRuntimeError, PyValueError};

/// Every way the MoQ nodes refuse.
#[derive(Debug, thiserror::Error)]
pub enum MoqModuleError {
    /// A config or settings value the node cannot act on.
    #[error("{what}")]
    Refused { what: String },

    /// The shared memory the nodes reach their gateway through.
    #[error("{what}")]
    Ipc { what: String },

    /// The relay session, a broadcast or a track.
    #[error("{what}")]
    Relay { what: String },

    /// The port or log the runtime lent this node.
    #[error("{what}")]
    ModuleHost { what: String },
}

/// This wheel's results.
pub type Result<T> = std::result::Result<T, MoqModuleError>;

impl From<tatolab_module_sdk::ModuleHostError> for MoqModuleError {
    fn from(refusal: tatolab_module_sdk::ModuleHostError) -> Self {
        Self::ModuleHost {
            what: refusal.to_string(),
        }
    }
}

impl From<MoqModuleError> for PyErr {
    fn from(refusal: MoqModuleError) -> Self {
        match refusal {
            MoqModuleError::Refused { .. } => PyValueError::new_err(refusal.to_string()),
            _ => PyRuntimeError::new_err(refusal.to_string()),
        }
    }
}
