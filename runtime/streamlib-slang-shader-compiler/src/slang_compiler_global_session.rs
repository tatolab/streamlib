// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::{CStr, CString, c_void};
use std::path::PathBuf;
use std::ptr::NonNull;

use crate::slang_com_object::{SlangComObject, vtable_method};
use crate::slang_compiler_abi;
use crate::{SlangShaderCompilerRefusal, SlangShaderSourceToCompile};

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
