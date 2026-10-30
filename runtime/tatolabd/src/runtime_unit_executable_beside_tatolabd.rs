// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The executables the runtime unit carries beside `tatolabd` — the local
//! relay and uv — as the unit lays them out in `bin/` and the macOS app bundle
//! in `Contents/MacOS`.

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
