// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The uv `tatolabd` makes every project's `.venv` with: beside its own
//! executable, as the runtime unit lays out `bin/uv` beside `bin/tatolabd` and
//! the macOS app bundle lays out `Contents/MacOS/uv` beside
//! `Contents/MacOS/tatolabd`.

use std::path::{Path, PathBuf};

use streamlib::sdk::processor_interpreter::RUNTIME_UNIT_UV_EXECUTABLE_FILE_NAME;

/// `uv` beside this executable, canonical, or why it is not there.
pub(crate) fn the_uv_executable_beside_this_one() -> Result<PathBuf, String> {
    let this_executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|failure| format!("this executable's own path cannot be named: {failure}"))?;
    the_uv_executable_beside(&this_executable)
}

/// `uv` beside `canonical_executable`, or why it is not there.
pub(crate) fn the_uv_executable_beside(canonical_executable: &Path) -> Result<PathBuf, String> {
    let uv_executable = canonical_executable
        .parent()
        .map(|directory| directory.join(RUNTIME_UNIT_UV_EXECUTABLE_FILE_NAME))
        .ok_or_else(|| {
            format!(
                "{} sits in no directory to find uv in",
                canonical_executable.display()
            )
        })?;
    if !uv_executable.is_file() {
        return Err(format!(
            "the runtime unit carries no {}; rebuild it with `cargo xtask build-runtime`",
            uv_executable.display()
        ));
    }
    Ok(uv_executable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uv_is_found_in_the_directory_tatolabd_sits_in() {
        let runtime_unit_bin = tempfile::tempdir().unwrap();
        let tatolabd = runtime_unit_bin.path().join("tatolabd");
        std::fs::write(runtime_unit_bin.path().join("uv"), "uv").unwrap();

        assert_eq!(
            the_uv_executable_beside(&tatolabd),
            Ok(runtime_unit_bin.path().join("uv"))
        );
    }

    #[test]
    fn a_unit_without_uv_is_named_with_how_to_rebuild_it() {
        let runtime_unit_bin = tempfile::tempdir().unwrap();

        let refusal = the_uv_executable_beside(&runtime_unit_bin.path().join("tatolabd"))
            .expect_err("no uv sits beside it");

        assert!(
            refusal.contains(&runtime_unit_bin.path().join("uv").display().to_string()),
            "{refusal}"
        );
        assert!(refusal.contains("cargo xtask build-runtime"), "{refusal}");
    }
}
