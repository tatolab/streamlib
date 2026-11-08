// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The engine's Slang shader compiler and the cache in front of it.
//!
//! Slang text is the kernel source contract. The compiler library travels with
//! the runtime unit and is opened by path on first use, so a kernel author
//! needs no shader toolchain beyond the installed wheel.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_void};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use sha2::{Digest, Sha256};

use crate::core::{Error, Result};

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

    const fn slang_stage(self) -> slang_compiler_abi::SlangStage {
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
    const fn slang_profile_name(self) -> &'static CStr {
        match self {
            Self::SpirV1_4ForVulkan1_2 => c"spirv_1_4",
        }
    }

    const fn slang_compile_target(self) -> i32 {
        match self {
            Self::SpirV1_4ForVulkan1_2 => slang_compiler_abi::SLANG_SPIRV,
        }
    }
}

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
        Ok(global_session.compiler_version.clone())
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
            &global_session.compiler_version,
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
            .compile(source, stage, backend, entry_point, label)?
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
/// runtime unit carries, then, in a debug build, the ones
/// `scripts/stage_slang_compiler_libraries.sh` stages under the workspace's
/// `target/` for `cargo test`.
fn slang_compiler_library_candidate_paths() -> Vec<PathBuf> {
    let mut candidate_paths = streamlib_consumer_rhi::slang_compiler_library_candidate_paths();
    if cfg!(debug_assertions) {
        candidate_paths.push(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(WORKSPACE_STAGED_SLANG_COMPILER_DIRECTORY)
                .join(
                    streamlib_consumer_rhi::runtime_unit_layout::SLANG_COMPILER_LIBRARY_FILE_NAME,
                ),
        );
    }
    candidate_paths
}

/// Where `scripts/stage_slang_compiler_libraries.sh` stages the libraries in
/// the workspace, relative to its root.
const WORKSPACE_STAGED_SLANG_COMPILER_DIRECTORY: &str = "target/slang_compiler";

/// Slang's one global session for the process, behind the lock every compile
/// holds from start to finish: a global session, and every session and module
/// it creates, is not thread-safe.
fn lock_the_process_slang_global_session()
-> Result<std::sync::MutexGuard<'static, LoadedSlangGlobalSession>> {
    static PROCESS_SLANG_GLOBAL_SESSION: OnceLock<
        std::result::Result<Mutex<LoadedSlangGlobalSession>, String>,
    > = OnceLock::new();
    match PROCESS_SLANG_GLOBAL_SESSION
        .get_or_init(|| LoadedSlangGlobalSession::open().map(Mutex::new))
    {
        Ok(global_session) => Ok(global_session
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)),
        Err(why_the_compiler_is_unavailable) => {
            Err(Error::GpuError(why_the_compiler_is_unavailable.clone()))
        }
    }
}

/// The opened Slang library and the global session made from it, kept for the
/// life of the process.
struct LoadedSlangGlobalSession {
    global_session: SlangComObject,
    compiler_version: String,
    _slang_compiler_library: libloading::Library,
}

// SAFETY: the global session is only reached through the process lock in
// `lock_the_process_slang_global_session`, so no two threads use it at once.
unsafe impl Send for LoadedSlangGlobalSession {}

impl LoadedSlangGlobalSession {
    fn open() -> std::result::Result<Self, String> {
        let mut refusal_per_candidate = Vec::new();
        for candidate_path in slang_compiler_library_candidate_paths() {
            // SAFETY: opening the Slang compiler runs only its own initialisers.
            match unsafe { libloading::Library::new(&candidate_path) } {
                Ok(slang_compiler_library) => {
                    tracing::info!(
                        slang_compiler_library = %candidate_path.display(),
                        "Slang compiler library opened"
                    );
                    return Self::create_global_session_from(slang_compiler_library);
                }
                Err(open_failure) => refusal_per_candidate
                    .push(format!("{}: {open_failure}", candidate_path.display())),
            }
        }
        Err(format!(
            "the Slang shader compiler library is not where the runtime carries it, so no \
             shader source can be compiled; a development checkout stages it with \
             `scripts/stage_slang_compiler_libraries.sh`. Looked in:\n  {}",
            refusal_per_candidate.join("\n  ")
        ))
    }

    fn create_global_session_from(
        slang_compiler_library: libloading::Library,
    ) -> std::result::Result<Self, String> {
        // SAFETY: the symbol is Slang's exported C entry point, with this signature.
        let create_global_session = unsafe {
            slang_compiler_library
                .get::<slang_compiler_abi::CreateGlobalSession2>(b"slang_createGlobalSession2\0")
        }
        .map_err(|e| format!("the Slang compiler library exports no global session: {e}"))?;
        let global_session_desc = slang_compiler_abi::SlangGlobalSessionDesc::default();
        let mut raw_global_session: *mut c_void = std::ptr::null_mut();
        // SAFETY: both pointers are valid for the call; the out pointer receives
        // one reference the returned object owns.
        let result =
            unsafe { create_global_session(&global_session_desc, &mut raw_global_session) };
        let global_session = SlangComObject::from_owned(raw_global_session).ok_or_else(|| {
            format!("Slang would not create its global session (result {result:#x})")
        })?;
        // SAFETY: `getBuildTagString` returns a static NUL-terminated string.
        let build_tag = unsafe {
            let build_tag_string = global_session.method::<slang_compiler_abi::GetBuildTagString>(
                slang_compiler_abi::IGLOBAL_SESSION_GET_BUILD_TAG_STRING,
            )(global_session.as_ptr());
            CStr::from_ptr(build_tag_string)
                .to_string_lossy()
                .into_owned()
        };
        Ok(Self {
            global_session,
            compiler_version: format!("slang {build_tag}"),
            _slang_compiler_library: slang_compiler_library,
        })
    }

    fn compile(
        &self,
        source: &str,
        stage: ShaderCompilationTargetStage,
        backend: ShaderCompilationBackend,
        entry_point: &str,
        label: &str,
    ) -> Result<Vec<u8>> {
        let compile_failure = |what: &str, diagnostics: Option<String>| {
            Error::GpuError(format!(
                "compiling `{label}` as a {} shader failed {what}{}",
                stage.wire_name(),
                diagnostics
                    .map(|text| format!(":\n{}", text.trim_end()))
                    .unwrap_or_default()
            ))
        };
        let source = CString::new(source)
            .map_err(|_| compile_failure("because the source holds a NUL byte", None))?;
        let label_text = CString::new(label)
            .map_err(|_| compile_failure("because its label holds a NUL byte", None))?;
        let entry_point_name = CString::new(entry_point)
            .map_err(|_| compile_failure("because the entry point holds a NUL byte", None))?;

        // SAFETY: the global session is valid and only this thread holds it.
        let profile = unsafe {
            self.global_session
                .method::<slang_compiler_abi::FindProfile>(
                    slang_compiler_abi::IGLOBAL_SESSION_FIND_PROFILE,
                )(
                self.global_session.as_ptr(),
                backend.slang_profile_name().as_ptr(),
            )
        };
        let compiler_option_entries = slang_compiler_abi::engine_compiler_option_entries();
        let target_desc = slang_compiler_abi::TargetDesc {
            format: backend.slang_compile_target(),
            profile,
            ..slang_compiler_abi::TargetDesc::default()
        };
        let session_desc = slang_compiler_abi::SessionDesc {
            targets: &target_desc,
            target_count: 1,
            compiler_option_entries: compiler_option_entries.as_ptr(),
            compiler_option_entry_count: compiler_option_entries.len() as u32,
            ..slang_compiler_abi::SessionDesc::default()
        };

        let mut raw_session: *mut c_void = std::ptr::null_mut();
        // SAFETY: the descriptor and everything it points to outlive the call.
        unsafe {
            self.global_session
                .method::<slang_compiler_abi::CreateSession>(
                    slang_compiler_abi::IGLOBAL_SESSION_CREATE_SESSION,
                )(
                self.global_session.as_ptr(),
                &session_desc,
                &mut raw_session,
            );
        }
        let session = SlangComObject::from_owned(raw_session)
            .ok_or_else(|| compile_failure("because Slang would not open a session", None))?;

        let mut module_diagnostics: *mut c_void = std::ptr::null_mut();
        // SAFETY: the strings are NUL-terminated and outlive the call. The
        // module is owned by the session, so it takes no reference of its own.
        let raw_module = unsafe {
            session.method::<slang_compiler_abi::LoadModuleFromSourceString>(
                slang_compiler_abi::ISESSION_LOAD_MODULE_FROM_SOURCE_STRING,
            )(
                session.as_ptr(),
                c"kernel".as_ptr(),
                label_text.as_ptr(),
                source.as_ptr(),
                &mut module_diagnostics,
            )
        };
        let module_diagnostics = take_diagnostics(module_diagnostics);
        let module = NonNull::new(raw_module).ok_or_else(|| {
            if module_diagnostics
                .as_deref()
                .is_some_and(|text| text.contains(SLANG_DIAGNOSTIC_FOR_SOURCE_READ_AS_GLSL))
            {
                compile_failure(
                    "because its `#version` line marks it as GLSL, and kernels are written in \
                     Slang",
                    module_diagnostics,
                )
            } else {
                compile_failure("to parse", module_diagnostics)
            }
        })?;

        let mut raw_entry_point: *mut c_void = std::ptr::null_mut();
        let mut entry_point_diagnostics: *mut c_void = std::ptr::null_mut();
        // SAFETY: the module is live; the out pointer receives one owned reference.
        unsafe {
            vtable_method::<slang_compiler_abi::FindAndCheckEntryPoint>(
                module.as_ptr(),
                slang_compiler_abi::IMODULE_FIND_AND_CHECK_ENTRY_POINT,
            )(
                module.as_ptr(),
                entry_point_name.as_ptr(),
                stage.slang_stage(),
                &mut raw_entry_point,
                &mut entry_point_diagnostics,
            );
        }
        let entry_point_diagnostics = take_diagnostics(entry_point_diagnostics);
        let entry_point_component =
            SlangComObject::from_owned(raw_entry_point).ok_or_else(|| {
                compile_failure(
                    &format!(
                        "because it has no `{entry_point}` entry point for the {} stage",
                        stage.wire_name()
                    ),
                    entry_point_diagnostics,
                )
            })?;

        let components = [module.as_ptr(), entry_point_component.as_ptr()];
        let mut raw_program: *mut c_void = std::ptr::null_mut();
        let mut program_diagnostics: *mut c_void = std::ptr::null_mut();
        // SAFETY: both components are live for the call.
        unsafe {
            session.method::<slang_compiler_abi::CreateCompositeComponentType>(
                slang_compiler_abi::ISESSION_CREATE_COMPOSITE_COMPONENT_TYPE,
            )(
                session.as_ptr(),
                components.as_ptr(),
                components.len() as slang_compiler_abi::SlangInt,
                &mut raw_program,
                &mut program_diagnostics,
            );
        }
        let program_diagnostics = take_diagnostics(program_diagnostics);
        let program = SlangComObject::from_owned(raw_program)
            .ok_or_else(|| compile_failure("to compose", program_diagnostics))?;

        let mut raw_linked_program: *mut c_void = std::ptr::null_mut();
        let mut link_diagnostics: *mut c_void = std::ptr::null_mut();
        // SAFETY: the program is live; the out pointer receives one owned reference.
        unsafe {
            program.method::<slang_compiler_abi::Link>(slang_compiler_abi::ICOMPONENT_TYPE_LINK)(
                program.as_ptr(),
                &mut raw_linked_program,
                &mut link_diagnostics,
            );
        }
        let link_diagnostics = take_diagnostics(link_diagnostics);
        let linked_program = SlangComObject::from_owned(raw_linked_program)
            .ok_or_else(|| compile_failure("to link", link_diagnostics))?;

        let mut raw_code: *mut c_void = std::ptr::null_mut();
        let mut code_diagnostics: *mut c_void = std::ptr::null_mut();
        // SAFETY: the linked program is live; the out pointer receives one owned blob.
        unsafe {
            linked_program.method::<slang_compiler_abi::GetEntryPointCode>(
                slang_compiler_abi::ICOMPONENT_TYPE_GET_ENTRY_POINT_CODE,
            )(
                linked_program.as_ptr(),
                0,
                0,
                &mut raw_code,
                &mut code_diagnostics,
            );
        }
        let code_diagnostics = take_diagnostics(code_diagnostics);
        let code = SlangComObject::from_owned(raw_code)
            .ok_or_else(|| compile_failure("to generate code", code_diagnostics))?;
        let compiled = code.blob_bytes().to_vec();

        drop(code);
        drop(linked_program);
        drop(program);
        drop(entry_point_component);
        drop(session);
        Ok(compiled)
    }
}

/// What Slang says when a source's `#version` line asks for its GLSL front end,
/// which the runtime does not ship.
const SLANG_DIAGNOSTIC_FOR_SOURCE_READ_AS_GLSL: &str = "'glsl' module not available";

/// The text of a diagnostics blob Slang handed back, releasing the blob.
fn take_diagnostics(raw_diagnostics_blob: *mut c_void) -> Option<String> {
    let diagnostics_blob = SlangComObject::from_owned(raw_diagnostics_blob)?;
    let text = String::from_utf8_lossy(diagnostics_blob.blob_bytes()).into_owned();
    (!text.trim().is_empty()).then_some(text)
}

/// One reference to a Slang COM object, released on drop.
struct SlangComObject(NonNull<c_void>);

impl SlangComObject {
    fn from_owned(raw_object: *mut c_void) -> Option<Self> {
        NonNull::new(raw_object).map(Self)
    }

    fn as_ptr(&self) -> *mut c_void {
        self.0.as_ptr()
    }

    /// The method at `slot` of this object's vtable.
    ///
    /// # Safety
    /// `slot` must be a method of this object's interface with signature `F`.
    unsafe fn method<F: Copy>(&self, slot: usize) -> F {
        // SAFETY: forwarded to the caller.
        unsafe { vtable_method(self.as_ptr(), slot) }
    }

    /// The bytes of this object, which must be an `ISlangBlob`.
    fn blob_bytes(&self) -> &[u8] {
        // SAFETY: the object is a live blob, whose pointer and size methods
        // describe one buffer it owns for as long as it lives.
        unsafe {
            let pointer = self.method::<slang_compiler_abi::GetBufferPointer>(
                slang_compiler_abi::IBLOB_GET_BUFFER_POINTER,
            )(self.as_ptr());
            let size = self.method::<slang_compiler_abi::GetBufferSize>(
                slang_compiler_abi::IBLOB_GET_BUFFER_SIZE,
            )(self.as_ptr());
            if pointer.is_null() || size == 0 {
                return &[];
            }
            std::slice::from_raw_parts(pointer.cast::<u8>(), size)
        }
    }
}

impl Drop for SlangComObject {
    fn drop(&mut self) {
        // SAFETY: every Slang interface starts with ISlangUnknown, and this
        // value holds exactly one reference.
        unsafe {
            self.method::<slang_compiler_abi::Release>(slang_compiler_abi::IUNKNOWN_RELEASE)(
                self.as_ptr(),
            );
        }
    }
}

/// The method at `slot` of the vtable `object` points at.
///
/// # Safety
/// `object` must be a live Slang COM object whose interface has a method of
/// signature `F` at `slot`.
unsafe fn vtable_method<F: Copy>(object: *mut c_void, slot: usize) -> F {
    // SAFETY: a COM object's first word points at its vtable, an array of
    // function pointers; the caller vouches for the slot and its signature.
    unsafe {
        let vtable = *object.cast::<*const *const c_void>();
        let function_pointer = *vtable.add(slot);
        std::mem::transmute_copy::<*const c_void, F>(&function_pointer)
    }
}

/// The part of Slang's C++ ABI (`slang.h`, release 2026.12) the compiler
/// calls: COM vtable slots, counted from `ISlangUnknown`'s three, and the
/// descriptor structs with their header defaults.
mod slang_compiler_abi {
    use std::ffi::{c_char, c_void};

    pub type SlangInt = i64;
    pub type SlangResult = i32;

    pub const SLANG_SPIRV: i32 = 6;

    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SlangStage(pub u32);

    impl SlangStage {
        pub const VERTEX: Self = Self(1);
        pub const FRAGMENT: Self = Self(5);
        pub const COMPUTE: Self = Self(6);
        pub const RAY_GENERATION: Self = Self(7);
        pub const INTERSECTION: Self = Self(8);
        pub const ANY_HIT: Self = Self(9);
        pub const CLOSEST_HIT: Self = Self(10);
        pub const MISS: Self = Self(11);
        pub const CALLABLE: Self = Self(12);
    }

    pub const IUNKNOWN_RELEASE: usize = 2;
    pub const IBLOB_GET_BUFFER_POINTER: usize = 3;
    pub const IBLOB_GET_BUFFER_SIZE: usize = 4;
    pub const IGLOBAL_SESSION_CREATE_SESSION: usize = 3;
    pub const IGLOBAL_SESSION_FIND_PROFILE: usize = 4;
    pub const IGLOBAL_SESSION_GET_BUILD_TAG_STRING: usize = 8;
    pub const ISESSION_CREATE_COMPOSITE_COMPONENT_TYPE: usize = 6;
    pub const ISESSION_LOAD_MODULE_FROM_SOURCE_STRING: usize = 20;
    pub const ICOMPONENT_TYPE_GET_ENTRY_POINT_CODE: usize = 6;
    pub const ICOMPONENT_TYPE_LINK: usize = 10;
    const ICOMPONENT_TYPE_METHOD_SLOTS_AFTER_IUNKNOWN: usize = 14;
    pub const IMODULE_FIND_AND_CHECK_ENTRY_POINT: usize =
        3 + ICOMPONENT_TYPE_METHOD_SLOTS_AFTER_IUNKNOWN + 8;

    pub type CreateGlobalSession2 = unsafe extern "C" fn(
        desc: *const SlangGlobalSessionDesc,
        out_global_session: *mut *mut c_void,
    ) -> SlangResult;
    pub type Release = unsafe extern "C" fn(this: *mut c_void) -> u32;
    pub type GetBufferPointer = unsafe extern "C" fn(this: *mut c_void) -> *const c_void;
    pub type GetBufferSize = unsafe extern "C" fn(this: *mut c_void) -> usize;
    pub type CreateSession = unsafe extern "C" fn(
        this: *mut c_void,
        desc: *const SessionDesc,
        out_session: *mut *mut c_void,
    ) -> SlangResult;
    pub type FindProfile = unsafe extern "C" fn(this: *mut c_void, name: *const c_char) -> u32;
    pub type GetBuildTagString = unsafe extern "C" fn(this: *mut c_void) -> *const c_char;
    pub type LoadModuleFromSourceString = unsafe extern "C" fn(
        this: *mut c_void,
        module_name: *const c_char,
        path: *const c_char,
        source: *const c_char,
        out_diagnostics: *mut *mut c_void,
    ) -> *mut c_void;
    pub type FindAndCheckEntryPoint = unsafe extern "C" fn(
        this: *mut c_void,
        name: *const c_char,
        stage: SlangStage,
        out_entry_point: *mut *mut c_void,
        out_diagnostics: *mut *mut c_void,
    ) -> SlangResult;
    pub type CreateCompositeComponentType = unsafe extern "C" fn(
        this: *mut c_void,
        component_types: *const *mut c_void,
        component_type_count: SlangInt,
        out_composite: *mut *mut c_void,
        out_diagnostics: *mut *mut c_void,
    ) -> SlangResult;
    pub type Link = unsafe extern "C" fn(
        this: *mut c_void,
        out_linked: *mut *mut c_void,
        out_diagnostics: *mut *mut c_void,
    ) -> SlangResult;
    pub type GetEntryPointCode = unsafe extern "C" fn(
        this: *mut c_void,
        entry_point_index: SlangInt,
        target_index: SlangInt,
        out_code: *mut *mut c_void,
        out_diagnostics: *mut *mut c_void,
    ) -> SlangResult;

    #[repr(C)]
    pub struct SlangGlobalSessionDesc {
        pub structure_size: u32,
        pub api_version: u32,
        pub min_language_version: u32,
        pub enable_glsl: bool,
        pub reserved: [u32; 16],
    }

    impl Default for SlangGlobalSessionDesc {
        fn default() -> Self {
            Self {
                structure_size: std::mem::size_of::<Self>() as u32,
                api_version: 0,
                min_language_version: 2025,
                enable_glsl: false,
                reserved: [0; 16],
            }
        }
    }

    #[repr(i32)]
    #[derive(Clone, Copy)]
    pub enum CompilerOptionName {
        DebugInformation = 44,
        Optimization = 46,
        VulkanUseEntryPointName = 52,
    }

    #[repr(C)]
    pub struct CompilerOptionValue {
        pub kind: i32,
        pub int_value_0: i32,
        pub int_value_1: i32,
        pub string_value_0: *const c_char,
        pub string_value_1: *const c_char,
    }

    #[repr(C)]
    pub struct CompilerOptionEntry {
        pub name: CompilerOptionName,
        pub value: CompilerOptionValue,
    }

    const fn integer_option(name: CompilerOptionName, value: i32) -> CompilerOptionEntry {
        CompilerOptionEntry {
            name,
            value: CompilerOptionValue {
                kind: 0,
                int_value_0: value,
                int_value_1: 0,
                string_value_0: std::ptr::null(),
                string_value_1: std::ptr::null(),
            },
        }
    }

    /// Optimisation at Slang's default level, no debug information, and the
    /// entry point's own name in the module.
    ///
    /// Slang keeps binding names without debug information, and the engine's
    /// SPIR-V reflection refuses the `NonSemantic.Shader.DebugInfo`
    /// instructions debug information adds.
    pub fn engine_compiler_option_entries() -> [CompilerOptionEntry; 3] {
        const SLANG_DEBUG_INFO_LEVEL_NONE: i32 = 0;
        const SLANG_OPTIMIZATION_LEVEL_DEFAULT: i32 = 1;
        [
            integer_option(
                CompilerOptionName::DebugInformation,
                SLANG_DEBUG_INFO_LEVEL_NONE,
            ),
            integer_option(
                CompilerOptionName::Optimization,
                SLANG_OPTIMIZATION_LEVEL_DEFAULT,
            ),
            integer_option(CompilerOptionName::VulkanUseEntryPointName, 1),
        ]
    }

    #[repr(C)]
    pub struct TargetDesc {
        pub structure_size: usize,
        pub format: i32,
        pub profile: u32,
        pub flags: u32,
        pub floating_point_mode: u32,
        pub line_directive_mode: u32,
        pub force_glsl_scalar_buffer_layout: bool,
        pub compiler_option_entries: *const CompilerOptionEntry,
        pub compiler_option_entry_count: u32,
    }

    impl Default for TargetDesc {
        fn default() -> Self {
            const SLANG_TARGET_FLAG_GENERATE_SPIRV_DIRECTLY: u32 = 1 << 10;
            Self {
                structure_size: std::mem::size_of::<Self>(),
                format: 0,
                profile: 0,
                flags: SLANG_TARGET_FLAG_GENERATE_SPIRV_DIRECTLY,
                floating_point_mode: 0,
                line_directive_mode: 0,
                force_glsl_scalar_buffer_layout: false,
                compiler_option_entries: std::ptr::null(),
                compiler_option_entry_count: 0,
            }
        }
    }

    #[repr(C)]
    pub struct SessionDesc {
        pub structure_size: usize,
        pub targets: *const TargetDesc,
        pub target_count: SlangInt,
        pub flags: u32,
        pub default_matrix_layout_mode: u32,
        pub search_paths: *const *const c_char,
        pub search_path_count: SlangInt,
        pub preprocessor_macros: *const c_void,
        pub preprocessor_macro_count: SlangInt,
        pub file_system: *mut c_void,
        pub enable_effect_annotations: bool,
        pub allow_glsl_syntax: bool,
        pub compiler_option_entries: *const CompilerOptionEntry,
        pub compiler_option_entry_count: u32,
        pub skip_spirv_validation: bool,
    }

    impl Default for SessionDesc {
        fn default() -> Self {
            const SLANG_MATRIX_LAYOUT_ROW_MAJOR: u32 = 1;
            Self {
                structure_size: std::mem::size_of::<Self>(),
                targets: std::ptr::null(),
                target_count: 0,
                flags: 0,
                default_matrix_layout_mode: SLANG_MATRIX_LAYOUT_ROW_MAJOR,
                search_paths: std::ptr::null(),
                search_path_count: 0,
                preprocessor_macros: std::ptr::null(),
                preprocessor_macro_count: 0,
                file_system: std::ptr::null_mut(),
                enable_effect_annotations: false,
                allow_glsl_syntax: false,
                compiler_option_entries: std::ptr::null(),
                compiler_option_entry_count: 0,
                skip_spirv_validation: false,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// The sizes `slang.h` 2026.12 gives these structs on a 64-bit
        /// target, measured with `sizeof` against the release's header.
        #[test]
        fn the_descriptor_structs_have_the_header_sizes() {
            assert_eq!(std::mem::size_of::<SlangGlobalSessionDesc>(), 80);
            assert_eq!(std::mem::size_of::<CompilerOptionEntry>(), 40);
            assert_eq!(std::mem::size_of::<TargetDesc>(), 48);
            assert_eq!(std::mem::size_of::<SessionDesc>(), 96);
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
