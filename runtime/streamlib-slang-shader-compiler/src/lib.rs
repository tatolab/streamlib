// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The Slang shader compiler, opened by path: one global session compiles
//! Slang source text for one pipeline stage.
//!
//! The engine compiles its kernels' sources through it at run time, and the
//! build scripts compile the engine's own shaders through it for
//! `include_bytes!`, with the same settings.

use std::ffi::{CStr, CString, c_void};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

mod slang_compiler_abi;

/// What an unspecified entry point means, on either shader path.
pub const DEFAULT_SHADER_ENTRY_POINT: &str = "main";

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

/// Why the Slang compiler would not open or would not compile a source, in
/// words for the author.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlangShaderCompilerRefusal(String);

impl SlangShaderCompilerRefusal {
    /// The refusal's text.
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SlangShaderCompilerRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SlangShaderCompilerRefusal {}

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

/// An opened Slang compiler library and its global session, which compiles
/// every source handed to it.
///
/// A global session, and every session and module it creates, is not
/// thread-safe: the holder compiles through it from one thread at a time.
pub struct SlangCompilerGlobalSession {
    global_session: SlangComObject,
    compiler_version: String,
    _slang_compiler_library: libloading::Library,
}

// SAFETY: the session is moved between threads only whole, and `compile`
// takes it by reference; its holder keeps two threads from compiling at once.
unsafe impl Send for SlangCompilerGlobalSession {}

impl std::fmt::Debug for SlangCompilerGlobalSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SlangCompilerGlobalSession")
            .field("compiler_version", &self.compiler_version)
            .finish_non_exhaustive()
    }
}

impl SlangCompilerGlobalSession {
    /// Open the first of `candidate_paths` that holds a Slang compiler
    /// library, naming every path tried when none does.
    pub fn open_the_first_library_of(
        candidate_paths: &[PathBuf],
    ) -> Result<Self, SlangShaderCompilerRefusal> {
        let mut refusal_per_candidate = Vec::new();
        for candidate_path in candidate_paths {
            // SAFETY: opening the Slang compiler runs only its own initialisers.
            match unsafe { libloading::Library::new(candidate_path) } {
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
        Err(SlangShaderCompilerRefusal(format!(
            "the Slang shader compiler library is not where the runtime carries it, so no \
             shader source can be compiled; a development checkout stages it with \
             `scripts/stage_slang_compiler_libraries.sh`. Looked in:\n  {}",
            refusal_per_candidate.join("\n  ")
        )))
    }

    /// The compiler as a cache key names it: `slang <build tag>`.
    pub fn compiler_version(&self) -> &str {
        &self.compiler_version
    }

    fn create_global_session_from(
        slang_compiler_library: libloading::Library,
    ) -> Result<Self, SlangShaderCompilerRefusal> {
        // SAFETY: the symbol is Slang's exported C entry point, with this signature.
        let create_global_session = unsafe {
            slang_compiler_library
                .get::<slang_compiler_abi::CreateGlobalSession2>(b"slang_createGlobalSession2\0")
        }
        .map_err(|e| {
            SlangShaderCompilerRefusal(format!(
                "the Slang compiler library exports no global session: {e}"
            ))
        })?;
        let global_session_desc = slang_compiler_abi::SlangGlobalSessionDesc::default();
        let mut raw_global_session: *mut c_void = std::ptr::null_mut();
        // SAFETY: both pointers are valid for the call; the out pointer receives
        // one reference the returned object owns.
        let result =
            unsafe { create_global_session(&global_session_desc, &mut raw_global_session) };
        let global_session = SlangComObject::from_owned(raw_global_session).ok_or_else(|| {
            SlangShaderCompilerRefusal(format!(
                "Slang would not create its global session (result {result:#x})"
            ))
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

    /// Compile `request`'s source in a session of its own, so no module it
    /// loads outlives the compile.
    pub fn compile(
        &self,
        request: &SlangShaderSourceToCompile<'_>,
    ) -> Result<Vec<u8>, SlangShaderCompilerRefusal> {
        let SlangShaderSourceToCompile {
            source,
            stage,
            backend,
            entry_point,
            label,
            module_search_directories,
            preprocessor_macros,
        } = *request;
        let compile_failure = |what: &str, diagnostics: Option<String>| {
            SlangShaderCompilerRefusal(format!(
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
        let module_search_directory_texts = module_search_directories
            .iter()
            .map(|directory| {
                CString::new(directory.to_string_lossy().into_owned()).map_err(|_| {
                    compile_failure(
                        &format!(
                            "because its search directory {} holds a NUL byte",
                            directory.display()
                        ),
                        None,
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let module_search_directory_pointers: Vec<*const std::ffi::c_char> =
            module_search_directory_texts
                .iter()
                .map(|directory| directory.as_ptr())
                .collect();
        let preprocessor_macro_texts = preprocessor_macros
            .iter()
            .map(|preprocessor_macro| {
                Ok((
                    CString::new(preprocessor_macro.name)?,
                    CString::new(preprocessor_macro.value)?,
                ))
            })
            .collect::<Result<Vec<_>, std::ffi::NulError>>()
            .map_err(|_| compile_failure("because a preprocessor macro holds a NUL byte", None))?;
        let preprocessor_macro_descs: Vec<slang_compiler_abi::PreprocessorMacroDesc> =
            preprocessor_macro_texts
                .iter()
                .map(|(name, value)| slang_compiler_abi::PreprocessorMacroDesc {
                    name: name.as_ptr(),
                    value: value.as_ptr(),
                })
                .collect();

        // SAFETY: the global session is valid and its holder keeps other threads out.
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
            search_paths: module_search_directory_pointers.as_ptr(),
            search_path_count: module_search_directory_pointers.len()
                as slang_compiler_abi::SlangInt,
            preprocessor_macros: preprocessor_macro_descs.as_ptr(),
            preprocessor_macro_count: preprocessor_macro_descs.len()
                as slang_compiler_abi::SlangInt,
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

/// Open the Slang compiler the workspace rooted at `workspace_root` stages.
pub fn open_the_workspace_staged_slang_compiler(
    workspace_root: &Path,
) -> Result<SlangCompilerGlobalSession, SlangShaderCompilerRefusal> {
    SlangCompilerGlobalSession::open_the_first_library_of(&[
        workspace_staged_slang_compiler_library(workspace_root),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn the_workspace_staged_slang_compiler() -> SlangCompilerGlobalSession {
        open_the_workspace_staged_slang_compiler(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."),
        )
        .expect("the workspace stages the Slang compiler")
    }

    #[test]
    fn a_preprocessor_macro_reaches_the_source() {
        let source = "RWStructuredBuffer<uint> written_values;\n\
                      [numthreads(1, 1, 1)] void main() { written_values[0] = WRITTEN_VALUE; }\n";
        let compiler = the_workspace_staged_slang_compiler();
        let without_the_macro = compiler.compile(&SlangShaderSourceToCompile::for_spirv(
            source,
            ShaderCompilationTargetStage::Compute,
            DEFAULT_SHADER_ENTRY_POINT,
            "macro.slang",
        ));
        assert!(
            without_the_macro.is_err(),
            "an undefined macro fails to parse"
        );
        compiler
            .compile(&SlangShaderSourceToCompile {
                preprocessor_macros: &[SlangPreprocessorMacro {
                    name: "WRITTEN_VALUE",
                    value: "7u",
                }],
                ..SlangShaderSourceToCompile::for_spirv(
                    source,
                    ShaderCompilationTargetStage::Compute,
                    DEFAULT_SHADER_ENTRY_POINT,
                    "macro.slang",
                )
            })
            .expect("the macro defines what the source reads");
    }

    #[test]
    fn an_import_is_found_in_a_module_search_directory() {
        let module_directory = std::env::temp_dir().join(format!(
            "slang-module-search-directory-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&module_directory).unwrap();
        std::fs::write(
            module_directory.join("doubling.slang"),
            "uint doubled(uint value) { return value * 2u; }\n",
        )
        .unwrap();
        let source = "import doubling;\nRWStructuredBuffer<uint> written_values;\n\
                      [numthreads(1, 1, 1)] void main() { written_values[0] = doubled(3u); }\n";
        let compiler = the_workspace_staged_slang_compiler();
        let compiled = compiler.compile(&SlangShaderSourceToCompile {
            module_search_directories: std::slice::from_ref(&module_directory),
            ..SlangShaderSourceToCompile::for_spirv(
                source,
                ShaderCompilationTargetStage::Compute,
                DEFAULT_SHADER_ENTRY_POINT,
                "import.slang",
            )
        });
        std::fs::remove_dir_all(&module_directory).unwrap();
        assert!(compiled.is_ok(), "{compiled:?}");
    }
}
