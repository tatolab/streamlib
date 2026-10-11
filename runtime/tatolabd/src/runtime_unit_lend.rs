// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! The lend `tatolabd` hands its processor interpreters: found relative to its
//! own executable, as a runtime unit lays out `bin/tatolabd` beside its lend,
//! or as the macOS app bundle lays out `Contents/MacOS/tatolabd` beside
//! `Contents/Resources/lend`.

use std::path::{Path, PathBuf};
use streamlib::sdk::processor_interpreter::{
    APP_BUNDLE_CONTENTS_DIRECTORY_NAME, APP_BUNDLE_EXECUTABLE_DIRECTORY_NAME,
    LEND_DIRECTORY_RELATIVE_TO_THE_APP_BUNDLE_CONTENTS,
    LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT,
    PROCESSOR_INTERPRETER_BOOTSTRAP_PATH_IN_THE_LEND_DIRECTORY,
    app_bundle_contents_directory_holding_the_executable_directory,
    lend_directory_in_the_app_bundle_contents, lend_directory_in_the_runtime_unit,
    processor_interpreter_bootstrap_path,
};

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

/// The lend beside `canonical_executable`, canonical: the app bundle's
/// `Contents/Resources/lend` when the executable sits in a bundle's
/// `Contents/MacOS`, else the runtime unit's lend beside its `bin/`. Refused,
/// naming every path looked at, unless one holds the processor-interpreter
/// bootstrap.
pub(crate) fn the_lend_beside_the_executable(
    canonical_executable: &Path,
) -> Result<PathBuf, String> {
    let executable_directory = canonical_executable.parent();
    let lend_directory_in_the_app_bundle = executable_directory
        .and_then(app_bundle_contents_directory_holding_the_executable_directory)
        .map(lend_directory_in_the_app_bundle_contents);
    let lend_directory_in_the_runtime_unit = executable_directory
        .and_then(Path::parent)
        .map(lend_directory_in_the_runtime_unit);
    let lend_directories_looked_at: Vec<PathBuf> = lend_directory_in_the_app_bundle
        .into_iter()
        .chain(lend_directory_in_the_runtime_unit)
        .collect();
    if lend_directories_looked_at.is_empty() {
        return Err(format!(
            "{} is not inside a runtime unit's `bin/`, so there is no lend beside it",
            canonical_executable.display()
        ));
    }
    let Some(lend_directory) = lend_directories_looked_at
        .iter()
        .find(|lend_directory| processor_interpreter_bootstrap_path(lend_directory).is_file())
    else {
        return Err(no_lend_refusal_naming_where_it_looked(
            &lend_directories_looked_at,
        ));
    };
    lend_directory
        .canonicalize()
        .map_err(|cannot_be_canonicalized| {
            format!(
                "the lend at {} cannot be canonicalized: {cannot_be_canonicalized}",
                lend_directory.display()
            )
        })
}

fn no_lend_refusal_naming_where_it_looked(lend_directories_looked_at: &[PathBuf]) -> String {
    let where_it_looked = lend_directories_looked_at
        .iter()
        .map(|lend_directory| lend_directory.display().to_string())
        .collect::<Vec<_>>()
        .join(", nor at ");
    let runtime_unit_shape = format!(
        "tatolabd runs from a runtime unit — `bin/tatolabd` beside \
         `{LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT}/`, as `cargo xtask build-runtime` \
         lays one out"
    );
    if lend_directories_looked_at.len() == 1 {
        return format!(
            "no lend at {where_it_looked}: it holds no \
             {PROCESSOR_INTERPRETER_BOOTSTRAP_PATH_IN_THE_LEND_DIRECTORY}. {runtime_unit_shape}"
        );
    }
    format!(
        "no lend at {where_it_looked}: neither holds \
         {PROCESSOR_INTERPRETER_BOOTSTRAP_PATH_IN_THE_LEND_DIRECTORY}. {runtime_unit_shape}, \
         or from an app bundle — \
         `{APP_BUNDLE_CONTENTS_DIRECTORY_NAME}/{APP_BUNDLE_EXECUTABLE_DIRECTORY_NAME}/tatolabd` \
         beside `{APP_BUNDLE_CONTENTS_DIRECTORY_NAME}/\
         {LEND_DIRECTORY_RELATIVE_TO_THE_APP_BUNDLE_CONTENTS}/`"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib::sdk::processor_interpreter::BINARY_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT;

    fn a_runtime_unit_with_its_lend(runtime_unit_root: &Path) -> PathBuf {
        let bootstrap = processor_interpreter_bootstrap_path(&lend_directory_in_the_runtime_unit(
            runtime_unit_root,
        ));
        std::fs::create_dir_all(bootstrap.parent().unwrap()).unwrap();
        std::fs::write(&bootstrap, "").unwrap();
        tatolabd_in_the_runtime_unit(runtime_unit_root)
    }

    fn tatolabd_in_the_runtime_unit(runtime_unit_root: &Path) -> PathBuf {
        let binary_directory =
            runtime_unit_root.join(BINARY_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT);
        std::fs::create_dir_all(&binary_directory).unwrap();
        binary_directory.join("tatolabd")
    }

    #[test]
    fn the_lend_is_found_beside_the_executables_bin_directory() {
        let runtime_unit = tempfile::TempDir::new().unwrap();
        let runtime_unit_root = runtime_unit.path().canonicalize().unwrap();
        let executable = a_runtime_unit_with_its_lend(&runtime_unit_root);

        assert_eq!(
            the_lend_beside_the_executable(&executable).unwrap(),
            runtime_unit_root.join(LEND_DIRECTORY_RELATIVE_TO_THE_RUNTIME_UNIT_ROOT)
        );
    }

    #[test]
    fn an_executable_with_no_lend_beside_it_is_refused_naming_where_it_looked() {
        let not_a_runtime_unit = tempfile::TempDir::new().unwrap();
        let executable = tatolabd_in_the_runtime_unit(not_a_runtime_unit.path());

        let refusal = the_lend_beside_the_executable(&executable).unwrap_err();

        assert!(
            refusal.contains(
                &lend_directory_in_the_runtime_unit(not_a_runtime_unit.path())
                    .display()
                    .to_string()
            ),
            "{refusal}"
        );
        assert!(
            refusal.contains(PROCESSOR_INTERPRETER_BOOTSTRAP_PATH_IN_THE_LEND_DIRECTORY),
            "{refusal}"
        );
    }

    /// An app bundle's `Contents` under `parent_directory`, with `tatolabd`'s
    /// directory made, and the path `tatolabd` would have in it.
    fn tatolabd_in_an_app_bundle(parent_directory: &Path) -> (PathBuf, PathBuf) {
        let app_bundle_contents = parent_directory
            .join("Tatolab.app")
            .join(APP_BUNDLE_CONTENTS_DIRECTORY_NAME);
        let executable_directory = app_bundle_contents.join(APP_BUNDLE_EXECUTABLE_DIRECTORY_NAME);
        std::fs::create_dir_all(&executable_directory).unwrap();
        (app_bundle_contents, executable_directory.join("tatolabd"))
    }

    fn lend_the_app_bundle(app_bundle_contents: &Path) {
        let bootstrap = processor_interpreter_bootstrap_path(
            &lend_directory_in_the_app_bundle_contents(app_bundle_contents),
        );
        std::fs::create_dir_all(bootstrap.parent().unwrap()).unwrap();
        std::fs::write(&bootstrap, "").unwrap();
    }

    #[test]
    fn the_lend_is_found_in_the_resources_of_the_app_bundle_holding_the_executable() {
        let temporary_directory = tempfile::TempDir::new().unwrap();
        let (app_bundle_contents, executable) =
            tatolabd_in_an_app_bundle(&temporary_directory.path().canonicalize().unwrap());
        lend_the_app_bundle(&app_bundle_contents);

        assert_eq!(
            the_lend_beside_the_executable(&executable).unwrap(),
            app_bundle_contents.join(LEND_DIRECTORY_RELATIVE_TO_THE_APP_BUNDLE_CONTENTS)
        );
    }

    #[test]
    fn the_app_bundles_lend_is_preferred_over_a_runtime_unit_shaped_lend_in_contents() {
        let temporary_directory = tempfile::TempDir::new().unwrap();
        let (app_bundle_contents, executable) =
            tatolabd_in_an_app_bundle(&temporary_directory.path().canonicalize().unwrap());
        lend_the_app_bundle(&app_bundle_contents);
        a_runtime_unit_with_its_lend(&app_bundle_contents);

        assert_eq!(
            the_lend_beside_the_executable(&executable).unwrap(),
            lend_directory_in_the_app_bundle_contents(&app_bundle_contents)
        );
    }

    #[test]
    fn an_app_bundle_with_no_lend_is_refused_naming_both_places_it_looked() {
        let temporary_directory = tempfile::TempDir::new().unwrap();
        let (app_bundle_contents, executable) =
            tatolabd_in_an_app_bundle(temporary_directory.path());

        let refusal = the_lend_beside_the_executable(&executable).unwrap_err();

        for place_looked_at in [
            lend_directory_in_the_app_bundle_contents(&app_bundle_contents),
            lend_directory_in_the_runtime_unit(&app_bundle_contents),
        ] {
            assert!(
                refusal.contains(&place_looked_at.display().to_string()),
                "{refusal}"
            );
        }
        assert!(
            refusal.contains(PROCESSOR_INTERPRETER_BOOTSTRAP_PATH_IN_THE_LEND_DIRECTORY),
            "{refusal}"
        );
    }

    #[test]
    fn a_macos_directory_outside_contents_is_not_an_app_bundle() {
        let not_an_app_bundle = tempfile::TempDir::new().unwrap();
        let executable_directory = not_an_app_bundle
            .path()
            .join("Elsewhere")
            .join(APP_BUNDLE_EXECUTABLE_DIRECTORY_NAME);
        std::fs::create_dir_all(&executable_directory).unwrap();

        let refusal =
            the_lend_beside_the_executable(&executable_directory.join("tatolabd")).unwrap_err();

        assert!(
            !refusal.contains(LEND_DIRECTORY_RELATIVE_TO_THE_APP_BUNDLE_CONTENTS),
            "{refusal}"
        );
        assert!(refusal.contains("it holds no"), "{refusal}");
    }

    #[test]
    fn a_lend_directory_without_the_bootstrap_is_refused() {
        let runtime_unit = tempfile::TempDir::new().unwrap();
        let lend_directory = lend_directory_in_the_runtime_unit(runtime_unit.path());
        std::fs::create_dir_all(
            processor_interpreter_bootstrap_path(&lend_directory)
                .parent()
                .unwrap(),
        )
        .unwrap();

        let refusal =
            the_lend_beside_the_executable(&tatolabd_in_the_runtime_unit(runtime_unit.path()))
                .unwrap_err();

        assert!(refusal.contains("no lend at"), "{refusal}");
    }
}
