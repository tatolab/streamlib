// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEnvironment {
    pub project_directory: PathBuf,
    pub interpreter: PathBuf,
}
