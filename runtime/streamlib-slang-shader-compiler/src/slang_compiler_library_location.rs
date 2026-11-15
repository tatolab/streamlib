// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use crate::{SlangCompilerGlobalSession, SlangShaderCompilerRefusal};

/// The Slang release the runtime unit carries and the workspace stages; Slang
/// loads its optimiser by a file name that embeds it.
pub const SLANG_COMPILER_RELEASE: &str = "2026.12";

#[cfg(target_os = "macos")]
mod slang_compiler_file_names_on_this_platform {
    /// The Slang compiler library opened by path.
    pub const SLANG_COMPILER_LIBRARY_FILE_NAME: &str = "libslang-compiler.dylib";

    /// The SPIR-V optimiser the Slang compiler loads from its own directory.
    pub const SLANG_SPIRV_OPTIMISER_LIBRARY_FILE_NAME: &str = "libslang-glslang-2026.12.dylib";
}

#[cfg(not(target_os = "macos"))]
mod slang_compiler_file_names_on_this_platform {
    /// The Slang compiler library opened by path.
    pub const SLANG_COMPILER_LIBRARY_FILE_NAME: &str = "libslang-compiler.so";

    /// The SPIR-V optimiser the Slang compiler loads from its own directory.
    pub const SLANG_SPIRV_OPTIMISER_LIBRARY_FILE_NAME: &str = "libslang-glslang-2026.12.so";
}

pub use slang_compiler_file_names_on_this_platform::{
    SLANG_COMPILER_LIBRARY_FILE_NAME, SLANG_SPIRV_OPTIMISER_LIBRARY_FILE_NAME,
};

/// Where `scripts/stage_slang_compiler_libraries.sh` stages the libraries in
/// a workspace, relative to its root.
pub const WORKSPACE_STAGED_SLANG_COMPILER_DIRECTORY: &str = "target/slang_compiler";

/// The Slang compiler library `scripts/stage_slang_compiler_libraries.sh`
/// stages in the workspace rooted at `workspace_root`.
pub fn workspace_staged_slang_compiler_library(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(WORKSPACE_STAGED_SLANG_COMPILER_DIRECTORY)
        .join(SLANG_COMPILER_LIBRARY_FILE_NAME)
}

/// Open the Slang compiler the workspace rooted at `workspace_root` stages.
pub fn open_the_workspace_staged_slang_compiler(
    workspace_root: &Path,
) -> Result<SlangCompilerGlobalSession, SlangShaderCompilerRefusal> {
    SlangCompilerGlobalSession::open_the_first_library_of(&[
        workspace_staged_slang_compiler_library(workspace_root),
    ])
}
