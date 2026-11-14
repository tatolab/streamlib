// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The engine's Slang shader compiler and the cache in front of it.
//!
//! Slang text is the kernel source contract. The compiler library travels with
//! the runtime unit and is opened by path on first use, so a kernel author
//! needs no shader toolchain beyond the installed wheel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use sha2::{Digest, Sha256};
pub use streamlib_slang_shader_compiler::{
    DEFAULT_SHADER_ENTRY_POINT, ShaderCompilationBackend, ShaderCompilationTargetStage,
};
use streamlib_slang_shader_compiler::{SlangCompilerGlobalSession, SlangShaderSourceToCompile};

use crate::core::{Error, Result};

/// Everything about a compilation that changes the code it emits.
///
/// Never the source alone: the same text compiled for another stage, another
/// backend, or by another compiler build is a different module.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShaderCompilationCacheKey {
    source_sha256: [u8; 32],
    stage: ShaderCompilationTargetStage,
    entry_point: String,
    backend: ShaderCompilationBackend,
    compiler_version: String,
}

impl ShaderCompilationCacheKey {
    #[must_use]
    pub fn new(
        source: &str,
        stage: ShaderCompilationTargetStage,
        entry_point: &str,
        backend: ShaderCompilationBackend,
        compiler_version: &str,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(source.as_bytes());
        Self {
            source_sha256: hasher.finalize().into(),
            stage,
            entry_point: entry_point.to_string(),
            backend,
            compiler_version: compiler_version.to_string(),
        }
    }
}

/// Compiles Slang text and remembers what it compiled.
///
/// Entries are never evicted: they are bounded by the distinct sources a
/// graph's processors author, and a kernel outlives the helper that
/// registered it.
pub struct SlangShaderSourceCompiler {
    compiled_code_by_key: Mutex<HashMap<ShaderCompilationCacheKey, Arc<[u8]>>>,
    invocation_count: AtomicU64,
}

impl std::fmt::Debug for SlangShaderSourceCompiler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SlangShaderSourceCompiler")
            .field("invocation_count", &self.invocation_count())
            .finish_non_exhaustive()
    }
}

impl Default for SlangShaderSourceCompiler {
    fn default() -> Self {
        Self::new()
    }
}

impl SlangShaderSourceCompiler {
    #[must_use]
    pub fn new() -> Self {
        Self {
            compiled_code_by_key: Mutex::new(HashMap::new()),
            invocation_count: AtomicU64::new(0),
        }
    }

    /// How many times the compiler itself has run.
    ///
    /// What a cache-hit assertion counts. Elapsed time cannot stand in: a
    /// re-created kernel is free of *compilation* while still allocating
    /// handles, so a timing comparison measures the wrong half.
    #[must_use]
    pub fn invocation_count(&self) -> u64 {
        self.invocation_count.load(Ordering::Relaxed)
    }

    /// The compiler as the cache key names it: `slang <build tag>`.
    pub fn compiler_version(&self) -> Result<String> {
        let global_session = lock_the_process_slang_global_session()?;
        Ok(global_session.compiler_version().to_owned())
    }

    /// Compile `source` for `stage` to SPIR-V, or hand back what an identical
    /// earlier request compiled.
    ///
    /// `label` names the source in the compiler's diagnostics: it reaches the
    /// author as the `<label>:<line>:<column>` location of an error.
    pub fn compile_or_reuse(
        &self,
        source: &str,
        stage: ShaderCompilationTargetStage,
        entry_point: &str,
        label: &str,
    ) -> Result<Arc<[u8]>> {
        self.compile_for_backend_or_reuse(
            source,
            stage,
            ShaderCompilationBackend::SpirV1_4ForVulkan1_2,
            entry_point,
            label,
        )
    }

    /// Compile `source` for `stage` and `backend`, or hand back what an
    /// identical earlier request compiled.
    pub fn compile_for_backend_or_reuse(
        &self,
        source: &str,
        stage: ShaderCompilationTargetStage,
        backend: ShaderCompilationBackend,
        entry_point: &str,
        label: &str,
    ) -> Result<Arc<[u8]>> {
        let global_session = lock_the_process_slang_global_session()?;
        let key = ShaderCompilationCacheKey::new(
            source,
            stage,
            entry_point,
            backend,
            global_session.compiler_version(),
        );
        if let Some(cached) = self.compiled_code_by_key.lock().unwrap().get(&key) {
            tracing::debug!(
                rhi_op = "compile_slang_shader_source",
                stage = stage.wire_name(),
                label,
                "SlangShaderSourceCompiler — cache hit"
            );
            return Ok(Arc::clone(cached));
        }

        self.invocation_count.fetch_add(1, Ordering::Relaxed);
        let compiled: Arc<[u8]> = global_session
            .compile(&SlangShaderSourceToCompile {
                backend,
                ..SlangShaderSourceToCompile::for_spirv(source, stage, entry_point, label)
            })
            .map_err(|refusal| Error::GpuError(refusal.to_string()))?
            .into();
        drop(global_session);
        tracing::debug!(
            rhi_op = "compile_slang_shader_source",
            stage = stage.wire_name(),
            label,
            compiled_bytes = compiled.len(),
            "SlangShaderSourceCompiler — compiled"
        );
        Ok(Arc::clone(
            self.compiled_code_by_key
                .lock()
                .unwrap()
                .entry(key)
                .or_insert(compiled),
        ))
    }
}

/// The Slang compiler libraries the engine looks for, in order: the ones the
/// runtime unit carries, then, in a debug build, the one
/// `scripts/stage_slang_compiler_libraries.sh` stages under the workspace's
/// `target/` for `cargo test`.
fn slang_compiler_library_candidate_paths() -> Vec<PathBuf> {
    let mut candidate_paths = streamlib_consumer_rhi::slang_compiler_library_candidate_paths();
    if cfg!(debug_assertions) {
        candidate_paths.push(
            streamlib_slang_shader_compiler::workspace_staged_slang_compiler_library(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
            ),
        );
    }
    candidate_paths
}

/// Slang's one global session for the process, behind the lock every compile
/// holds from start to finish: a global session, and every session and module
/// it creates, is not thread-safe.
fn lock_the_process_slang_global_session()
-> Result<std::sync::MutexGuard<'static, SlangCompilerGlobalSession>> {
    static PROCESS_SLANG_GLOBAL_SESSION: OnceLock<
        std::result::Result<Mutex<SlangCompilerGlobalSession>, String>,
    > = OnceLock::new();
    match PROCESS_SLANG_GLOBAL_SESSION.get_or_init(|| {
        SlangCompilerGlobalSession::open_the_first_library_of(
            &slang_compiler_library_candidate_paths(),
        )
        .map(Mutex::new)
        .map_err(|refusal| refusal.to_string())
    }) {
        Ok(global_session) => Ok(global_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)),
        Err(why_the_compiler_is_unavailable) => {
            Err(Error::GpuError(why_the_compiler_is_unavailable.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMPUTE_SOURCE: &str = "\
[[vk::binding(0, 0)]] Sampler2D source_image;
[[vk::binding(1, 0)]] [format(\"rgba8\")] RWTexture2D<float4> output_image;
[shader(\"compute\")] [numthreads(8, 8, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    output_image[invocation.xy] = source_image.Load(int3(invocation.xy, 0));
}
";

    fn compile(
        compiler: &SlangShaderSourceCompiler,
        source: &str,
        stage: ShaderCompilationTargetStage,
    ) -> Arc<[u8]> {
        compiler
            .compile_or_reuse(source, stage, DEFAULT_SHADER_ENTRY_POINT, "test.slang")
            .unwrap_or_else(|e| panic!("compiling the {} source failed: {e}", stage.wire_name()))
    }

    fn is_spirv_module(spirv: &[u8]) -> bool {
        spirv.len() >= 4 && spirv[..4] == 0x0723_0203u32.to_le_bytes()
    }

    fn spirv_holds_the_string(spirv: &[u8], text: &str) -> bool {
        let mut needle = text.as_bytes().to_vec();
        needle.push(0);
        spirv.windows(needle.len()).any(|window| window == needle)
    }

    #[test]
    fn slang_source_compiles_to_a_spirv_module() {
        let spirv = compile(
            &SlangShaderSourceCompiler::new(),
            COMPUTE_SOURCE,
            ShaderCompilationTargetStage::Compute,
        );
        assert!(is_spirv_module(&spirv), "expected a SPIR-V module");
    }

    #[test]
    fn every_ray_tracing_pipeline_stage_compiles() {
        let compiler = SlangShaderSourceCompiler::new();
        let payload = "struct RayPayload { float3 color; };\n";
        let sources = [
            (
                ShaderCompilationTargetStage::RayGeneration,
                "[[vk::binding(0, 0)]] RWTexture2D<float4> output_image;\n\
                 void main() { output_image[DispatchRaysIndex().xy] = float4(1.0); }",
            ),
            (
                ShaderCompilationTargetStage::RayMiss,
                "void main(inout RayPayload payload) { payload.color = float3(0.0); }",
            ),
            (
                ShaderCompilationTargetStage::RayClosestHit,
                "void main(inout RayPayload payload, BuiltInTriangleIntersectionAttributes hit) \
                 { payload.color = float3(hit.barycentrics, 0.5); }",
            ),
            (
                ShaderCompilationTargetStage::RayAnyHit,
                "void main(inout RayPayload payload, BuiltInTriangleIntersectionAttributes hit) \
                 { IgnoreHit(); }",
            ),
            (
                ShaderCompilationTargetStage::RayIntersection,
                "struct SphereHit { float2 uv; };\n\
                 void main() { SphereHit hit; hit.uv = float2(0.0); ReportHit(1.0, 0, hit); }",
            ),
            (
                ShaderCompilationTargetStage::RayCallable,
                "void main(inout RayPayload called) { called.color = float3(1.0); }",
            ),
        ];
        for (stage, body) in sources {
            let spirv = compile(&compiler, &format!("{payload}{body}\n"), stage);
            assert!(
                is_spirv_module(&spirv),
                "the {} stage did not compile to a module",
                stage.wire_name()
            );
        }
    }

    /// By-name binding needs the shader's own spelling of each binding in the
    /// module, with optimisation on and debug information off.
    #[test]
    fn an_engine_compiled_module_keeps_its_binding_names() {
        let spirv = compile(
            &SlangShaderSourceCompiler::new(),
            COMPUTE_SOURCE,
            ShaderCompilationTargetStage::Compute,
        );
        let (bindings, _push_constant_size) = crate::core::rhi::derive_bindings_from_spirv(&spirv)
            .expect("reflection refuses a module without binding names");
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
    fn storage_buffers_and_push_constants_keep_their_variable_names() {
        let spirv = compile(
            &SlangShaderSourceCompiler::new(),
            "\
struct DoublingParameters { float factor; uint element_count; };
[[vk::push_constant]] ConstantBuffer<DoublingParameters> doubling_parameters;
[[vk::binding(0, 0)]] StructuredBuffer<float> undoubled_tensor;
[[vk::binding(1, 0)]] RWStructuredBuffer<float> doubled_tensor;
[numthreads(64, 1, 1)]
void main(uint3 invocation : SV_DispatchThreadID) {
    if (invocation.x >= doubling_parameters.element_count) { return; }
    doubled_tensor[invocation.x] = undoubled_tensor[invocation.x] * doubling_parameters.factor;
}
",
            ShaderCompilationTargetStage::Compute,
        );
        let (bindings, push_constant_size) =
            crate::core::rhi::derive_bindings_from_spirv(&spirv).expect("reflection");
        let names: Vec<&str> = bindings
            .iter()
            .filter_map(|spec| spec.name.as_deref())
            .collect();
        assert_eq!(
            names,
            ["undoubled_tensor", "doubled_tensor"],
            "{bindings:?}"
        );
        assert_eq!(push_constant_size, 8);
    }

    #[test]
    fn an_entry_point_other_than_main_keeps_its_own_name() {
        let source = COMPUTE_SOURCE.replace("void main(", "void sharpen(");
        let spirv = SlangShaderSourceCompiler::new()
            .compile_or_reuse(
                &source,
                ShaderCompilationTargetStage::Compute,
                "sharpen",
                "sharpen.slang",
            )
            .expect("a named entry point compiles");
        assert!(spirv_holds_the_string(&spirv, "sharpen"));
        assert!(!spirv_holds_the_string(&spirv, "main"));
    }

    #[test]
    fn a_missing_entry_point_is_refused_by_name() {
        let message = SlangShaderSourceCompiler::new()
            .compile_or_reuse(
                COMPUTE_SOURCE,
                ShaderCompilationTargetStage::Compute,
                "sharpen",
                "blur.slang",
            )
            .expect_err("a source without the named entry point must be refused")
            .to_string();
        assert!(message.contains("sharpen"), "{message}");
        assert!(message.contains("blur.slang"), "{message}");
    }

    #[test]
    fn compiling_the_same_source_twice_invokes_the_compiler_once() {
        let compiler = SlangShaderSourceCompiler::new();
        let first = compile(
            &compiler,
            COMPUTE_SOURCE,
            ShaderCompilationTargetStage::Compute,
        );
        let second = compile(
            &compiler,
            COMPUTE_SOURCE,
            ShaderCompilationTargetStage::Compute,
        );
        assert_eq!(compiler.invocation_count(), 1);
        assert_eq!(first, second);
    }

    #[test]
    fn a_different_stage_is_a_different_compilation() {
        let compiler = SlangShaderSourceCompiler::new();
        let source = "\
struct VertexOutput { float4 position : SV_Position; };
[numthreads(1, 1, 1)] void compute_main() {}
VertexOutput vertex_main() { VertexOutput output; output.position = float4(0.0); return output; }
";
        compiler
            .compile_or_reuse(
                source,
                ShaderCompilationTargetStage::Compute,
                "compute_main",
                "test.slang",
            )
            .unwrap();
        compiler
            .compile_or_reuse(
                source,
                ShaderCompilationTargetStage::Vertex,
                "vertex_main",
                "test.slang",
            )
            .unwrap();
        assert_eq!(compiler.invocation_count(), 2);
    }

    #[test]
    fn each_key_component_changes_the_key() {
        let key = |source: &str, stage, entry_point: &str, compiler_version: &str| {
            ShaderCompilationCacheKey::new(
                source,
                stage,
                entry_point,
                ShaderCompilationBackend::SpirV1_4ForVulkan1_2,
                compiler_version,
            )
        };
        let engine = key(
            COMPUTE_SOURCE,
            ShaderCompilationTargetStage::Compute,
            "main",
            "slang 2026.12",
        );
        assert_ne!(
            engine,
            key(
                "",
                ShaderCompilationTargetStage::Compute,
                "main",
                "slang 2026.12"
            )
        );
        assert_ne!(
            engine,
            key(
                COMPUTE_SOURCE,
                ShaderCompilationTargetStage::Vertex,
                "main",
                "slang 2026.12"
            )
        );
        assert_ne!(
            engine,
            key(
                COMPUTE_SOURCE,
                ShaderCompilationTargetStage::Compute,
                "sharpen",
                "slang 2026.12"
            )
        );
        assert_ne!(
            engine,
            key(
                COMPUTE_SOURCE,
                ShaderCompilationTargetStage::Compute,
                "main",
                "slang 2026.13"
            )
        );
    }

    /// The author reads this message, so it carries the compiler's own
    /// diagnostic: the label it was given, the line, and the offending name.
    #[test]
    fn a_syntax_error_names_the_source_and_the_line() {
        let message = SlangShaderSourceCompiler::new()
            .compile_or_reuse(
                "[numthreads(1, 1, 1)]\nvoid main() { no_such_function(); }\n",
                ShaderCompilationTargetStage::Compute,
                DEFAULT_SHADER_ENTRY_POINT,
                "blur.slang",
            )
            .expect_err("a shader that does not compile must be refused")
            .to_string();
        assert!(message.contains("blur.slang:2:"), "{message}");
        assert!(message.contains("no_such_function"), "{message}");
    }

    #[test]
    fn glsl_source_is_refused_naming_its_version_line() {
        let message = SlangShaderSourceCompiler::new()
            .compile_or_reuse(
                "#version 450\nlayout(local_size_x = 1) in;\nvoid main() {}\n",
                ShaderCompilationTargetStage::Compute,
                DEFAULT_SHADER_ENTRY_POINT,
                "legacy.comp",
            )
            .expect_err("Slang is the only kernel source language")
            .to_string();
        assert!(message.contains("legacy.comp"), "{message}");
        assert!(message.contains("`#version`"), "{message}");
        assert!(message.contains("Slang"), "{message}");
    }

    #[test]
    fn a_failed_compilation_is_not_cached_as_a_success() {
        let compiler = SlangShaderSourceCompiler::new();
        for _ in 0..2 {
            compiler
                .compile_or_reuse(
                    "[numthreads(1, 1, 1)] void main() { nope(); }\n",
                    ShaderCompilationTargetStage::Compute,
                    DEFAULT_SHADER_ENTRY_POINT,
                    "broken.slang",
                )
                .expect_err("a broken shader must stay refused");
        }
        assert_eq!(compiler.invocation_count(), 2);
    }

    #[test]
    fn an_unknown_stage_name_is_refused_naming_every_accepted_one() {
        let message = ShaderCompilationTargetStage::from_wire_name("geometry")
            .expect_err("`geometry` is not a stage this engine compiles for")
            .to_string();
        assert!(message.contains("geometry"), "{message}");
        for stage in ShaderCompilationTargetStage::ALL {
            assert!(message.contains(stage.wire_name()), "{message}");
        }
    }

    #[test]
    fn every_stage_wire_name_round_trips() {
        for stage in ShaderCompilationTargetStage::ALL {
            assert_eq!(
                ShaderCompilationTargetStage::from_wire_name(stage.wire_name()).unwrap(),
                *stage
            );
        }
    }

    /// The cache key names the compiler build that is loaded, and the build
    /// the runtime unit carries is the one the layout pins.
    #[test]
    fn the_loaded_compiler_is_the_pinned_release() {
        let compiler_version = SlangShaderSourceCompiler::new()
            .compiler_version()
            .expect("the Slang compiler library opens");
        assert!(
            compiler_version.starts_with("slang ")
                && compiler_version.contains(
                    streamlib_consumer_rhi::runtime_unit_layout::BUNDLED_SLANG_COMPILER_VERSION
                ),
            "{compiler_version}"
        );
    }
}
