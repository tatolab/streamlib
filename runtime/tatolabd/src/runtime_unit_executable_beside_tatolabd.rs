// Copyright (c) 2025 Tatolab LLC
// SPDX-License-Identifier: GPL-3.0-only

//! The executables the runtime unit carries beside `tatolabd` — the local
//! relay, uv, and the TypeScript compiler and linter a project's views and
//! controls are checked with — as the unit lays them out in `bin/` and the
//! macOS app bundle in `Contents/MacOS`.

use std::path::{Path, PathBuf};

/// The executable `executable_file_name` beside this one, canonical, or why it
/// is not there.
pub(crate) fn runtime_unit_executable_beside_this_one(
    executable_file_name: &str,
) -> Result<PathBuf, String> {
    let this_executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|failure| format!("this executable's own path cannot be named: {failure}"))?;
    runtime_unit_executable_beside(&this_executable, executable_file_name)
}

/// The programs a project's views and controls are checked with, each relative
/// to the directory `tatolabd` sits in.
const RUNTIME_UNIT_CODE_CHECKERS: [&str; 2] = [
    streamlib::sdk::node_interpreter::RUNTIME_UNIT_TYPESCRIPT_COMPILER_RELATIVE_TO_THE_BINARY_DIRECTORY,
    streamlib::sdk::node_interpreter::RUNTIME_UNIT_LINTER_EXECUTABLE_FILE_NAME,
];

/// Why each program a project's views and controls are checked with is not
/// beside this executable, naming the program.
pub(crate) fn code_checkers_the_runtime_unit_does_not_carry() -> Vec<String> {
    RUNTIME_UNIT_CODE_CHECKERS
        .into_iter()
        .filter_map(|code_checker| runtime_unit_executable_beside_this_one(code_checker).err())
        .collect()
}

/// The executable `executable_file_name` beside `canonical_executable`, or why
/// it is not there.
fn runtime_unit_executable_beside(
    canonical_executable: &Path,
    executable_file_name: &str,
) -> Result<PathBuf, String> {
    let executable = canonical_executable
        .parent()
        .map(|directory| directory.join(executable_file_name))
        .ok_or_else(|| {
            format!(
                "{} sits in no directory to find {executable_file_name} in",
                canonical_executable.display()
            )
        })?;
    if !executable.is_file() {
        return Err(format!(
            "the runtime unit carries no {}; rebuild it with `cargo xtask build-runtime`",
            executable.display()
        ));
    }
    Ok(executable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_executable_is_found_in_the_directory_tatolabd_sits_in() {
        let runtime_unit_bin = tempfile::tempdir().unwrap();
        let tatolabd = runtime_unit_bin.path().join("tatolabd");
        std::fs::write(runtime_unit_bin.path().join("uv"), "uv").unwrap();

        assert_eq!(
            runtime_unit_executable_beside(&tatolabd, "uv"),
            Ok(runtime_unit_bin.path().join("uv"))
        );
    }

    #[test]
    fn the_typescript_compiler_is_found_in_its_directory_beside_tatolabd() {
        let runtime_unit_bin = tempfile::tempdir().unwrap();
        let tatolabd = runtime_unit_bin.path().join("tatolabd");
        std::fs::create_dir_all(runtime_unit_bin.path().join("typescript")).unwrap();
        std::fs::write(runtime_unit_bin.path().join("typescript/tsc"), "tsc").unwrap();

        assert_eq!(
            runtime_unit_executable_beside(&tatolabd, RUNTIME_UNIT_CODE_CHECKERS[0]),
            Ok(runtime_unit_bin.path().join("typescript/tsc"))
        );
        let refusal = runtime_unit_executable_beside(&tatolabd, RUNTIME_UNIT_CODE_CHECKERS[1])
            .expect_err("no linter sits beside it");
        assert!(refusal.contains("oxlint"), "{refusal}");
    }

    #[test]
    fn a_unit_without_the_executable_is_named_with_how_to_rebuild_it() {
        let runtime_unit_bin = tempfile::tempdir().unwrap();

        let refusal =
            runtime_unit_executable_beside(&runtime_unit_bin.path().join("tatolabd"), "moq-relay")
                .expect_err("no relay sits beside it");

        assert!(
            refusal.contains(
                &runtime_unit_bin
                    .path()
                    .join("moq-relay")
                    .display()
                    .to_string()
            ),
            "{refusal}"
        );
        assert!(refusal.contains("cargo xtask build-runtime"), "{refusal}");
    }
}
