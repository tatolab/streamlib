// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The lend `tatolabd` hands its processor interpreters: found relative to its
//! own executable, as a runtime unit lays out `bin/tatolabd` beside
//! `lib/tatolab/lend/`.

use std::path::{Path, PathBuf};

/// The lend directory, relative to the directory holding the runtime unit's
/// `bin/`.
const LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT: &str = "lib/tatolab/lend";

/// The file a lend must hold: what every processor interpreter runs by path.
const PROCESSOR_INTERPRETER_BOOTSTRAP_RELATIVE_TO_THE_LEND: &str =
    "tatolab/runtime/_processor_interpreter_bootstrap.py";

/// The lend beside this process's own executable, canonical, or the refusal
/// naming where it was looked for.
pub(crate) fn the_lend_beside_this_executable() -> Result<PathBuf, String> {
    let this_executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|cannot_name_this_executable| {
            format!(
                "cannot name this executable's own path, so there is no lend to find beside it: \
                 {cannot_name_this_executable}"
            )
        })?;
    the_lend_beside_the_executable(&this_executable)
}

/// The lend a runtime unit lays out beside `canonical_executable`'s `bin/`,
/// canonical, refused naming the path looked at unless it holds the
/// processor-interpreter bootstrap.
pub(crate) fn the_lend_beside_the_executable(
    canonical_executable: &Path,
) -> Result<PathBuf, String> {
    let Some(lend_directory) =
        canonical_executable
            .parent()
            .and_then(Path::parent)
            .map(|runtime_unit_root| {
                runtime_unit_root.join(LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT)
            })
    else {
        return Err(format!(
            "{} is not inside a runtime unit's `bin/`, so there is no lend beside it",
            canonical_executable.display()
        ));
    };
    let processor_interpreter_bootstrap =
        lend_directory.join(PROCESSOR_INTERPRETER_BOOTSTRAP_RELATIVE_TO_THE_LEND);
    if !processor_interpreter_bootstrap.is_file() {
        return Err(format!(
            "no lend at {}: it holds no {PROCESSOR_INTERPRETER_BOOTSTRAP_RELATIVE_TO_THE_LEND}. \
             tatolabd runs from a runtime unit — `bin/tatolabd` beside \
             `{LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT}/`, as `cargo xtask \
             build-runtime` lays one out",
            lend_directory.display()
        ));
    }
    lend_directory
        .canonicalize()
        .map_err(|cannot_be_canonicalized| {
            format!(
                "the lend at {} cannot be canonicalized: {cannot_be_canonicalized}",
                lend_directory.display()
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_runtime_unit_with_its_lend(runtime_unit_root: &Path) -> PathBuf {
        let bootstrap = runtime_unit_root
            .join(LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT)
            .join(PROCESSOR_INTERPRETER_BOOTSTRAP_RELATIVE_TO_THE_LEND);
        std::fs::create_dir_all(bootstrap.parent().unwrap()).unwrap();
        std::fs::write(&bootstrap, "").unwrap();
        std::fs::create_dir_all(runtime_unit_root.join("bin")).unwrap();
        runtime_unit_root.join("bin/tatolabd")
    }

    #[test]
    fn the_lend_is_found_beside_the_executables_bin_directory() {
        let runtime_unit = tempfile::TempDir::new().unwrap();
        let runtime_unit_root = runtime_unit.path().canonicalize().unwrap();
        let executable = a_runtime_unit_with_its_lend(&runtime_unit_root);

        assert_eq!(
            the_lend_beside_the_executable(&executable).unwrap(),
            runtime_unit_root.join("lib/tatolab/lend")
        );
    }

    #[test]
    fn an_executable_with_no_lend_beside_it_is_refused_naming_where_it_looked() {
        let not_a_runtime_unit = tempfile::TempDir::new().unwrap();
        let executable = not_a_runtime_unit.path().join("bin/tatolabd");

        let refusal = the_lend_beside_the_executable(&executable).unwrap_err();

        assert!(
            refusal.contains(
                &not_a_runtime_unit
                    .path()
                    .join("lib/tatolab/lend")
                    .display()
                    .to_string()
            ),
            "{refusal}"
        );
        assert!(
            refusal.contains("_processor_interpreter_bootstrap.py"),
            "{refusal}"
        );
    }

    #[test]
    fn a_lend_directory_without_the_bootstrap_is_refused() {
        let runtime_unit = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(runtime_unit.path().join("lib/tatolab/lend/tatolab/runtime"))
            .unwrap();

        let refusal =
            the_lend_beside_the_executable(&runtime_unit.path().join("bin/tatolabd")).unwrap_err();

        assert!(refusal.contains("no lend at"), "{refusal}");
    }
}
