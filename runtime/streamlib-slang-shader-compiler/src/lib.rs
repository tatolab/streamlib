// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The Slang shader compiler, opened by path: one global session compiles
//! Slang source text for one pipeline stage.
//!
//! The engine compiles its kernels' sources through it at run time, and the
//! build scripts compile the engine's own shaders through it for
//! `include_bytes!`, with the same settings.

mod slang_com_object;
mod slang_compiler_abi;
mod slang_compiler_global_session;
mod slang_compiler_library_location;
mod slang_shader_compile_request;
mod slang_shader_compiler_refusal;
mod slang_shader_file_compilation;

#[cfg(test)]
mod tests;

pub use slang_compiler_global_session::SlangCompilerGlobalSession;
pub use slang_compiler_library_location::{
    SLANG_COMPILER_LIBRARY_FILE_NAME, SLANG_COMPILER_RELEASE,
    SLANG_SPIRV_OPTIMISER_LIBRARY_FILE_NAME, WORKSPACE_STAGED_SLANG_COMPILER_DIRECTORY,
    open_the_workspace_staged_slang_compiler, workspace_staged_slang_compiler_library,
};
pub use slang_shader_compile_request::{
    DEFAULT_SHADER_ENTRY_POINT, ShaderCompilationBackend, ShaderCompilationTargetStage,
    SlangPreprocessorMacro, SlangShaderSourceToCompile,
};
pub use slang_shader_compiler_refusal::SlangShaderCompilerRefusal;
pub use slang_shader_file_compilation::compile_a_slang_shader_file;
