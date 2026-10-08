// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Keeps `tatolab` a PEP 420 namespace package, in the tree and in the lend.
//!
//! A processor interpreter finds `tatolab.runtime` in the runtime unit's lend
//! directory and `tatolab.stream` in the stream's own venv. Python merges the
//! two `tatolab/` directories only while neither holds an `__init__.py`: one
//! regular `tatolab` package anywhere on the path shadows every other portion,
//! and the interpreter then cannot import whichever half it hid.
//!
//! Two scan roots. The repository's files, through `git ls-files` like every
//! other gate. And the lend `cargo xtask build-runtime` lays out under
//! `target/`, which git ignores, so it is walked directly when it exists.

use crate::build_runtime::runtime_unit_lend_directory_in_the_workspace;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// The file whose presence turns the shared `tatolab` namespace into one package.
const TATOLAB_NAMESPACE_PACKAGE_INIT_RELATIVE_PATH: &str = "tatolab/__init__.py";

/// Every `tatolab/__init__.py` this gate found, by scan root.
#[derive(Debug, Default)]
pub struct TatolabNamespacePackageInitScanReport {
    /// Repository-relative paths, from `git ls-files`.
    pub repository_paths_holding_an_init: Vec<String>,
    /// Paths under the lend, as walked from the workspace root.
    pub lend_paths_holding_an_init: Vec<PathBuf>,
    /// How many repository files the scan read, so an empty listing fails the gate.
    pub repository_files_scanned: usize,
}

/// Run the gate over the repository and, when one is built, the lend.
pub fn run(workspace_root: &Path) -> Result<()> {
    let report = scan(workspace_root)?;

    crate::ensure_source_walking_gate_read_source(
        "check-no-tatolab-namespace-package-init",
        "the repository",
        report.repository_files_scanned,
        "a `tatolab/__init__.py` to shadow every other `tatolab.*` portion",
    )?;

    let mut failure_lines: Vec<String> = Vec::new();
    for repository_path in &report.repository_paths_holding_an_init {
        failure_lines.push(format!(
            "{repository_path}: makes `tatolab` a regular package — it must stay a PEP 420 \
             namespace, or the lent `tatolab.runtime` and the venv's `tatolab.stream` stop \
             merging"
        ));
    }
    for lend_path in &report.lend_paths_holding_an_init {
        failure_lines.push(format!(
            "{}: the runtime unit's lend holds a `tatolab/__init__.py` — every processor \
             interpreter borrowing it would lose the venv's `tatolab.stream`",
            lend_path.display()
        ));
    }

    anyhow::ensure!(
        failure_lines.is_empty(),
        "check-no-tatolab-namespace-package-init found {} violation(s):\n{}",
        failure_lines.len(),
        failure_lines.join("\n"),
    );

    tracing::info!(
        "check-no-tatolab-namespace-package-init: {} repository files scanned, no tatolab/__init__.py",
        report.repository_files_scanned,
    );
    Ok(())
}

/// Find every `tatolab/__init__.py` in the repository's files and in the lend.
pub fn scan(workspace_root: &Path) -> Result<TatolabNamespacePackageInitScanReport> {
    let repository_paths = crate::list_repository_files_under(workspace_root, ".")?;
    let repository_paths_holding_an_init = repository_paths
        .iter()
        .filter(|repository_path| is_a_tatolab_namespace_package_init(Path::new(repository_path)))
        .cloned()
        .collect();

    let lend_directory = runtime_unit_lend_directory_in_the_workspace(workspace_root);
    let lend_paths_holding_an_init = if lend_directory.exists() {
        tatolab_namespace_package_inits_under_lend_directory(&lend_directory)?
    } else {
        Vec::new()
    };

    Ok(TatolabNamespacePackageInitScanReport {
        repository_paths_holding_an_init,
        lend_paths_holding_an_init,
        repository_files_scanned: repository_paths.len(),
    })
}

/// Every `tatolab/__init__.py` anywhere beneath a lend directory.
pub fn tatolab_namespace_package_inits_under_lend_directory(
    lend_directory: &Path,
) -> Result<Vec<PathBuf>> {
    let mut lend_paths_holding_an_init = Vec::new();
    for lend_entry in walkdir::WalkDir::new(lend_directory).sort_by_file_name() {
        let lend_entry = lend_entry
            .with_context(|| format!("walking the lend at {}", lend_directory.display()))?;
        if lend_entry.file_type().is_dir() {
            continue;
        }
        if is_a_tatolab_namespace_package_init(lend_entry.path()) {
            lend_paths_holding_an_init.push(lend_entry.into_path());
        }
    }
    Ok(lend_paths_holding_an_init)
}

/// Refuse a lend holding any `tatolab/__init__.py`, naming each one.
pub fn ensure_lend_directory_keeps_tatolab_a_namespace(lend_directory: &Path) -> Result<()> {
    let lend_paths_holding_an_init =
        tatolab_namespace_package_inits_under_lend_directory(lend_directory)?;
    anyhow::ensure!(
        lend_paths_holding_an_init.is_empty(),
        "the lend at {} holds {} — `tatolab` must stay a PEP 420 namespace package, or every \
         processor interpreter borrowing this lend loses the venv's `tatolab.stream`",
        lend_directory.display(),
        lend_paths_holding_an_init
            .iter()
            .map(|lend_path| lend_path.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
    );
    Ok(())
}

fn is_a_tatolab_namespace_package_init(path: &Path) -> bool {
    path.ends_with(TATOLAB_NAMESPACE_PACKAGE_INIT_RELATIVE_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn workspace_with_namespace_portions() -> tempfile::TempDir {
        let workspace = tempfile::TempDir::new().unwrap();
        write_file(&workspace.path().join(".gitignore"), "target/\n");
        write_file(
            &workspace
                .path()
                .join("sdk/streamlib-python-wheel/python/tatolab/runtime/__init__.py"),
            "",
        );
        write_file(
            &workspace
                .path()
                .join("sdk/tatolab-stream/tatolab/stream/__init__.py"),
            "",
        );
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(workspace.path())
            .status()
            .unwrap();
        workspace
    }

    fn plant_lend(workspace_root: &Path) -> PathBuf {
        let lend_directory = runtime_unit_lend_directory_in_the_workspace(workspace_root);
        write_file(&lend_directory.join("tatolab/runtime/__init__.py"), "");
        write_file(
            &lend_directory.join("streamlib-0.0.0.dist-info/WHEEL"),
            "Wheel-Version: 1.0\n",
        );
        lend_directory
    }

    #[test]
    fn a_tree_and_lend_holding_only_namespace_portions_pass() {
        let workspace = workspace_with_namespace_portions();
        plant_lend(workspace.path());

        run(workspace.path()).unwrap();
    }

    #[test]
    fn a_tree_with_no_lend_built_passes() {
        let workspace = workspace_with_namespace_portions();

        let report = scan(workspace.path()).unwrap();

        assert!(report.lend_paths_holding_an_init.is_empty());
        run(workspace.path()).unwrap();
    }

    #[test]
    fn a_tatolab_init_in_the_tree_is_refused_naming_the_path() {
        let workspace = workspace_with_namespace_portions();
        write_file(
            &workspace
                .path()
                .join("sdk/tatolab-stream/tatolab/__init__.py"),
            "",
        );

        let gate_failure = run(workspace.path()).unwrap_err().to_string();

        assert!(
            gate_failure.contains("sdk/tatolab-stream/tatolab/__init__.py"),
            "{gate_failure}"
        );
    }

    #[test]
    fn a_tatolab_init_at_the_repository_root_is_refused() {
        let workspace = workspace_with_namespace_portions();
        write_file(&workspace.path().join("tatolab/__init__.py"), "");

        let report = scan(workspace.path()).unwrap();

        assert_eq!(
            report.repository_paths_holding_an_init,
            vec!["tatolab/__init__.py".to_owned()]
        );
    }

    #[test]
    fn a_tatolab_init_in_the_lend_is_refused_naming_the_path() {
        let workspace = workspace_with_namespace_portions();
        let lend_directory = plant_lend(workspace.path());
        let planted_init = lend_directory.join("tatolab/__init__.py");
        write_file(&planted_init, "");

        let report = scan(workspace.path()).unwrap();
        let gate_failure = run(workspace.path()).unwrap_err().to_string();

        assert!(report.repository_paths_holding_an_init.is_empty());
        assert_eq!(
            report.lend_paths_holding_an_init,
            vec![planted_init.clone()]
        );
        assert!(
            gate_failure.contains(&planted_init.display().to_string()),
            "{gate_failure}"
        );
    }

    #[test]
    fn a_lend_holding_a_tatolab_init_is_refused_by_the_lend_check_alone() {
        let lend_parent = tempfile::TempDir::new().unwrap();
        let lend_directory = lend_parent.path().join("lend");
        write_file(&lend_directory.join("tatolab/runtime/__init__.py"), "");
        ensure_lend_directory_keeps_tatolab_a_namespace(&lend_directory).unwrap();

        write_file(&lend_directory.join("tatolab/__init__.py"), "");
        let lend_failure = ensure_lend_directory_keeps_tatolab_a_namespace(&lend_directory)
            .unwrap_err()
            .to_string();

        assert!(
            lend_failure.contains(
                &lend_directory
                    .join("tatolab/__init__.py")
                    .display()
                    .to_string()
            ),
            "{lend_failure}"
        );
    }

    #[test]
    fn a_runtime_init_is_not_mistaken_for_the_namespace_init() {
        assert!(!is_a_tatolab_namespace_package_init(Path::new(
            "python/tatolab/runtime/__init__.py"
        )));
        assert!(!is_a_tatolab_namespace_package_init(Path::new(
            "python/not_tatolab/__init__.py"
        )));
        assert!(is_a_tatolab_namespace_package_init(Path::new(
            "python/tatolab/__init__.py"
        )));
    }
}
