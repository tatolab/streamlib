// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's GLSL source compiler and the cache in front of it.
//!
//! GLSL text is the kernel source contract, so the compiler is linked into the
//! engine rather than expected on the machine: a kernel author needs no shader
//! toolchain beyond the installed wheel, for every kernel kind.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use sha2::{Digest, Sha256};

use crate::core::{Error, Result};

/// The vendored compiler, as it appears in the compilation cache key.
///
/// Hand-written because shaderc reports no version at runtime.
/// `the_recorded_compiler_version_matches_the_pinned_dependency` reads
/// `Cargo.lock`, so the constant cannot drift away from what is linked in.
pub const VENDORED_GLSL_COMPILER_VERSION: &str = "shaderc 0.10.1";

/// GLSL's entry point is `main`, and glslang will not rename one.
///
/// Handing shaderc another name is silently ignored — it emits a module whose
/// `OpEntryPoint` still says `main` — so the alternative to refusing is a
/// pipeline built against a function that is not in the module.
pub const GLSL_SOURCE_ENTRY_POINT: &str = "main";

/// The pipeline stage a GLSL source compiles for.
///
/// Every ray-tracing stage is here deliberately: ray-tracing kernels are a
/// decided capability, and a GLSL contract covering compute and graphics but
/// not raygen / miss / hit / intersection would exclude one kernel kind that
/// an author would discover only on writing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderPipelineStage {
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

impl ShaderPipelineStage {
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

    /// Every stage's wire spelling, for error messages that have to list them.
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
    pub fn from_wire_name(wire_name: &str) -> Result<Self> {
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
                Error::GpuError(format!(
                    "`{wire_name}` is not a shader stage; the stages a kernel source \
                     compiles for are {accepted}"
                ))
            })
    }

    const fn shaderc_kind(self) -> shaderc::ShaderKind {
        match self {
            Self::Compute => shaderc::ShaderKind::Compute,
            Self::Vertex => shaderc::ShaderKind::Vertex,
            Self::Fragment => shaderc::ShaderKind::Fragment,
            Self::RayGeneration => shaderc::ShaderKind::RayGeneration,
            Self::RayMiss => shaderc::ShaderKind::Miss,
            Self::RayClosestHit => shaderc::ShaderKind::ClosestHit,
            Self::RayAnyHit => shaderc::ShaderKind::AnyHit,
            Self::RayIntersection => shaderc::ShaderKind::Intersection,
            Self::RayCallable => shaderc::ShaderKind::Callable,
        }
    }
}

/// The Vulkan client version and SPIR-V version the engine compiles against.
///
/// One engine, one target — this is not a caller-facing dial. It is in the
/// cache key regardless, because moving the pin changes the emitted words and
/// a key without it would hand back SPIR-V built for the old target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShaderTargetEnvironment {
    vulkan_env_version: shaderc::EnvVersion,
    spirv_version: shaderc::SpirvVersion,
}

impl ShaderTargetEnvironment {
    /// The pair `build.rs` compiles the engine's own shaders with
    /// (`--target-env=vulkan1.2 --target-spv=spv1.4`). Runtime-compiled kernels
    /// share the device with those, so they share the target.
    pub const ENGINE: Self = Self {
        vulkan_env_version: shaderc::EnvVersion::Vulkan1_2,
        spirv_version: shaderc::SpirvVersion::V1_4,
    };

    fn key_spelling(self) -> String {
        format!(
            "vulkan-env {} / spirv {}",
            self.vulkan_env_version as u32, self.spirv_version as u32
        )
    }
}

/// Everything about a compilation that changes the SPIR-V it emits.
///
/// Never the source alone: the same text compiled for another stage, against
/// another target environment, or by another compiler build is a different
/// module, and a key that dropped any of those would serve one for the other.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GlslShaderCompilationCacheKey {
    source_sha256: String,
    stage: ShaderPipelineStage,
    entry_point: String,
    target_environment: String,
    compiler_version: &'static str,
}

impl GlslShaderCompilationCacheKey {
    #[must_use]
    pub fn new(
        source: &str,
        stage: ShaderPipelineStage,
        entry_point: &str,
        target_environment: ShaderTargetEnvironment,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(source.as_bytes());
        Self {
            source_sha256: format!("{:x}", hasher.finalize()),
            stage,
            entry_point: entry_point.to_string(),
            target_environment: target_environment.key_spelling(),
            compiler_version: VENDORED_GLSL_COMPILER_VERSION,
        }
    }
}

/// Compiles GLSL text to SPIR-V and remembers what it compiled.
///
/// Held for a `GpuContext`'s lifetime, so re-creating an identical kernel — the
/// same source for the same stage — costs no compilation. Entries are never
/// evicted: they are bounded by the distinct sources a graph's processors
/// author, and a kernel outlives the helper that registered it.
/// The compiler is built on first use rather than at construction so a
/// `GpuContext` that never compiles a kernel pays nothing for one, and so
/// context construction stays infallible.
pub struct GlslShaderSourceToSpirvCompiler {
    compiler: OnceLock<shaderc::Compiler>,
    compiled_spirv_by_key: Mutex<HashMap<GlslShaderCompilationCacheKey, Arc<Vec<u8>>>>,
    invocation_count: AtomicU64,
}

impl std::fmt::Debug for GlslShaderSourceToSpirvCompiler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GlslShaderSourceToSpirvCompiler")
            .field("compiler_version", &VENDORED_GLSL_COMPILER_VERSION)
            .field("invocation_count", &self.invocation_count())
            .finish_non_exhaustive()
    }
}

impl Default for GlslShaderSourceToSpirvCompiler {
    fn default() -> Self {
        Self::new()
    }
}

impl GlslShaderSourceToSpirvCompiler {
    #[must_use]
    pub fn new() -> Self {
        Self {
            compiler: OnceLock::new(),
            compiled_spirv_by_key: Mutex::new(HashMap::new()),
            invocation_count: AtomicU64::new(0),
        }
    }

    /// The vendored compiler, started on first use.
    ///
    /// A lost initialization race drops the redundant compiler; both are
    /// equivalent, so which one wins does not matter.
    fn compiler(&self) -> Result<&shaderc::Compiler> {
        if let Some(started) = self.compiler.get() {
            return Ok(started);
        }
        let started = shaderc::Compiler::new().map_err(|e| {
            Error::GpuError(format!(
                "{VENDORED_GLSL_COMPILER_VERSION} would not start: {e}"
            ))
        })?;
        Ok(self.compiler.get_or_init(|| started))
    }

    /// How many times the compiler itself has run.
    ///
    /// What a cache-hit assertion counts. Elapsed time cannot stand in for it:
    /// re-creating a kernel is free of *compilation* while still allocating
    /// Vulkan handles, so a timing comparison measures the wrong half.
    #[must_use]
    pub fn invocation_count(&self) -> u64 {
        self.invocation_count.load(Ordering::Relaxed)
    }

    /// Compile `source` for `stage`, or hand back what an identical earlier
    /// request compiled.
    ///
    /// `label` names the source in the compiler's diagnostics — it reaches the
    /// author as the `<label>:<line>:` prefix on a syntax error.
    pub fn compile_or_reuse(
        &self,
        source: &str,
        stage: ShaderPipelineStage,
        entry_point: &str,
        label: &str,
    ) -> Result<Arc<Vec<u8>>> {
        if entry_point != GLSL_SOURCE_ENTRY_POINT {
            return Err(Error::GpuError(format!(
                "kernel source declares entry point `{entry_point}`, but a GLSL entry point is \
                 always `{GLSL_SOURCE_ENTRY_POINT}` — glslang will not rename one, so the \
                 pipeline would be built against a function the module does not contain. \
                 Rename the shader's function to `{GLSL_SOURCE_ENTRY_POINT}`, or hand over \
                 pre-compiled SPIR-V if the entry point has to differ"
            )));
        }

        let key = GlslShaderCompilationCacheKey::new(
            source,
            stage,
            entry_point,
            ShaderTargetEnvironment::ENGINE,
        );
        if let Some(cached) = self.compiled_spirv_by_key.lock().unwrap().get(&key) {
            tracing::debug!(
                rhi_op = "compile_glsl_shader_source",
                stage = stage.wire_name(),
                label,
                "GlslShaderSourceToSpirvCompiler — cache hit"
            );
            return Ok(Arc::clone(cached));
        }

        let spirv = Arc::new(self.compile(source, stage, entry_point, label)?);
        Ok(Arc::clone(
            self.compiled_spirv_by_key
                .lock()
                .unwrap()
                .entry(key)
                .or_insert(spirv),
        ))
    }

    fn compile(
        &self,
        source: &str,
        stage: ShaderPipelineStage,
        entry_point: &str,
        label: &str,
    ) -> Result<Vec<u8>> {
        let target = ShaderTargetEnvironment::ENGINE;
        let mut options = shaderc::CompileOptions::new().map_err(|e| {
            Error::GpuError(format!("shader compile options would not initialize: {e}"))
        })?;
        options.set_target_env(shaderc::TargetEnv::Vulkan, target.vulkan_env_version as u32);
        options.set_target_spirv(target.spirv_version);
        // No optimization level is set, and none may be: every level above
        // zero strips `OpName`, and a binding with no reflected name cannot be
        // dispatched against by name at all. The engine keeps debug names in
        // what it compiles itself — `derive_bindings_from_spirv` refuses a
        // module that lost them, so setting one here would refuse every kernel
        // the engine compiled.

        let compiler = self.compiler()?;
        self.invocation_count.fetch_add(1, Ordering::Relaxed);
        let compiled = compiler
            .compile_into_spirv(
                source,
                stage.shaderc_kind(),
                label,
                entry_point,
                Some(&options),
            )
            .map_err(|e| {
                Error::GpuError(format!(
                    "compiling the {} kernel source failed: {e}",
                    stage.wire_name()
                ))
            })?;

        tracing::debug!(
            rhi_op = "compile_glsl_shader_source",
            stage = stage.wire_name(),
            label,
            spirv_bytes = compiled.as_binary_u8().len(),
            "GlslShaderSourceToSpirvCompiler — compiled"
        );
        Ok(compiled.as_binary_u8().to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPUTE_SOURCE: &str = "\
#version 450
layout(local_size_x = 8, local_size_y = 8) in;
layout(set = 0, binding = 0) uniform sampler2D source_image;
layout(set = 0, binding = 1, rgba8) uniform writeonly image2D output_image;
void main() {
    ivec2 at = ivec2(gl_GlobalInvocationID.xy);
    imageStore(output_image, at, texelFetch(source_image, at, 0));
}
";

    fn compiler() -> GlslShaderSourceToSpirvCompiler {
        GlslShaderSourceToSpirvCompiler::new()
    }

    fn compile(
        compiler: &GlslShaderSourceToSpirvCompiler,
        source: &str,
        stage: ShaderPipelineStage,
    ) -> Arc<Vec<u8>> {
        compiler
            .compile_or_reuse(source, stage, GLSL_SOURCE_ENTRY_POINT, "test.glsl")
            .unwrap_or_else(|e| panic!("compiling the {} source failed: {e}", stage.wire_name()))
    }

    /// A SPIR-V module starts with the magic number, so this is the cheapest
    /// proof that what came back is a module rather than an error string.
    fn is_spirv_module(spirv: &[u8]) -> bool {
        spirv.len() >= 4 && spirv[..4] == 0x0723_0203u32.to_le_bytes()
    }

    #[test]
    fn glsl_source_compiles_to_a_spirv_module() {
        let spirv = compile(&compiler(), COMPUTE_SOURCE, ShaderPipelineStage::Compute);
        assert!(is_spirv_module(&spirv), "expected a SPIR-V module");
    }

    /// The claim the ADR rejected a pure-Rust front end on: a GLSL contract
    /// that excluded the ray-tracing pipeline stages would not be the contract
    /// the plan states. Asserted by compiling one of each, not by citing the
    /// compiler's feature list.
    #[test]
    fn every_ray_tracing_pipeline_stage_compiles() {
        let compiler = compiler();
        let sources = [
            (
                ShaderPipelineStage::RayGeneration,
                "layout(location = 0) rayPayloadEXT vec3 payload;\nvoid main() { payload = vec3(1.0); }",
            ),
            (
                ShaderPipelineStage::RayMiss,
                "layout(location = 0) rayPayloadInEXT vec3 payload;\nvoid main() { payload = vec3(0.0); }",
            ),
            (
                ShaderPipelineStage::RayClosestHit,
                "layout(location = 0) rayPayloadInEXT vec3 payload;\nvoid main() { payload = vec3(0.5); }",
            ),
            (
                ShaderPipelineStage::RayAnyHit,
                "layout(location = 0) rayPayloadInEXT vec3 payload;\nvoid main() { ignoreIntersectionEXT; }",
            ),
            (
                ShaderPipelineStage::RayIntersection,
                "hitAttributeEXT vec2 attribs;\nvoid main() { reportIntersectionEXT(1.0, 0u); }",
            ),
            (
                ShaderPipelineStage::RayCallable,
                "layout(location = 0) callableDataInEXT vec3 called;\nvoid main() { called = vec3(1.0); }",
            ),
        ];
        for (stage, body) in sources {
            let source = format!("#version 460\n#extension GL_EXT_ray_tracing : require\n{body}\n");
            let spirv = compile(&compiler, &source, stage);
            assert!(
                is_spirv_module(&spirv),
                "the {} stage did not compile to a module",
                stage.wire_name()
            );
        }
    }

    /// The rule that makes by-name binding work at all. The engine never
    /// optimizes what it compiles, so the shader's own spelling of each
    /// binding survives into reflection.
    #[test]
    fn an_engine_compiled_module_keeps_its_binding_names() {
        let spirv = compile(&compiler(), COMPUTE_SOURCE, ShaderPipelineStage::Compute);
        let (bindings, _push_constant_size) = crate::core::rhi::derive_bindings_from_spirv(&spirv)
            .expect(
                "reflection refuses a name-stripped module, so this failing means the engine \
                 stripped names it must keep",
            );
        let names: Vec<&str> = bindings
            .iter()
            .filter_map(|spec| spec.name.as_deref())
            .collect();
        assert!(
            names.contains(&"source_image") && names.contains(&"output_image"),
            "expected the shader's own binding names, got {names:?}"
        );
    }

    #[test]
    fn compiling_the_same_source_twice_invokes_the_compiler_once() {
        let compiler = compiler();
        let first = compile(&compiler, COMPUTE_SOURCE, ShaderPipelineStage::Compute);
        let second = compile(&compiler, COMPUTE_SOURCE, ShaderPipelineStage::Compute);
        assert_eq!(compiler.invocation_count(), 1);
        assert_eq!(first, second);
    }

    /// The key covers everything that changes the output. One test per
    /// component, each proving the compiler ran a second time — the
    /// source-alone key this replaces would have served the first result for
    /// every one of them.
    #[test]
    fn a_different_stage_is_a_different_compilation() {
        let compiler = compiler();
        let source = "#version 450\nvoid main() {}\n";
        compile(&compiler, source, ShaderPipelineStage::Compute);
        compile(&compiler, source, ShaderPipelineStage::Vertex);
        assert_eq!(compiler.invocation_count(), 2);
    }

    #[test]
    fn a_different_target_environment_is_a_different_key() {
        let vulkan_1_3_spirv_1_6 = ShaderTargetEnvironment {
            vulkan_env_version: shaderc::EnvVersion::Vulkan1_3,
            spirv_version: shaderc::SpirvVersion::V1_6,
        };
        let engine = GlslShaderCompilationCacheKey::new(
            COMPUTE_SOURCE,
            ShaderPipelineStage::Compute,
            GLSL_SOURCE_ENTRY_POINT,
            ShaderTargetEnvironment::ENGINE,
        );
        let other = GlslShaderCompilationCacheKey::new(
            COMPUTE_SOURCE,
            ShaderPipelineStage::Compute,
            GLSL_SOURCE_ENTRY_POINT,
            vulkan_1_3_spirv_1_6,
        );
        assert_ne!(engine, other);
    }

    #[test]
    fn a_different_entry_point_is_a_different_key() {
        let main = GlslShaderCompilationCacheKey::new(
            COMPUTE_SOURCE,
            ShaderPipelineStage::Compute,
            GLSL_SOURCE_ENTRY_POINT,
            ShaderTargetEnvironment::ENGINE,
        );
        let sharpen = GlslShaderCompilationCacheKey::new(
            COMPUTE_SOURCE,
            ShaderPipelineStage::Compute,
            "sharpen",
            ShaderTargetEnvironment::ENGINE,
        );
        assert_ne!(main, sharpen);
    }

    /// glslang will not rename a GLSL entry point: handing shaderc another
    /// name emits a module still declaring `main`. Refused at construction
    /// rather than accepted into a pipeline that cannot find its function.
    #[test]
    fn a_non_main_entry_point_on_glsl_source_is_refused() {
        let err = compiler()
            .compile_or_reuse(
                COMPUTE_SOURCE,
                ShaderPipelineStage::Compute,
                "sharpen",
                "test.comp",
            )
            .err()
            .expect("a GLSL entry point other than `main` must be refused");
        let message = format!("{err}");
        assert!(message.contains("sharpen"), "{message}");
        assert!(message.contains("main"), "{message}");
    }

    /// The author reads this message, so it has to carry the compiler's own
    /// diagnostic — the label it was given and the offending line.
    #[test]
    fn a_syntax_error_names_the_source_and_the_line() {
        let err = compiler()
            .compile_or_reuse(
                "#version 450\nvoid main() { no_such_function(); }\n",
                ShaderPipelineStage::Compute,
                GLSL_SOURCE_ENTRY_POINT,
                "blur.comp",
            )
            .err()
            .expect("a shader that does not compile must be refused");
        let message = format!("{err}");
        assert!(message.contains("blur.comp"), "{message}");
        assert!(message.contains(":2"), "{message}");
        assert!(message.contains("no_such_function"), "{message}");
    }

    #[test]
    fn a_failed_compilation_is_not_cached_as_a_success() {
        let compiler = compiler();
        let broken = "#version 450\nvoid main() { nope(); }\n";
        for _ in 0..2 {
            compiler
                .compile_or_reuse(
                    broken,
                    ShaderPipelineStage::Compute,
                    GLSL_SOURCE_ENTRY_POINT,
                    "broken.comp",
                )
                .err()
                .expect("a broken shader must stay refused");
        }
        assert_eq!(compiler.invocation_count(), 2);
    }

    #[test]
    fn an_unknown_stage_name_is_refused_naming_every_accepted_one() {
        let err = ShaderPipelineStage::from_wire_name("geometry")
            .err()
            .expect("`geometry` is not a stage this engine compiles for");
        let message = format!("{err}");
        assert!(message.contains("geometry"), "{message}");
        for stage in ShaderPipelineStage::ALL {
            assert!(message.contains(stage.wire_name()), "{message}");
        }
    }

    #[test]
    fn every_stage_wire_name_round_trips() {
        for stage in ShaderPipelineStage::ALL {
            assert_eq!(
                ShaderPipelineStage::from_wire_name(stage.wire_name()).unwrap(),
                *stage
            );
        }
    }

    /// The cache key names a compiler version, and nothing at runtime can
    /// check it against what is linked in. `Cargo.lock` can.
    #[test]
    fn the_recorded_compiler_version_matches_the_pinned_dependency() {
        let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("the engine crate sits two levels under the workspace root");
        let lock = std::fs::read_to_string(workspace_root.join("Cargo.lock"))
            .expect("the workspace lockfile must be readable");
        let locked_version = lock
            .split("[[package]]")
            .find(|entry| entry.contains("name = \"shaderc\""))
            .and_then(|entry| {
                entry
                    .lines()
                    .find_map(|line| line.trim().strip_prefix("version = \""))
            })
            .and_then(|version| version.strip_suffix('"'))
            .expect("shaderc must be in the lockfile");
        assert_eq!(
            VENDORED_GLSL_COMPILER_VERSION,
            format!("shaderc {locked_version}"),
            "the compilation cache key names a compiler version that is not the one linked in; \
             bump VENDORED_GLSL_COMPILER_VERSION with the dependency, so a compiler change \
             invalidates the cache it would otherwise silently reuse"
        );
    }
}
