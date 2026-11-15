// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::CStr;
use std::path::PathBuf;

use crate::SlangShaderCompilerRefusal;
use crate::slang_compiler_abi;

/// What an unspecified entry point means, on either shader path.
pub const DEFAULT_SHADER_ENTRY_POINT: &str = "main";

/// The pipeline stage a shader source compiles for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderCompilationTargetStage {
    Compute,
    Vertex,
    Fragment,
    RayGeneration,
    RayMiss,
    RayClosestHit,
    RayAnyHit,
    RayIntersection,
    RayCallable,
}

impl ShaderCompilationTargetStage {
    /// The stage's spelling on the escalate wire and in the cache key.
    #[must_use]
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Compute => "compute",
            Self::Vertex => "vertex",
            Self::Fragment => "fragment",
            Self::RayGeneration => "ray_generation",
            Self::RayMiss => "ray_miss",
            Self::RayClosestHit => "ray_closest_hit",
            Self::RayAnyHit => "ray_any_hit",
            Self::RayIntersection => "ray_intersection",
            Self::RayCallable => "ray_callable",
        }
    }

    /// Every stage, for error messages that have to list them.
    pub const ALL: &'static [Self] = &[
        Self::Compute,
        Self::Vertex,
        Self::Fragment,
        Self::RayGeneration,
        Self::RayMiss,
        Self::RayClosestHit,
        Self::RayAnyHit,
        Self::RayIntersection,
        Self::RayCallable,
    ];

    /// Parse a wire spelling, naming every accepted one on failure.
    pub fn from_wire_name(wire_name: &str) -> Result<Self, SlangShaderCompilerRefusal> {
        Self::ALL
            .iter()
            .copied()
            .find(|stage| stage.wire_name() == wire_name)
            .ok_or_else(|| {
                let accepted = Self::ALL
                    .iter()
                    .map(|stage| format!("`{}`", stage.wire_name()))
                    .collect::<Vec<_>>()
                    .join(", ");
                SlangShaderCompilerRefusal(format!(
                    "`{wire_name}` is not a shader stage; the stages a kernel source \
                     compiles for are {accepted}"
                ))
            })
    }

    pub(crate) const fn slang_stage(self) -> slang_compiler_abi::SlangStage {
        use slang_compiler_abi::SlangStage;
        match self {
            Self::Compute => SlangStage::COMPUTE,
            Self::Vertex => SlangStage::VERTEX,
            Self::Fragment => SlangStage::FRAGMENT,
            Self::RayGeneration => SlangStage::RAY_GENERATION,
            Self::RayMiss => SlangStage::MISS,
            Self::RayClosestHit => SlangStage::CLOSEST_HIT,
            Self::RayAnyHit => SlangStage::ANY_HIT,
            Self::RayIntersection => SlangStage::INTERSECTION,
            Self::RayCallable => SlangStage::CALLABLE,
        }
    }
}

/// The code a shader source compiles to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderCompilationBackend {
    /// SPIR-V 1.4 for Vulkan 1.2: the floor `SPV_KHR_ray_tracing` needs, so it
    /// covers every stage the engine builds a pipeline for.
    SpirV1_4ForVulkan1_2,
}

impl ShaderCompilationBackend {
    pub(crate) const fn slang_profile_name(self) -> &'static CStr {
        match self {
            Self::SpirV1_4ForVulkan1_2 => c"spirv_1_4",
        }
    }

    pub(crate) const fn slang_compile_target(self) -> i32 {
        match self {
            Self::SpirV1_4ForVulkan1_2 => slang_compiler_abi::SLANG_SPIRV,
        }
    }
}

/// A `#define` the source is compiled with, as `-D<name>=<value>` would give it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlangPreprocessorMacro<'request> {
    /// The macro's name.
    pub name: &'request str,
    /// The text it expands to.
    pub value: &'request str,
}

/// One Slang source and what to compile it for.
#[derive(Debug, Clone, Copy)]
pub struct SlangShaderSourceToCompile<'request> {
    /// The Slang text.
    pub source: &'request str,
    /// The stage its entry point is checked and compiled for.
    pub stage: ShaderCompilationTargetStage,
    /// The code it compiles to.
    pub backend: ShaderCompilationBackend,
    /// The entry point's name.
    pub entry_point: &'request str,
    /// The name its diagnostics give the source, as `<label>:<line>:<column>`.
    pub label: &'request str,
    /// Where an `import` or `#include` in the source is looked for.
    pub module_search_directories: &'request [PathBuf],
    /// The macros the source is preprocessed with.
    pub preprocessor_macros: &'request [SlangPreprocessorMacro<'request>],
}

impl<'request> SlangShaderSourceToCompile<'request> {
    /// `source` for `stage` to SPIR-V, with no search directory or macro.
    pub fn for_spirv(
        source: &'request str,
        stage: ShaderCompilationTargetStage,
        entry_point: &'request str,
        label: &'request str,
    ) -> Self {
        Self {
            source,
            stage,
            backend: ShaderCompilationBackend::SpirV1_4ForVulkan1_2,
            entry_point,
            label,
            module_search_directories: &[],
            preprocessor_macros: &[],
        }
    }
}
