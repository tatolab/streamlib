// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! CI gate reporting `.rs` files under a folder-backed package's `processors/`
//! directory that nothing in the generated module tree names.
//!
//! Compiler-interpreted `cfg` beats a scanner everywhere except one place: a
//! file no `mod` chain names is not "excluded on this target", it is absent
//! from the build entirely. `cargo` and `clippy` both say nothing — verified.
//! An author lands `processors/camera/windows.rs`, forgets the `mod windows;`,
//! and the file sits in the tree compiling nowhere and reviewed by no one.
//!
//! The bar is **declaration** reachability, not compile reachability: a file
//! passes if any `mod` chain rooted at a generated crate-root arm names it,
//! whether or not a build target compiles it. That is forced by the
//! parked-module convention — `packages/*/processors/_apple_impl_pending_/`
//! opens with `#![cfg(any())]` and compiles on no target by design, yet the
//! generated crate root still declares the arm so that unparking is a cfg edit
//! and nothing else. A compile-reachability bar would call all ~15 files under
//! those parked directories orphans.
//!
//! Consequence worth naming: a `mod` behind a predicate no real target
//! satisfies (a typo'd `target_os`) still counts as naming its file. Proving
//! that class needs a real rustc target registry, which the cfg evaluator in
//! `streamlib-processor-extract` does not have — it proves satisfiability over
//! the atoms a predicate mentions, under which a typo'd `target_os` is
//! satisfiable. This gate closes the "nothing names it" hole; the typo hole
//! stays open.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use streamlib_idents::PACKAGE_PROCESSOR_SOURCE_DIR_NAME;
use streamlib_processor_extract::{
    discover_package_dirs_declaring_a_generated_crate_root,
    enumerate_processor_source_module_files_the_crate_names,
};
use walkdir::WalkDir;

/// One `.rs` under a package's `processors/` directory that no `mod` chain in
/// the package's generated module tree names.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct OrphanProcessorSourceFile {
    /// The package whose `processors/` directory holds the file.
    pub package_dir: PathBuf,
    /// The unnamed file.
    pub orphan_file: PathBuf,
}

/// What one sweep of the workspace found, plus how much source it read — a
/// gate that scanned nothing must fail loudly rather than report a clean tree.
#[derive(Debug, Default)]
pub struct ProcessorSourceReachabilityReport {
    pub orphans: Vec<OrphanProcessorSourceFile>,
    pub scanned_processor_source_file_count: usize,
    pub scanned_package_count: usize,
    /// Directories holding `processors/` that this sweep did not cover,
    /// because the package commits its own crate root instead of declaring
    /// the generated one. Reported rather than dropped: a gate that quietly
    /// skips part of its stated scope reads as "covered everything".
    pub processor_source_dirs_outside_the_sweep: Vec<PathBuf>,
}

pub fn run(workspace_root: &Path) -> Result<()> {
    let report = scan_workspace(workspace_root)?;

    crate::ensure_source_walking_gate_read_source(
        "check-processor-source-reachability",
        &format!("every folder-backed package's {PACKAGE_PROCESSOR_SOURCE_DIR_NAME}/"),
        report.scanned_processor_source_file_count,
        "an orphan processor source file sit in the tree",
    )?;

    for skipped in &report.processor_source_dirs_outside_the_sweep {
        println!(
            "  note: {} is not swept — its package commits its own crate root, so its \
             processor source is named by `#[path]` from `src/` rather than by a \
             generated arm.",
            skipped
                .strip_prefix(workspace_root)
                .unwrap_or(skipped)
                .display(),
        );
    }

    if report.orphans.is_empty() {
        println!(
            "✓ check-processor-source-reachability: every one of {} .rs file(s) under \
             {}/ across {} folder-backed package(s) is named by the generated module tree.",
            report.scanned_processor_source_file_count,
            PACKAGE_PROCESSOR_SOURCE_DIR_NAME,
            report.scanned_package_count,
        );
        return Ok(());
    }

    eprintln!(
        "✗ check-processor-source-reachability: {} orphan file(s) — nothing in the \
         generated module tree names them, so cargo and clippy never read them:",
        report.orphans.len()
    );
    for orphan in &report.orphans {
        eprintln!(
            "  {}  (package {})",
            orphan
                .orphan_file
                .strip_prefix(workspace_root)
                .unwrap_or(&orphan.orphan_file)
                .display(),
            orphan
                .package_dir
                .strip_prefix(workspace_root)
                .unwrap_or(&orphan.package_dir)
                .display(),
        );
    }
    eprintln!(
        "\nFix:\n  \
         A top-level `{dir}/<name>.rs` or `{dir}/<name>/mod.rs` becomes an arm \
         automatically, so an orphan is always a nested file no `mod` names.\n  \
         Options:\n    \
           1. Add the missing `mod <name>;` to the `mod.rs` of the directory \
              holding it (the usual cause — a file landed without its \
              declaration).\n    \
           2. If it belongs to a parked platform arm, move it under that arm's \
              directory and declare it from the parked `mod.rs`. Parked source \
              is still named by the crate; `#![cfg(any())]` gates it, it does \
              not hide it.\n    \
           3. If it is genuinely dead, delete it.",
        dir = PACKAGE_PROCESSOR_SOURCE_DIR_NAME,
    );

    anyhow::bail!("check-processor-source-reachability failed");
}

/// Sweep every folder-backed package under `workspace_root`.
pub fn scan_workspace(workspace_root: &Path) -> Result<ProcessorSourceReachabilityReport> {
    let package_dirs = discover_package_dirs_declaring_a_generated_crate_root(workspace_root)
        .with_context(|| {
            format!(
                "discovering folder-backed packages under {}",
                workspace_root.display()
            )
        })?;

    let mut report = ProcessorSourceReachabilityReport {
        scanned_package_count: package_dirs.len(),
        processor_source_dirs_outside_the_sweep: processor_source_dirs_outside(
            workspace_root,
            &package_dirs,
        ),
        ..Default::default()
    };
    for package_dir in &package_dirs {
        scan_package(package_dir, &mut report)?;
    }
    report.orphans.sort();
    Ok(report)
}

fn scan_package(package_dir: &Path, report: &mut ProcessorSourceReachabilityReport) -> Result<()> {
    let processor_source_dir = package_dir.join(PACKAGE_PROCESSOR_SOURCE_DIR_NAME);
    if !processor_source_dir.is_dir() {
        return Ok(());
    }

    let named: BTreeSet<PathBuf> =
        enumerate_processor_source_module_files_the_crate_names(package_dir)
            .with_context(|| {
                format!(
                    "enumerating named module files in {}",
                    package_dir.display()
                )
            })?
            .iter()
            .map(|module_file| comparable_form(module_file))
            .collect();

    for rust_file in rust_files_under(&processor_source_dir) {
        report.scanned_processor_source_file_count += 1;
        if !named.contains(&comparable_form(&rust_file)) {
            report.orphans.push(OrphanProcessorSourceFile {
                package_dir: package_dir.to_path_buf(),
                orphan_file: rust_file,
            });
        }
    }
    Ok(())
}

/// The form two paths to the same file must be reduced to before they can be
/// compared.
///
/// `#[path = "../shared/helper.rs"]` resolves to a path carrying a literal
/// `..` component, while the directory walk yields the already-descended form.
/// `PathBuf` equality is lexical and does not reduce `..`, so without this a
/// correctly declared file reads as an orphan — and the gate would tell the
/// author to move or delete it. Canonicalizing also makes both sides agree
/// through a symlinked directory, which lexical reduction gets wrong.
///
/// Both sides name a file that exists, so the fallback is unreachable in
/// practice; it keeps a racing deletion from turning into a panic.
fn comparable_form(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Every `<dir>/processors` under `workspace_root` belonging to a Cargo
/// package that is not in `swept_package_dirs` — a package that commits its
/// own crate root pulls its processor source in by `#[path]` from `src/`, and
/// this sweep starts from generated arms, so it never sees those files.
fn processor_source_dirs_outside(
    workspace_root: &Path,
    swept_package_dirs: &[PathBuf],
) -> Vec<PathBuf> {
    let swept: BTreeSet<&Path> = swept_package_dirs.iter().map(PathBuf::as_path).collect();
    let mut outside: Vec<PathBuf> = WalkDir::new(workspace_root)
        .into_iter()
        // Depth 0 is the workspace root itself, whose own name is not the
        // sweep's business — a root under a dot-directory (or a `/tmp/.tmpXXX`
        // fixture) would otherwise prune the entire walk to nothing.
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            entry.depth() == 0
                || (name != "target" && name != "node_modules" && !name.starts_with('.'))
        })
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_type().is_dir() && entry.file_name() == PACKAGE_PROCESSOR_SOURCE_DIR_NAME
        })
        .map(|entry| entry.into_path())
        .filter(|processor_source_dir| {
            let Some(package_dir) = processor_source_dir.parent() else {
                return false;
            };
            package_dir.join("Cargo.toml").is_file() && !swept.contains(package_dir)
        })
        .collect();
    outside.sort();
    outside
}

/// Every `.rs` under `dir`, recursively. `processors/` is the shared discovery
/// root for every language, so `.py` / `.ts` siblings are simply not this
/// gate's business.
fn rust_files_under(dir: &Path) -> BTreeSet<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("rs"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_processor_extract::generated_crate_root_lib_path_value;

    /// A workspace holding exactly one folder-backed package, so a scan's
    /// findings are attributable to the files the test wrote and nothing else.
    fn workspace_with_one_folder_backed_package(files: &[(&str, &str)]) -> tempfile::TempDir {
        let workspace = tempfile::tempdir().expect("tempdir");
        let package_dir = workspace.path().join("packages/fixture");
        std::fs::create_dir_all(&package_dir).expect("create package dir");
        std::fs::write(
            package_dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n\n\
                 [lib]\npath = \"{}\"\n",
                generated_crate_root_lib_path_value()
            ),
        )
        .expect("write Cargo.toml");

        for (relative_path, body) in files {
            let path = package_dir.join(relative_path);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("create dirs");
            std::fs::write(path, body).expect("write fixture file");
        }
        workspace
    }

    fn orphan_file_names(report: &ProcessorSourceReachabilityReport) -> Vec<String> {
        report
            .orphans
            .iter()
            .map(|orphan| {
                orphan
                    .orphan_file
                    .file_name()
                    .expect("file name")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    /// The gate's whole reason to exist, and the issue's stated validation
    /// shape: one file nothing declares, one declared only behind a
    /// non-host target. Exactly one failure, and it names the orphan.
    #[test]
    fn an_undeclared_file_is_an_orphan_and_a_non_host_only_file_is_not() {
        let workspace = workspace_with_one_folder_backed_package(&[
            (
                "processors/camera/mod.rs",
                "#[cfg(target_os = \"macos\")]\nmod apple_capture;\n",
            ),
            ("processors/camera/apple_capture.rs", "pub struct Apple;\n"),
            ("processors/camera/forgotten.rs", "pub struct Forgotten;\n"),
        ]);

        let report = scan_workspace(workspace.path()).expect("scan");

        assert_eq!(
            orphan_file_names(&report),
            vec!["forgotten.rs"],
            "the Apple-only file is declared and must pass when linted on \
             any host; only the undeclared file is an orphan"
        );
    }

    /// The parked-module convention: `#![cfg(any())]` compiles on no target by
    /// design, and the generated crate root still declares the arm so that
    /// unparking is a cfg edit and nothing else. A compile-reachability bar
    /// would call every file under a parked directory an orphan — this locks
    /// that it does not.
    #[test]
    fn a_file_declared_inside_a_parked_subtree_is_not_an_orphan() {
        let workspace = workspace_with_one_folder_backed_package(&[
            (
                "processors/_apple_impl_pending_/mod.rs",
                "#![cfg(any())]\nmod parked_camera;\n",
            ),
            (
                "processors/_apple_impl_pending_/parked_camera.rs",
                "pub struct ParkedCamera;\n",
            ),
        ]);

        let report = scan_workspace(workspace.path()).expect("scan");

        assert!(
            report.orphans.is_empty(),
            "parked source is named by the crate, not hidden from it: {:?}",
            orphan_file_names(&report)
        );
        assert_eq!(report.scanned_processor_source_file_count, 2);
    }

    /// A nested file reached through two `mod` hops, one of them a `mod.rs`
    /// directory arm. Without this the gate could pass by only ever resolving
    /// one level and calling everything deeper unreachable — which the
    /// single-hop cases above would not distinguish.
    #[test]
    fn a_file_reached_through_a_multi_hop_mod_chain_is_not_an_orphan() {
        let workspace = workspace_with_one_folder_backed_package(&[
            ("processors/codec/mod.rs", "mod inner;\n"),
            ("processors/codec/inner/mod.rs", "mod leaf;\n"),
            ("processors/codec/inner/leaf.rs", "pub struct Leaf;\n"),
        ]);

        let report = scan_workspace(workspace.path()).expect("scan");

        assert!(
            report.orphans.is_empty(),
            "{:?}",
            orphan_file_names(&report)
        );
        assert_eq!(report.scanned_processor_source_file_count, 3);
    }

    /// A `#[path]`-attributed `mod` names a file the standard search would
    /// never find. Resolving it is why this walk shares the extractor's
    /// resolution primitive instead of re-deriving one.
    #[test]
    fn a_file_named_only_by_a_path_attributed_mod_is_not_an_orphan() {
        let workspace = workspace_with_one_folder_backed_package(&[
            (
                "processors/codec/mod.rs",
                "#[path = \"vendored/impl.rs\"]\nmod vendored_impl;\n",
            ),
            (
                "processors/codec/vendored/impl.rs",
                "pub struct Vendored;\n",
            ),
        ]);

        let report = scan_workspace(workspace.path()).expect("scan");

        assert!(
            report.orphans.is_empty(),
            "{:?}",
            orphan_file_names(&report)
        );
    }

    /// A `#[path]` that climbs out of its own directory resolves to a path
    /// carrying a literal `..`, while the directory walk yields the descended
    /// form. Comparing those lexically calls a correctly declared file an
    /// orphan and tells the author to delete it — the one failure a gate
    /// cannot have.
    #[test]
    fn a_file_named_by_a_parent_relative_path_attribute_is_not_an_orphan() {
        let workspace = workspace_with_one_folder_backed_package(&[
            (
                "processors/codec/mod.rs",
                "#[path = \"../shared/helper.rs\"]\nmod helper;\n",
            ),
            ("processors/shared/mod.rs", "pub struct Shared;\n"),
            ("processors/shared/helper.rs", "pub struct Helper;\n"),
        ]);

        let report = scan_workspace(workspace.path()).expect("scan");

        assert!(
            report.orphans.is_empty(),
            "a `..`-relative #[path] names its file just as much as a plain \
             `mod` does: {:?}",
            orphan_file_names(&report)
        );
    }

    /// An inline `mod foo { ... }` introduces a `foo` directory component for
    /// its children's file resolution. Getting that wrong makes every file
    /// under an inline module an orphan, and no other test would notice.
    #[test]
    fn a_file_declared_from_inside_an_inline_mod_is_not_an_orphan() {
        let workspace = workspace_with_one_folder_backed_package(&[
            ("processors/codec/mod.rs", "mod inner {\n    mod leaf;\n}\n"),
            ("processors/codec/inner/leaf.rs", "pub struct Leaf;\n"),
        ]);

        let report = scan_workspace(workspace.path()).expect("scan");

        assert!(
            report.orphans.is_empty(),
            "{:?}",
            orphan_file_names(&report)
        );
        assert_eq!(report.scanned_processor_source_file_count, 2);
    }

    /// A package that commits its own crate root pulls its processor source in
    /// by `#[path]` from `src/`, which this sweep never starts from. The
    /// directory is reported rather than dropped — a gate that quietly skips
    /// part of its stated scope reads as "covered everything".
    #[test]
    fn a_processors_dir_on_a_package_that_commits_its_own_crate_root_is_reported_as_unswept() {
        let workspace = workspace_with_one_folder_backed_package(&[(
            "processors/blur.rs",
            "pub struct Blur;\n",
        )]);
        let committed = workspace.path().join("packages/committed");
        std::fs::create_dir_all(committed.join("processors")).expect("create dirs");
        std::fs::write(
            committed.join("Cargo.toml"),
            "[package]\nname = \"committed\"\nversion = \"0.1.0\"\n",
        )
        .expect("write Cargo.toml");
        std::fs::write(
            committed.join("processors/api_server.rs"),
            "pub struct ApiServer;\n",
        )
        .expect("write processor");

        let report = scan_workspace(workspace.path()).expect("scan");

        assert!(
            report.orphans.is_empty(),
            "an unswept directory must not be reported as orphans: {:?}",
            orphan_file_names(&report)
        );
        assert_eq!(
            report.processor_source_dirs_outside_the_sweep,
            vec![committed.join("processors")]
        );
    }

    /// A top-level `processors/<name>.rs` becomes an arm with no `mod`
    /// anywhere, so the gate must not demand a declaration for it.
    #[test]
    fn a_top_level_arm_needs_no_mod_declaration() {
        let workspace = workspace_with_one_folder_backed_package(&[(
            "processors/blur.rs",
            "pub struct Blur;\n",
        )]);

        let report = scan_workspace(workspace.path()).expect("scan");

        assert!(
            report.orphans.is_empty(),
            "{:?}",
            orphan_file_names(&report)
        );
        assert_eq!(report.scanned_processor_source_file_count, 1);
    }

    /// A `processors/` subdirectory with no `mod.rs` is not an arm, so nothing
    /// names the Rust source inside it. The extractor only `warn`s about this
    /// today; the set diff makes it a failure.
    #[test]
    fn rust_source_in_a_subdirectory_with_no_mod_rs_is_an_orphan() {
        let workspace = workspace_with_one_folder_backed_package(&[(
            "processors/stray/helper.rs",
            "pub struct Helper;\n",
        )]);

        let report = scan_workspace(workspace.path()).expect("scan");

        assert_eq!(orphan_file_names(&report), vec!["helper.rs"]);
    }

    /// A package with no `processors/` at all is first-class (schema-only), so
    /// it contributes no findings — and, importantly, no files, which is what
    /// `ensure_source_walking_gate_read_source` exists to notice.
    #[test]
    fn a_package_with_no_processor_source_dir_contributes_nothing() {
        let workspace = workspace_with_one_folder_backed_package(&[]);

        let report = scan_workspace(workspace.path()).expect("scan");

        assert!(report.orphans.is_empty());
        assert_eq!(report.scanned_processor_source_file_count, 0);
        assert_eq!(report.scanned_package_count, 1);
    }
}
