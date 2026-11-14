// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: GPL-3.0-only

//! The part of Slang's C++ ABI (`slang.h`, release 2026.12) the compiler
//! calls: COM vtable slots, counted from `ISlangUnknown`'s three, and the
//! descriptor structs with their header defaults.

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
pub struct PreprocessorMacroDesc {
    pub name: *const c_char,
    pub value: *const c_char,
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
    pub preprocessor_macros: *const PreprocessorMacroDesc,
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
        assert_eq!(std::mem::size_of::<PreprocessorMacroDesc>(), 16);
    }
}
