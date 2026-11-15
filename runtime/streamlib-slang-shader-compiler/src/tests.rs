// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;

use super::*;

fn the_workspace_staged_slang_compiler() -> SlangCompilerGlobalSession {
    open_the_workspace_staged_slang_compiler(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
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
