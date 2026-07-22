// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Compile a just-placed `streamlib_modules/@org/name/` slot on-the-box.
//!
//! `add` / `install` place a package's bytes into its co-located
//! `streamlib_modules/@org/name/` slot (the pure [`AppModulesDir`] twin never
//! builds), then hand the slot here to compile it IN PLACE through the injected
//! [`BuildOrchestrator`]: the build source IS the destination slot, so the
//! orchestrator promotes only the regenerated build-output units
//! (`lib/<triple>/`, `.venv/`, `_generated_/`) beside the untouched sources.
//!
//! Provenance is [`PackageSourceProvenance::ImmutableManagedExtract`]: an
//! added / reproduced copy is a self-contained managed extract, not the user's
//! editable checkout, so cargo's build-once-reuse (zero-cargo second load) and
//! build-scratch reclamation both apply. A dev `streamlib link` slot is
//! deliberately NOT routed here — a linked checkout is a mutable user tree that
//! stays lazy/edit-rebuild at run time.
//!
//! Failure posture differs by command:
//! - `add` rolls the placement back (folder + lock entry) so a non-compiling
//!   package leaves no residue.
//! - `install` reproduces rather than acquires, so it does NOT roll back;
//!   it aggregates every broken package and fails listing them all.
//!
//! [`AppModulesDir`]: streamlib::sdk::runtime::AppModulesDir

use std::path::Path;

use anyhow::Result;
use streamlib::sdk::PolyglotBuildOrchestrator;
use streamlib::sdk::runtime::{
    AddPackageReport, AppModulesDir, BuildEvent, BuildEventSink, BuildOrchestrator, BuildPolicy,
    BuildRequest, BuildSource, BuildStream, InstallFromLockfileReport, InstalledFromLockKind,
    PackageSourceProvenance, StagedArtifact, host_target_triple,
};
use streamlib_idents::PackageRef;

/// The default in-process polyglot build orchestrator the CLI wires for
/// compile-on-place.
pub fn default_orchestrator() -> PolyglotBuildOrchestrator {
    PolyglotBuildOrchestrator::default()
}

/// Compile one placed slot in place. `source` and `staging_destination_slot_dir`
/// are both `package_dir`, which the orchestrator detects as an in-place
/// destination (promote build outputs beside the untouched sources).
pub fn build_placed_slot(
    orchestrator: &dyn BuildOrchestrator,
    sink: &dyn BuildEventSink,
    package: &PackageRef,
    package_dir: &Path,
    policy: BuildPolicy,
) -> Result<StagedArtifact> {
    let request = BuildRequest {
        package: package.clone(),
        source: BuildSource::PackageDir(package_dir.to_path_buf()),
        source_provenance: PackageSourceProvenance::ImmutableManagedExtract,
        policy,
        host_triple: host_target_triple().to_string(),
        staging_destination_slot_dir: package_dir.to_path_buf(),
    };
    orchestrator
        .materialize(&request, sink)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// `add`-side compile: build the just-placed slot, rolling the placement
/// (folder + lock entry) back on failure so a non-compiling `add` leaves no
/// `streamlib_modules/` slot and no lock entry.
pub fn build_added_slot_or_rollback(
    app: &AppModulesDir,
    report: &AddPackageReport,
    orchestrator: &dyn BuildOrchestrator,
    sink: &dyn BuildEventSink,
    policy: BuildPolicy,
) -> Result<()> {
    match build_placed_slot(orchestrator, sink, &report.package, &report.package_dir, policy) {
        Ok(_) => Ok(()),
        Err(build_err) => {
            if let Err(rollback_err) = app.remove_package(&report.package) {
                tracing::warn!(
                    package = %report.package,
                    error = %rollback_err,
                    "add: rolling back placement after a build failure also failed"
                );
            }
            anyhow::bail!(
                "building '{}' failed; rolled back its streamlib_modules/ slot and \
                 streamlib.lock entry:\n{build_err}",
                report.package
            )
        }
    }
}

/// `install`-side compile: build every materialized (non-linked) reproduced
/// slot, AGGREGATING failures so one broken package doesn't mask the rest.
/// Install reproduces rather than acquires, so it does NOT roll back — a
/// partially built set stays on disk and every broken package is listed.
/// Linked entries stay lazy/edit-rebuild and are skipped.
pub fn build_installed_slots(
    report: &InstallFromLockfileReport,
    orchestrator: &dyn BuildOrchestrator,
    sink: &dyn BuildEventSink,
    policy: BuildPolicy,
) -> Result<()> {
    let mut failures: Vec<(PackageRef, anyhow::Error)> = Vec::new();
    for pkg in &report.packages {
        if pkg.kind == InstalledFromLockKind::Linked {
            continue;
        }
        println!();
        println!("Building {} v{}…", pkg.package, pkg.version);
        if let Err(e) =
            build_placed_slot(orchestrator, sink, &pkg.package, &pkg.package_dir, policy)
        {
            eprintln!("  build failed for {}: {e}", pkg.package);
            failures.push((pkg.package.clone(), e));
        }
    }
    if failures.is_empty() {
        return Ok(());
    }
    let mut message = String::from("install failed to build the following package(s):");
    for (package, error) in &failures {
        message.push_str(&format!("\n  - {package}: {error}"));
    }
    anyhow::bail!(message)
}

/// Console sink: surface build-tool output to the operator's terminal so a
/// compile failure shows the compiler error rather than a debug-level swallow.
pub struct ConsoleBuildEventSink;

impl BuildEventSink for ConsoleBuildEventSink {
    fn emit(&self, event: BuildEvent) {
        match event {
            BuildEvent::Started { language } => println!("  compiling {language}…"),
            BuildEvent::Line { stream, line } => match stream {
                BuildStream::Stdout => println!("    {line}"),
                BuildStream::Stderr => eprintln!("    {line}"),
            },
            BuildEvent::Finished { language } => println!("  compiled {language}"),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use streamlib::sdk::runtime::{AddPackageSource, BuildError};
    use streamlib_idents::{Org, Package};

    use super::*;

    /// A recording stub orchestrator: captures every request it is handed and
    /// fails the packages named in `fail_packages`, so the CLI wiring
    /// (request shape, rollback, aggregation, linked-skip) is testable without
    /// a toolchain.
    #[derive(Default)]
    struct StubOrchestrator {
        fail_packages: HashSet<String>,
        seen: Mutex<Vec<BuildRequest>>,
    }

    impl BuildOrchestrator for StubOrchestrator {
        fn materialize(
            &self,
            request: &BuildRequest,
            _sink: &dyn BuildEventSink,
        ) -> std::result::Result<StagedArtifact, BuildError> {
            self.seen.lock().unwrap().push(request.clone());
            if self.fail_packages.contains(&request.package.to_string()) {
                return Err(BuildError::BuildFailed {
                    tool: "cargo".to_string(),
                    package: request.package.to_string(),
                    detail: "synthetic compile failure".to_string(),
                });
            }
            Ok(StagedArtifact {
                staged_dir: request.staging_destination_slot_dir.clone(),
                rebuilt: true,
            })
        }
    }

    /// Write a minimal schema-only package folder (a valid `package:` manifest,
    /// no processors, so `add_package` places it with no toolchain needed).
    fn write_schema_only_package(dir: &Path, org: &str, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("streamlib.yaml"),
            format!("package:\n  org: {org}\n  name: {name}\n  version: 0.1.0\n"),
        )
        .unwrap();
    }

    fn ref_of(org: &str, name: &str) -> PackageRef {
        PackageRef::new(Org::new(org).unwrap(), Package::new(name).unwrap())
    }

    #[test]
    fn build_placed_slot_requests_in_place_immutable_ifstale() {
        let stub = StubOrchestrator::default();
        let sink = ConsoleBuildEventSink;
        let package = ref_of("tatolab", "widget");
        let slot = PathBuf::from("/tmp/streamlib_modules/@tatolab/widget");

        build_placed_slot(&stub, &sink, &package, &slot, BuildPolicy::IfStale).unwrap();

        let seen = stub.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        let req = &seen[0];
        // In-place: the build source IS the destination slot.
        assert!(matches!(&req.source, BuildSource::PackageDir(dir) if dir == &slot));
        assert_eq!(req.staging_destination_slot_dir, slot);
        // The two zero-cargo-reuse preconditions the CLI must supply.
        assert_eq!(
            req.source_provenance,
            PackageSourceProvenance::ImmutableManagedExtract
        );
        assert_eq!(req.policy, BuildPolicy::IfStale);
        assert_eq!(req.host_triple, host_target_triple());
    }

    #[test]
    fn add_rollback_removes_slot_and_lock_entry_on_build_failure() {
        let app_root = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        write_schema_only_package(src.path(), "tatolab", "widget");

        let app = AppModulesDir::at(app_root.path());
        let report = app
            .add_package(
                &AddPackageSource::Folder {
                    path: src.path().to_path_buf(),
                },
                &Default::default(),
            )
            .unwrap();
        // Placement happened.
        assert!(report.package_dir.exists());
        assert!(app.read_lockfile().unwrap().packages.contains_key("@tatolab/widget"));

        let stub = StubOrchestrator {
            fail_packages: HashSet::from(["@tatolab/widget".to_string()]),
            ..Default::default()
        };
        let err = build_added_slot_or_rollback(
            &app,
            &report,
            &stub,
            &ConsoleBuildEventSink,
            BuildPolicy::IfStale,
        )
        .expect_err("a failed build must fail the add");
        assert!(err.to_string().contains("rolled back"), "{err}");

        // Rolled back: no slot, no lock entry.
        assert!(!report.package_dir.exists(), "slot must be gone after rollback");
        assert!(
            !app.read_lockfile()
                .unwrap()
                .packages
                .contains_key("@tatolab/widget"),
            "lock entry must be gone after rollback"
        );
    }

    #[test]
    fn add_keeps_slot_and_lock_entry_on_build_success() {
        let app_root = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        write_schema_only_package(src.path(), "tatolab", "widget");

        let app = AppModulesDir::at(app_root.path());
        let report = app
            .add_package(
                &AddPackageSource::Folder {
                    path: src.path().to_path_buf(),
                },
                &Default::default(),
            )
            .unwrap();

        build_added_slot_or_rollback(
            &app,
            &report,
            &StubOrchestrator::default(),
            &ConsoleBuildEventSink,
            BuildPolicy::IfStale,
        )
        .unwrap();

        assert!(report.package_dir.exists());
        assert!(app.read_lockfile().unwrap().packages.contains_key("@tatolab/widget"));
    }

    #[test]
    fn install_aggregates_failures_skips_linked_and_does_not_roll_back() {
        let app_root = tempfile::tempdir().unwrap();
        let good_src = tempfile::tempdir().unwrap();
        let bad_src = tempfile::tempdir().unwrap();
        let linked_src = tempfile::tempdir().unwrap();
        write_schema_only_package(good_src.path(), "tatolab", "good");
        write_schema_only_package(bad_src.path(), "tatolab", "bad");
        write_schema_only_package(linked_src.path(), "tatolab", "linked");

        let app = AppModulesDir::at(app_root.path());
        app.add_package(
            &AddPackageSource::Folder {
                path: good_src.path().to_path_buf(),
            },
            &Default::default(),
        )
        .unwrap();
        app.add_package(
            &AddPackageSource::Folder {
                path: bad_src.path().to_path_buf(),
            },
            &Default::default(),
        )
        .unwrap();
        app.link_package(linked_src.path()).unwrap();

        let report = app.install_from_lockfile().unwrap();

        let stub = StubOrchestrator {
            fail_packages: HashSet::from(["@tatolab/bad".to_string()]),
            ..Default::default()
        };
        let err =
            build_installed_slots(&report, &stub, &ConsoleBuildEventSink, BuildPolicy::IfStale)
                .expect_err("a broken package must fail install");
        let message = err.to_string();
        assert!(message.contains("@tatolab/bad"), "must name the broken package: {message}");
        assert!(!message.contains("@tatolab/good"), "must not name the good package: {message}");

        // The linked entry stays lazy — never handed to the orchestrator.
        let built: Vec<String> = stub
            .seen
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.package.to_string())
            .collect();
        assert!(built.contains(&"@tatolab/good".to_string()));
        assert!(built.contains(&"@tatolab/bad".to_string()));
        assert!(
            !built.contains(&"@tatolab/linked".to_string()),
            "a linked slot must not be compiled at install time: {built:?}"
        );

        // Install does NOT roll back: both materialized slots remain on disk.
        assert!(app.package_dir(&ref_of("tatolab", "good")).exists());
        assert!(app.package_dir(&ref_of("tatolab", "bad")).exists());
    }
}
