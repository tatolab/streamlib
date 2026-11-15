// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use crate::{
    DEFAULT_SHADER_ENTRY_POINT, ShaderCompilationBackend, ShaderCompilationTargetStage,
    SlangCompilerGlobalSession, SlangPreprocessorMacro, SlangShaderCompilerRefusal,
    SlangShaderSourceToCompile,
};

/// Compile the Slang file at `source_path` and write its code to
/// `compiled_code_path`, as a build script stages a shader for `include_bytes!`.
pub fn compile_a_slang_shader_file(
    slang_compiler: &SlangCompilerGlobalSession,
    source_path: &Path,
    stage: ShaderCompilationTargetStage,
    module_search_directories: &[PathBuf],
    preprocessor_macros: &[SlangPreprocessorMacro<'_>],
    compiled_code_path: &Path,
) -> Result<(), SlangShaderCompilerRefusal> {
    let source = std::fs::read_to_string(source_path).map_err(|e| {
        SlangShaderCompilerRefusal(format!("reading {}: {e}", source_path.display()))
    })?;
    let label = source_path.to_string_lossy();
    let compiled = slang_compiler.compile(&SlangShaderSourceToCompile {
        source: &source,
        stage,
        backend: ShaderCompilationBackend::SpirV1_4ForVulkan1_2,
        entry_point: DEFAULT_SHADER_ENTRY_POINT,
        label: &label,
        module_search_directories,
        preprocessor_macros,
    })?;
    std::fs::write(compiled_code_path, compiled).map_err(|e| {
        SlangShaderCompilerRefusal(format!("writing {}: {e}", compiled_code_path.display()))
    })
}
