// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;

/// Where one loaded stream's processor interpreters start: the stream's project
/// directory and that directory's venv interpreter.
///
/// Recorded beside a loaded graph and never inside it, so the same graph loads
/// from another checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEnvironment {
    /// The directory the stream's own modules import from, and the working
    /// directory of every processor interpreter it starts.
    pub project_directory: PathBuf,
    /// The interpreter a processor interpreter is an exec of — the project's
    /// venv `python`, by absolute path.
    pub interpreter: PathBuf,
}
