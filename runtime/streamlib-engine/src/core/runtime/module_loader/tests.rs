// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Module-loading tests. Runner-lifecycle tests stay in `runtime.rs`'s
//! `tests` module; everything else lives here: `list_available_triples`,
//! the [`Strategy`] source resolver + dep-walker fixtures, recursive dep
//! walking, cycle detection, the [`BuildPolicy`] no-orchestrator
//! semantics, and `remove_module` deferral.

use super::processor_registration::{host_target_triple, list_available_triples};
use super::*;
use serial_test::serial;

/// Resolve the co-located `streamlib_modules/@org/name` slot through the
/// production [`installed_package_slot_dir`] seam, so a fixture writes to (and
/// an assertion reads) the exact slot a locked run derives — never a
/// hand-rolled `{name}` string that could silently drift from the seam. The
/// slot is version-free; the single place that pins the literal on-disk layout
/// is the `installed_cache_slot_layout_canary` test.
///
/// [`installed_package_slot_dir`]: crate::core::installed_package_slot_dir
fn installed_package_slot_for_test(org: &str, name: &str) -> std::path::PathBuf {
    let pkg_ref = streamlib_idents::PackageRef::new(
        streamlib_idents::Org::new(org).unwrap(),
        streamlib_idents::Package::new(name).unwrap(),
    );
    crate::core::installed_package_slot_dir(None, &pkg_ref)
}

/// Minimal [`BuildOrchestrator`] for tests: "materializes" a
/// `PackageDir` source by loading it in place (no toolchain invocation).
/// Doubles as proof the injected build seam works, and lets path/git-dep
/// fixtures — whose transitive deps derive [`BuildPolicy::IfStale`] —
/// load without a real builder. Build-requiring loads with NO
/// orchestrator wired fail loud (`BuildRequiredButNoOrchestrator`); these
/// tests opt in to this no-op loader to exercise the recursion instead.
struct LoadAsIsOrchestrator;
impl BuildOrchestrator for LoadAsIsOrchestrator {
    fn materialize(
        &self,
        request: &BuildRequest,
        _sink: &dyn BuildEventSink,
    ) -> std::result::Result<StagedArtifact, BuildError> {
        match &request.source {
            BuildSource::PackageDir(dir) => Ok(StagedArtifact {
                staged_dir: dir.clone(),
                rebuilt: false,
            }),
            other => Err(BuildError::UnsupportedSource(format!("{other:?}"))),
        }
    }
}

/// [`LoadAsIsOrchestrator`] plus a per-directory materialize-call counter,
/// letting tests assert a skipped package's subtree was not re-walked.
struct MaterializeCountingOrchestrator {
    counts:
        std::sync::Arc<parking_lot::Mutex<std::collections::HashMap<std::path::PathBuf, usize>>>,
}
impl BuildOrchestrator for MaterializeCountingOrchestrator {
    fn materialize(
        &self,
        request: &BuildRequest,
        _sink: &dyn BuildEventSink,
    ) -> std::result::Result<StagedArtifact, BuildError> {
        match &request.source {
            BuildSource::PackageDir(dir) => {
                *self.counts.lock().entry(dir.clone()).or_insert(0) += 1;
                Ok(StagedArtifact {
                    staged_dir: dir.clone(),
                    rebuilt: false,
                })
            }
            other => Err(BuildError::UnsupportedSource(format!("{other:?}"))),
        }
    }
}

/// Sum of materialize calls across all dirs whose final path component
/// matches `dir_name`.
fn count_materializations_for_dir_named(
    counts: &std::sync::Arc<
        parking_lot::Mutex<std::collections::HashMap<std::path::PathBuf, usize>>,
    >,
    dir_name: &str,
) -> usize {
    counts
        .lock()
        .iter()
        .filter(|(dir, _)| dir.file_name().is_some_and(|name| name == dir_name))
        .map(|(_, count)| *count)
        .sum()
}

/// [`MaterializeCountingOrchestrator`] plus a rendezvous barrier on dirs
/// whose final path component is in `rendezvous_dir_names` —
/// deterministically overlaps two concurrent walks at the shared
/// dependency (both walks must arrive before either proceeds to the
/// single-version gate).
struct RendezvousLoadAsIsOrchestrator {
    rendezvous_dir_names: Vec<String>,
    rendezvous: std::sync::Barrier,
    counts:
        std::sync::Arc<parking_lot::Mutex<std::collections::HashMap<std::path::PathBuf, usize>>>,
}
impl BuildOrchestrator for RendezvousLoadAsIsOrchestrator {
    fn materialize(
        &self,
        request: &BuildRequest,
        _sink: &dyn BuildEventSink,
    ) -> std::result::Result<StagedArtifact, BuildError> {
        match &request.source {
            BuildSource::PackageDir(dir) => {
                *self.counts.lock().entry(dir.clone()).or_insert(0) += 1;
                let matches = dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| self.rendezvous_dir_names.iter().any(|r| r == name));
                if matches {
                    self.rendezvous.wait();
                }
                Ok(StagedArtifact {
                    staged_dir: dir.clone(),
                    rebuilt: false,
                })
            }
            other => Err(BuildError::UnsupportedSource(format!("{other:?}"))),
        }
    }
}

/// [`LoadAsIsOrchestrator`] that BLOCKS materialization of the dir whose
/// final path component matches `gated_dir_name` until the test releases
/// it (then optionally fails it) — deterministically holds the gated
/// package's parent in flight while a concurrent walk gates on it.
struct GatedSubDependencyOrchestrator {
    gated_dir_name: String,
    release: parking_lot::Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    fail_after_release: bool,
}
impl BuildOrchestrator for GatedSubDependencyOrchestrator {
    fn materialize(
        &self,
        request: &BuildRequest,
        _sink: &dyn BuildEventSink,
    ) -> std::result::Result<StagedArtifact, BuildError> {
        match &request.source {
            BuildSource::PackageDir(dir) => {
                let gated = dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name == self.gated_dir_name);
                if gated {
                    if let Some(release_receiver) = self.release.lock().take() {
                        let _ = release_receiver.recv();
                    }
                    if self.fail_after_release {
                        return Err(BuildError::UnsupportedSource(
                            "injected gated sub-dependency failure".into(),
                        ));
                    }
                }
                Ok(StagedArtifact {
                    staged_dir: dir.clone(),
                    rebuilt: false,
                })
            }
            other => Err(BuildError::UnsupportedSource(format!("{other:?}"))),
        }
    }
}

/// Poll `condition` until it holds or `timeout` elapses; returns the
/// final evaluation.
fn poll_until(timeout: std::time::Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    condition()
}

// =========================================================================
// list_available_triples
// =========================================================================

#[test]
fn list_available_triples_filters_to_subdirs_and_sorts() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = tmp.path().join("lib");

    // Missing lib/ → empty list, no error.
    assert!(list_available_triples(&lib).unwrap().is_empty());

    std::fs::create_dir(&lib).unwrap();
    std::fs::create_dir(lib.join("aarch64-apple-darwin")).unwrap();
    std::fs::create_dir(lib.join("x86_64-unknown-linux-gnu")).unwrap();
    std::fs::write(lib.join("README.md"), b"stray").unwrap();

    let triples = list_available_triples(&lib).unwrap();
    assert_eq!(
        triples,
        vec![
            "aarch64-apple-darwin".to_string(),
            "x86_64-unknown-linux-gnu".to_string(),
        ]
    );
}

// =========================================================================
// Strategy::Path dep walker (loads an explicit directory; recurses)
// =========================================================================

/// Path-style dep recursion: `add_module_with(Path(A))` must walk into
/// `B` (declared as `path: ../b`) and parse its manifest.
#[test]
#[serial]
fn path_strategy_recurses_into_path_dep() {
    let runtime = Runner::new().unwrap();
    runtime.set_build_orchestrator(LoadAsIsOrchestrator);
    let tmp = tempfile::tempdir().unwrap();

    let a = tmp.path().join("a");
    std::fs::create_dir(&a).unwrap();
    std::fs::write(
        a.join("streamlib.yaml"),
        r#"
package:
  org: tatolab
  name: a
  version: "0.1.0"
dependencies:
  "@tatolab/b":
    path: ../b
"#,
    )
    .unwrap();

    let b = tmp.path().join("b");
    std::fs::create_dir(&b).unwrap();
    std::fs::write(
        b.join("streamlib.yaml"),
        r#"
package:
  org: tatolab
  name: b
  version: "0.1.0"
"#,
    )
    .unwrap();

    runtime
        .add_module_with_blocking(
            streamlib_idents::ModuleIdent::any(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("a").unwrap(),
            ),
            Strategy::Path {
                path: a.clone(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect("add_module_with should recurse into path dep without error");
}

#[test]
#[serial]
fn path_strategy_registers_package_schemas_for_runtime_lookup() {
    let runtime = Runner::new().unwrap();
    let tmp = tempfile::tempdir().unwrap();

    let pkg = tmp.path().join("pkg-with-schema");
    std::fs::create_dir(&pkg).unwrap();
    std::fs::create_dir(pkg.join("schemas")).unwrap();
    std::fs::write(
        pkg.join("schemas/my_test_config.yaml"),
        "metadata:\n  type: MyTestConfig\n  expected_payload_bytes: 8192\n",
    )
    .unwrap();
    std::fs::write(
        pkg.join("streamlib.yaml"),
        r#"
package:
  org: tatolab
  name: test-load-project-registers-schemas
  version: "0.1.0"

schemas:
  MyTestConfig:
    file: schemas/my_test_config.yaml
"#,
    )
    .unwrap();

    let canonical = "@tatolab/test-load-project-registers-schemas/MyTestConfig";

    assert!(
        crate::core::embedded_schemas::get_embedded_schema_definition(canonical).is_none(),
        "fresh canonical id must not exist before add_module_with"
    );

    runtime
        .add_module_with_blocking(
            streamlib_idents::ModuleIdent::any(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("test-load-project-registers-schemas").unwrap(),
            ),
            Strategy::Path {
                path: pkg.clone(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect("add_module_with must succeed for schemas-only package");

    let body = crate::core::embedded_schemas::get_embedded_schema_definition(canonical)
        .expect("registered schema must be discoverable post-load");
    assert!(body.contains("MyTestConfig"));
    let port_spec =
        streamlib_processor_schema::PortSchemaSpec::Specific(streamlib_idents::SchemaIdent::new(
            streamlib_idents::Org::new("tatolab").unwrap(),
            streamlib_idents::Package::new("test-load-project-registers-schemas").unwrap(),
            streamlib_idents::TypeName::new("MyTestConfig").unwrap(),
            streamlib_idents::SemVer::new(1, 0, 0),
        ));
    assert_eq!(
        crate::core::embedded_schemas::expected_payload_bytes_for_port_spec(&port_spec).unwrap(),
        8192,
        "expected_payload_bytes_for_port_spec must read metadata declared by the loaded package"
    );
}

#[test]
#[serial]
fn path_strategy_path_dep_missing_manifest_propagates_error() {
    let runtime = Runner::new().unwrap();
    let tmp = tempfile::tempdir().unwrap();

    let a = tmp.path().join("a");
    std::fs::create_dir(&a).unwrap();
    std::fs::write(
        a.join("streamlib.yaml"),
        r#"
package:
  org: tatolab
  name: a
  version: "0.1.0"
dependencies:
  "@tatolab/missing":
    path: ../does-not-exist
"#,
    )
    .unwrap();

    let result = runtime.add_module_with_blocking(
        streamlib_idents::ModuleIdent::any(
            streamlib_idents::Org::new("tatolab").unwrap(),
            streamlib_idents::Package::new("a").unwrap(),
        ),
        Strategy::Path {
            path: a.clone(),
            build: BuildPolicy::NeverBuild,
        },
    );
    assert!(
        result.is_err(),
        "add_module_with must error when a path dep target has no streamlib.yaml"
    );
}

#[test]
#[serial]
fn path_strategy_resolves_registry_dep_via_consumer_patch() {
    let runtime = Runner::new().unwrap();
    runtime.set_build_orchestrator(LoadAsIsOrchestrator);
    let tmp = tempfile::tempdir().unwrap();

    let b = tmp.path().join("b");
    std::fs::create_dir(&b).unwrap();
    std::fs::write(
        b.join("streamlib.yaml"),
        "package:\n  org: tatolab\n  name: b\n  version: \"0.1.0\"\n",
    )
    .unwrap();

    let a = tmp.path().join("a");
    std::fs::create_dir(&a).unwrap();
    std::fs::write(
        a.join("streamlib.yaml"),
        r#"
package:
  org: tatolab
  name: a
  version: "0.1.0"
dependencies:
  "@tatolab/b": "^0.1.0"
patch:
  "@tatolab/b":
    path: ../b
"#,
    )
    .unwrap();

    runtime
        .add_module_with_blocking(
            streamlib_idents::ModuleIdent::any(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("a").unwrap(),
            ),
            Strategy::Path {
                path: a.clone(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect("consumer-scoped patch must resolve the registry dep to ../b/");
}

#[test]
#[serial]
fn path_strategy_resolves_git_patch_via_shared_helper() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("b-repo");
    std::fs::create_dir(&repo).unwrap();

    let run_git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .status()
            .expect("git invocation");
        assert!(status.success(), "git {:?} failed", args);
    };

    run_git(&["init", "--quiet"]);
    run_git(&["config", "user.email", "test@example.com"]);
    run_git(&["config", "user.name", "test"]);
    std::fs::write(
        repo.join("streamlib.yaml"),
        "package:\n  org: tatolab\n  name: b\n  version: \"0.1.0\"\n",
    )
    .unwrap();
    run_git(&["add", "streamlib.yaml"]);
    run_git(&["commit", "--quiet", "-m", "initial"]);
    let rev_output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&repo)
        .output()
        .expect("git rev-parse");
    let rev = String::from_utf8(rev_output.stdout)
        .unwrap()
        .trim()
        .to_string();

    let sandbox = tempfile::tempdir().unwrap();
    let prev_home = std::env::var_os("STREAMLIB_HOME");
    unsafe {
        std::env::set_var("STREAMLIB_HOME", sandbox.path());
    }
    let _restore = StreamlibHomeRestore(prev_home);

    let consumer = tempfile::tempdir().unwrap();
    std::fs::write(
        consumer.path().join("streamlib.yaml"),
        format!(
            r#"
package:
  org: tatolab
  name: consumer
  version: "0.1.0"
dependencies:
  "@tatolab/b": "^0.1.0"
patch:
  "@tatolab/b":
    git: "{}"
    rev: "{}"
"#,
            repo.display(),
            rev,
        ),
    )
    .unwrap();

    // No orchestrator wired: the git dep derives `IfStale`, which
    // degrades to load-the-fetched-checkout-as-is. The schemas-only
    // checkout loads cleanly.
    let runtime = Runner::new().unwrap();
    runtime.set_build_orchestrator(LoadAsIsOrchestrator);
    runtime
        .add_module_with_blocking(
            streamlib_idents::ModuleIdent::any(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("consumer").unwrap(),
            ),
            Strategy::Path {
                path: consumer.path().to_path_buf(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect("git patch must clone the local repo and recurse into it");
}

#[test]
#[serial]
fn path_strategy_strict_errors_on_missing_patch_path() {
    let runtime = Runner::new().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("streamlib.yaml"),
        r#"
package:
  org: tatolab
  name: a
  version: "0.1.0"
dependencies:
  "@tatolab/b": "^0.1.0"
patch:
  "@tatolab/b":
    path: ./does-not-exist
"#,
    )
    .unwrap();

    let err = runtime
        .add_module_with_blocking(
            streamlib_idents::ModuleIdent::any(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("a").unwrap(),
            ),
            Strategy::Path {
                path: tmp.path().to_path_buf(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect_err("missing patch path must error strictly");
    let msg = format!("{err}");
    assert!(
        msg.contains("@tatolab/b"),
        "error must surface the canonical dep ref, got: {msg}"
    );
    assert!(
        msg.contains("does-not-exist") && msg.contains("does not exist"),
        "error must call out the missing patch path, got: {msg}"
    );
}

/// Drops a previously-saved `STREAMLIB_HOME` environment variable state
/// when the test scope ends, so a sandboxed `STREAMLIB_HOME` override
/// doesn't leak into the next `#[serial]` test.
struct StreamlibHomeRestore(Option<std::ffi::OsString>);
impl Drop for StreamlibHomeRestore {
    fn drop(&mut self) {
        // SAFETY: `#[serial]` makes every test in this module exclusive.
        unsafe {
            match self.0.take() {
                Some(v) => std::env::set_var("STREAMLIB_HOME", v),
                None => std::env::remove_var("STREAMLIB_HOME"),
            }
        }
    }
}

/// Pins the process-global app-modules root override to a test app root for the
/// scope, clearing it on drop. Post-#1506 the installed slot lives under
/// `<app-root>/streamlib_modules/`, so a locked-run test that hand-stages via
/// the `None` seam and reads a lockfile from the same dir must anchor both at
/// one app root — otherwise the seam falls through to the process cwd and the
/// staged slot and the locked read diverge.
struct AppModulesRootOverrideGuard;
impl AppModulesRootOverrideGuard {
    fn install(app_root: &std::path::Path) -> Self {
        crate::core::streamlib_home::set_app_modules_root_override(Some(app_root.to_path_buf()));
        Self
    }
}
impl Drop for AppModulesRootOverrideGuard {
    fn drop(&mut self) {
        crate::core::streamlib_home::set_app_modules_root_override(None);
    }
}

/// Clears a set of env vars on construction and restores them on drop, so a
/// test can assert behavior under "no ambient registry config" deterministically
/// regardless of the developer/CI shell. `#[serial]` makes the env exclusive.
struct EnvVarsCleared(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl EnvVarsCleared {
    fn new(vars: &[&'static str]) -> Self {
        let saved = vars
            .iter()
            .map(|&v| {
                let prev = std::env::var_os(v);
                // SAFETY: `#[serial]` makes every test in this module exclusive.
                unsafe {
                    std::env::remove_var(v);
                }
                (v, prev)
            })
            .collect();
        Self(saved)
    }
}
impl Drop for EnvVarsCleared {
    fn drop(&mut self) {
        for (v, prev) in self.0.drain(..) {
            // SAFETY: `#[serial]` makes every test in this module exclusive.
            unsafe {
                match prev {
                    Some(val) => std::env::set_var(v, val),
                    None => std::env::remove_var(v),
                }
            }
        }
    }
}

/// Assemble a minimal schemas-only `.slpkg` (a ZIP with `streamlib.yaml`
/// plus one schema file) at `out`, returning the SHA-256 hex of the
/// archive bytes for an integrity-pin assertion.
fn write_schemas_only_slpkg(out: &std::path::Path, name: &str, type_name: &str) -> String {
    use std::io::Write;
    let stem = type_name.to_ascii_lowercase();
    let manifest = format!(
        "package:\n  org: tatolab\n  name: {name}\n  version: \"0.1.0\"\n\
         schemas:\n  {type_name}:\n    file: schemas/{stem}.yaml\n"
    );
    let schema = format!("metadata:\n  type: {type_name}\n  expected_payload_bytes: 4096\n");

    let mut buf = Vec::new();
    {
        let mut zw = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zw.start_file("streamlib.yaml", opts).unwrap();
        zw.write_all(manifest.as_bytes()).unwrap();
        zw.start_file(format!("schemas/{stem}.yaml"), opts).unwrap();
        zw.write_all(schema.as_bytes()).unwrap();
        zw.finish().unwrap();
    }
    std::fs::write(out, &buf).unwrap();

    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(&buf);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// End-to-end: `Strategy::Url` against a `file://` URL fetches the
/// `.slpkg`, verifies the integrity pin, extracts, resolves (schemas-only
/// → load-as-is), and registers the schema — proving the full
/// fetch→verify→extract→resolve→register path. Locks exit criteria 1 & 2.
#[test]
#[serial]
fn url_strategy_fetches_extracts_and_registers_schema() {
    let sandbox = tempfile::tempdir().unwrap();
    let prev_home = std::env::var_os("STREAMLIB_HOME");
    unsafe {
        std::env::set_var("STREAMLIB_HOME", sandbox.path());
    }
    let _restore = StreamlibHomeRestore(prev_home);
    // Pin the app-modules root to the sandbox so the extracted slot lands under
    // it (not the crate working tree), keeping `cargo test` clean.
    let _modules_root = AppModulesRootOverrideGuard::install(sandbox.path());

    let src = tempfile::tempdir().unwrap();
    let slpkg = src.path().join("url-pkg.slpkg");
    let sha = write_schemas_only_slpkg(&slpkg, "url-fetch-pkg", "UrlFetchSchema");
    let url = format!("file://{}", slpkg.display());

    let canonical = "@tatolab/url-fetch-pkg/UrlFetchSchema";
    assert!(
        crate::core::embedded_schemas::get_embedded_schema_definition(canonical).is_none(),
        "schema must not exist before the URL load"
    );

    let runtime = Runner::new().unwrap();
    runtime
        .add_module_with_blocking(
            streamlib_idents::ModuleIdent::any(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("url-fetch-pkg").unwrap(),
            ),
            Strategy::Url {
                url,
                build: BuildPolicy::NeverBuild,
                checksum: Some(ArtifactChecksum::Sha256(sha)),
            },
        )
        .expect("Strategy::Url must fetch, verify, extract, resolve, and register");

    assert!(
        crate::core::embedded_schemas::get_embedded_schema_definition(canonical).is_some(),
        "schema must be registered after the URL load — the full path did not run if this fails"
    );
}

/// A wrong integrity pin must fail the whole load loud — never register a
/// package whose bytes don't match the pin. Locks exit criterion 2's
/// negative path through the public runtime surface.
#[test]
#[serial]
fn url_strategy_rejects_checksum_mismatch() {
    let sandbox = tempfile::tempdir().unwrap();
    let prev_home = std::env::var_os("STREAMLIB_HOME");
    unsafe {
        std::env::set_var("STREAMLIB_HOME", sandbox.path());
    }
    let _restore = StreamlibHomeRestore(prev_home);
    let _modules_root = AppModulesRootOverrideGuard::install(sandbox.path());

    let src = tempfile::tempdir().unwrap();
    let slpkg = src.path().join("url-pkg.slpkg");
    write_schemas_only_slpkg(&slpkg, "url-bad-pkg", "UrlBadSchema");
    let url = format!("file://{}", slpkg.display());

    let runtime = Runner::new().unwrap();
    let err = runtime
        .add_module_with_blocking(
            streamlib_idents::ModuleIdent::any(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("url-bad-pkg").unwrap(),
            ),
            Strategy::Url {
                url,
                build: BuildPolicy::NeverBuild,
                checksum: Some(ArtifactChecksum::Sha256("00".repeat(32))),
            },
        )
        .expect_err("a mismatched integrity pin must fail the load");
    assert!(
        matches!(err, AddModuleError::IntegrityCheckFailed { .. }),
        "expected IntegrityCheckFailed, got: {err:?}"
    );
    assert!(
        crate::core::embedded_schemas::get_embedded_schema_definition(
            "@tatolab/url-bad-pkg/UrlBadSchema"
        )
        .is_none(),
        "no schema may register when the integrity check fails"
    );
}

#[test]
#[serial]
fn path_package_registry_dep_routes_to_registry_not_installed_cache() {
    // Registry-only model: a package's streamlib.yaml dependency resolves from
    // the static registry (Strategy::Registry), NOT the installed-package cache.
    // The installed-cache-as-dep fallback an earlier model used is gone — proven
    // by routing: with no registry configured, the dep errors
    // RegistryNotConfigured even though a satisfying installed-cache entry exists.
    let sandbox = tempfile::tempdir().unwrap();
    let prev_home = std::env::var_os("STREAMLIB_HOME");
    unsafe {
        std::env::set_var("STREAMLIB_HOME", sandbox.path());
    }
    let _restore = StreamlibHomeRestore(prev_home);
    // Pin the app-modules root to the sandbox so the hand-staged slot lands
    // under it (not the crate working tree), keeping `cargo test` clean.
    let _modules_root = AppModulesRootOverrideGuard::install(sandbox.path());
    // No ambient registry config, so the routing is observable regardless of
    // the developer / CI shell.
    let _no_registry = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL", "STREAMLIB_REGISTRY_TOKEN"]);

    // A co-located streamlib_modules slot for @tatolab/b that WOULD satisfy
    // `^0.1.0` — resolved through the seam so it lands at the real layout.
    let dep_cache_dir = installed_package_slot_for_test("tatolab", "b");
    std::fs::create_dir_all(&dep_cache_dir).unwrap();
    std::fs::write(
        dep_cache_dir.join("streamlib.yaml"),
        "package:\n  org: tatolab\n  name: b\n  version: \"0.1.0\"\n",
    )
    .unwrap();

    // ...is ignored: the consumer's dep routes to Strategy::Registry, which
    // fails because no registry is configured.
    let consumer = tempfile::tempdir().unwrap();
    std::fs::write(
        consumer.path().join("streamlib.yaml"),
        r#"
package:
  org: tatolab
  name: consumer
  version: "0.1.0"
dependencies:
  "@tatolab/b": "^0.1.0"
"#,
    )
    .unwrap();

    let runtime = Runner::new().unwrap();
    let err = runtime
        .add_module_with_blocking(
            streamlib_idents::ModuleIdent::any(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("consumer").unwrap(),
            ),
            Strategy::Path {
                path: consumer.path().to_path_buf(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect_err("a registry dep must route to the registry, not the installed cache");

    assert!(
        matches!(err, AddModuleError::RegistryNotConfigured { ref package, .. } if package.name.as_str() == "b"),
        "expected RegistryNotConfigured(@tatolab/b) — deps resolve from the registry, \
         not the installed cache; got: {err:?}",
    );
}

#[test]
#[serial]
fn path_strategy_unresolvable_registry_dep_errors_actionably() {
    let runtime = Runner::new().unwrap();
    let tmp = tempfile::tempdir().unwrap();

    let a = tmp.path().join("a");
    std::fs::create_dir(&a).unwrap();
    std::fs::write(
        a.join("streamlib.yaml"),
        r#"
package:
  org: tatolab
  name: a
  version: "0.1.0"
dependencies:
  "@tatolab/missing": "^1.0.0"
"#,
    )
    .unwrap();

    let err = runtime
        .add_module_with_blocking(
            streamlib_idents::ModuleIdent::any(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("a").unwrap(),
            ),
            Strategy::Path {
                path: a.clone(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect_err("unresolvable registry dep must error");
    let msg = format!("{}", err);
    assert!(
        msg.contains("@tatolab/missing"),
        "error must surface the canonical `@org/name` key, got: {msg}"
    );
}

#[test]
#[serial]
fn path_strategy_rust_dylib_missing_host_triple_surfaces_available_triples() {
    let runtime = Runner::new().unwrap();
    let tmp = tempfile::tempdir().unwrap();

    let pkg = tmp.path().join("pkg");
    std::fs::create_dir(&pkg).unwrap();
    std::fs::write(
        pkg.join("streamlib.yaml"),
        r#"
package:
  org: tatolab
  name: triple-mismatch-pkg
  version: "0.1.0"
processors:
  - name: TestProcessor
    description: "Test"
    runtime: rust
    execution: manual
    inputs:
      - name: video_in
        schema: any
    outputs:
      - name: video_out
        schema: any
"#,
    )
    .unwrap();

    let wrong_triple = "wrong-arch-unknown-elsewhere-gnu";
    let wrong_dir = pkg.join("lib").join(wrong_triple);
    std::fs::create_dir_all(&wrong_dir).unwrap();
    std::fs::write(wrong_dir.join("libfake.so"), b"not-a-real-dylib").unwrap();

    let err = runtime
        .add_module_with_blocking(
            streamlib_idents::ModuleIdent::any(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("triple-mismatch-pkg").unwrap(),
            ),
            Strategy::Path {
                path: pkg.clone(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect_err("missing host-triple subdir must error");
    let msg = format!("{}", err);
    assert!(
        msg.contains(host_target_triple()),
        "error must name the host triple so the user sees what was expected, got: {msg}"
    );
    assert!(
        msg.contains(wrong_triple),
        "error must list the triples that ARE present, got: {msg}"
    );
    // Placeholder drop-guard witness: the processor-registration failure
    // exit must clear the armed single-version placeholder.
    assert!(
        !runtime
            .resolution_memo
            .contains_package(&streamlib_idents::PackageRef::new(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("triple-mismatch-pkg").unwrap(),
            )),
        "processor-registration failure must clear the in-flight placeholder",
    );
}

#[test]
#[serial]
fn path_strategy_rust_dylib_resolves_host_triple_then_dlopens() {
    let runtime = Runner::new().unwrap();
    let tmp = tempfile::tempdir().unwrap();

    let pkg = tmp.path().join("pkg");
    std::fs::create_dir(&pkg).unwrap();
    std::fs::write(
        pkg.join("streamlib.yaml"),
        r#"
package:
  org: tatolab
  name: triple-match-pkg
  version: "0.1.0"
processors:
  - name: TestProcessor
    description: "Test"
    runtime: rust
    execution: manual
    inputs:
      - name: video_in
        schema: any
    outputs:
      - name: video_out
        schema: any
"#,
    )
    .unwrap();

    let triple_dir = pkg.join("lib").join(host_target_triple());
    std::fs::create_dir_all(&triple_dir).unwrap();
    let dylib_ext = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };
    let dylib_path = triple_dir.join(format!("libfake.{}", dylib_ext));
    std::fs::write(&dylib_path, b"not-a-real-dylib").unwrap();

    let err = runtime
        .add_module_with_blocking(
            streamlib_idents::ModuleIdent::any(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("triple-match-pkg").unwrap(),
            ),
            Strategy::Path {
                path: pkg.clone(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect_err("junk dylib must fail at dlopen, not at path resolution");
    let msg = format!("{}", err);
    assert!(
        msg.contains("libfake"),
        "error must reference the dylib file (proving path resolution reached dlopen), got: {msg}"
    );
    assert!(
        !msg.contains("No .so file found")
            && !msg.contains("No .dylib file found")
            && !msg.contains("No .dll file found"),
        "error must NOT be the 'no dylib found' variant, got: {msg}"
    );
}

#[test]
#[serial]
fn path_strategy_schemas_only_skips_lib_lookup() {
    let runtime = Runner::new().unwrap();
    let tmp = tempfile::tempdir().unwrap();

    let pkg = tmp.path().join("schemas-only");
    std::fs::create_dir(&pkg).unwrap();
    std::fs::write(
        pkg.join("streamlib.yaml"),
        r#"
package:
  org: tatolab
  name: schemas-only-pkg
  version: "0.1.0"
"#,
    )
    .unwrap();

    runtime
        .add_module_with_blocking(
            streamlib_idents::ModuleIdent::any(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("schemas-only-pkg").unwrap(),
            ),
            Strategy::Path {
                path: pkg.clone(),
                build: BuildPolicy::NeverBuild,
            },
        )
        .expect("schemas-only package must load without touching lib/");
}

/// The manifest-vs-dylib cross-check must match a staged processor by its
/// `(org, package, type)` tuple, ignoring the version — the cdylib always
/// stages its own identity at `0.0.0` (the `#[processor]` grammar rejects
/// any inline version, #1409), while the loader composes the expected
/// ident from the package manifest's version. A version-strict compare
/// (`0.0.0 != 1.0.0`) misses every real load and reports the processor as
/// "declared but not registered" — issue #1460.
#[test]
fn staged_processor_cross_check_matches_across_versions() {
    use crate::core::descriptors::{ProcessorDescriptor, SchemaIdent};
    use streamlib_idents::{Org, Package, PackageRef, SemVer, TypeName};

    let staging = staging::ModuleLoadRegistrationStaging::new();

    let owner = PackageRef::new(
        Org::new("tatolab").unwrap(),
        Package::new("camera").unwrap(),
    );

    // The cdylib stages its own processor identity at `0.0.0`.
    let cdylib_code_identity = SchemaIdent::new(
        Org::new("tatolab").unwrap(),
        Package::new("camera").unwrap(),
        TypeName::new("CameraCapture").unwrap(),
        SemVer::new(0, 0, 0),
    );
    staging.stage_processor(
        ProcessorDescriptor::new(cdylib_code_identity, "camera capture"),
        staging::StagedProcessorRegistrationKind::Dynamic {
            constructor: Box::new(|_node| {
                Err(crate::core::Error::Configuration(
                    "test constructor never invoked".into(),
                ))
            }),
        },
        owner,
    );

    // The loader composes the expected ident from the manifest version
    // (the camera manifest is `1.0.0`).
    let manifest_composed_identity = SchemaIdent::new(
        Org::new("tatolab").unwrap(),
        Package::new("camera").unwrap(),
        TypeName::new("CameraCapture").unwrap(),
        SemVer::new(1, 0, 0),
    );

    assert!(
        staging.contains_staged_processor_for_tuple(&manifest_composed_identity),
        "the cross-check must match on (org, package, type), ignoring the \
         version the cdylib staged its identity at",
    );
}

// =========================================================================
// add_module / add_module_with — imperative module API + BuildPolicy
// =========================================================================

mod add_module_tests {
    use std::sync::Arc;

    use super::*;
    use streamlib_idents::{ModuleIdent, Org, Package, SemVer, SemVerRange};

    /// RAII guard that pins both `STREAMLIB_HOME` and the app-modules root to
    /// the same tempdir for the test scope, restoring the prior state on drop.
    /// The installed-package slot lives under `<app-root>/streamlib_modules/`
    /// (co-location, #1506), so isolating the cache off the host requires
    /// pinning the app-modules root too — otherwise the seam falls through to
    /// the process cwd. Both are process-global, so cache-backed tests using
    /// this guard are `#[serial]`.
    struct HomeGuard {
        home_prev: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn install(home_root: &std::path::Path) -> Self {
            let home_prev = std::env::var_os("STREAMLIB_HOME");
            unsafe {
                std::env::set_var("STREAMLIB_HOME", home_root);
            }
            crate::core::streamlib_home::set_app_modules_root_override(Some(home_root.to_path_buf()));
            Self { home_prev }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            crate::core::streamlib_home::set_app_modules_root_override(None);
            unsafe {
                match self.home_prev.take() {
                    Some(v) => std::env::set_var("STREAMLIB_HOME", v),
                    None => std::env::remove_var("STREAMLIB_HOME"),
                }
            }
        }
    }

    /// Write a schemas-only `streamlib.yaml` at `dir`. When `schema` is
    /// provided, emits a `schemas:` block plus the named schema YAML so
    /// post-load callers can verify the schema actually registered.
    fn write_schemas_only_manifest(
        dir: &std::path::Path,
        org: &str,
        name: &str,
        version: &str,
        schema: Option<&str>,
    ) {
        let body = if let Some(type_name) = schema {
            let stem = type_name.to_ascii_lowercase();
            std::fs::create_dir_all(dir.join("schemas")).unwrap();
            std::fs::write(
                dir.join("schemas").join(format!("{stem}.yaml")),
                format!("metadata:\n  type: {type_name}\n  expected_payload_bytes: 4096\n"),
            )
            .unwrap();
            format!(
                "package:\n  org: {org}\n  name: {name}\n  version: \"{version}\"\n\
                 schemas:\n  {type_name}:\n    file: schemas/{stem}.yaml\n"
            )
        } else {
            format!("package:\n  org: {org}\n  name: {name}\n  version: \"{version}\"\n")
        };
        std::fs::write(dir.join("streamlib.yaml"), body).expect("write streamlib.yaml");
    }

    /// Materialize a schemas-only package into the sandboxed app's
    /// `streamlib_modules/@org/name` slot so bare `add_module` (which resolves
    /// against that folder) can find it.
    fn install_cached_package(org: &str, name: &str, version: &str, schema: Option<&str>) {
        let dep_cache_dir = installed_package_slot_for_test(org, name);
        std::fs::create_dir_all(&dep_cache_dir).unwrap();
        write_schemas_only_manifest(&dep_cache_dir, org, name, version, schema);
    }

    /// Layout-pin canary: the ONE test in this crate that hard-codes the
    /// co-located installed-slot literal — `<app-root>/streamlib_modules/@org/name`,
    /// version-free (#1506). Every other fixture derives its slot through the
    /// [`installed_package_slot_dir`] seam (see `installed_package_slot_for_test`)
    /// so they track this pin for free — a write==read oracle. A relocation
    /// that moves the slot again must update THIS assertion and the seam body
    /// together; nothing else.
    ///
    /// [`installed_package_slot_dir`]: crate::core::installed_package_slot_dir
    #[test]
    #[serial]
    fn installed_cache_slot_layout_canary() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        let pkg_ref = streamlib_idents::PackageRef::new(
            Org::new("tatolab").unwrap(),
            Package::new("canary-pkg").unwrap(),
        );
        // `None` resolves the app root via the same chain the module loader
        // uses; the guard pins it to `home`, so the slot lands under it.
        let slot = crate::core::installed_package_slot_dir(None, &pkg_ref);
        assert_eq!(
            slot,
            home.path()
                .join("streamlib_modules/@tatolab/canary-pkg"),
            "installed-cache slot layout drifted from the pinned literal",
        );
    }

    #[test]
    #[serial]
    fn add_module_loads_from_installed_cache() {
        const TYPE_NAME: &str = "AddModuleCacheSchema";
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        install_cached_package("tatolab", "add-module-cache", "1.2.3", Some(TYPE_NAME));

        let runtime = Runner::new().expect("Runner::new");
        let canonical = format!("@tatolab/add-module-cache/{TYPE_NAME}");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical).is_none(),
            "schema must not exist before add_module"
        );

        runtime
            .add_module_blocking(ModuleIdent::new(
                Org::new("tatolab").unwrap(),
                Package::new("add-module-cache").unwrap(),
                SemVerRange::Caret(SemVer::new(1, 0, 0)),
            ))
            .expect("cache-backed add_module must succeed");

        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical).is_some(),
            "schema must be registered after add_module — the loader did not run if this fails"
        );
    }

    #[test]
    #[serial]
    fn add_module_rejects_version_range_mismatch() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        install_cached_package("tatolab", "add-module-range", "1.0.0", None);

        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module_blocking(ModuleIdent::new(
                Org::new("tatolab").unwrap(),
                Package::new("add-module-range").unwrap(),
                SemVerRange::Caret(SemVer::new(2, 0, 0)),
            ))
            .expect_err("range mismatch must error");

        assert!(
            matches!(err, AddModuleError::VersionRangeUnsatisfied { found, .. } if found == SemVer::new(1, 0, 0)),
            "expected VersionRangeUnsatisfied(1.0.0), got: {err:?}",
        );
    }

    #[test]
    #[serial]
    fn add_module_rejects_clobbered_slot_manifest() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        // Slot at @tatolab/add-module-identity but the on-disk manifest declares
        // a different identity (clobbered slot). Resolve through the seam so the
        // fixture tracks the real layout. Post-#1523 the single streamlib_modules
        // lookup gates on the slot's manifest declaring the requested package, so
        // a clobbered slot is not the requested module: it warns and reports
        // ModuleNotFound rather than resolving to the wrong identity.
        let dep_cache_dir = installed_package_slot_for_test("tatolab", "add-module-identity");
        std::fs::create_dir_all(&dep_cache_dir).unwrap();
        write_schemas_only_manifest(&dep_cache_dir, "vendor", "other", "1.0.0", None);

        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module_blocking(ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("add-module-identity").unwrap(),
            ))
            .expect_err("clobbered slot manifest must error");

        assert!(
            matches!(err, AddModuleError::ModuleNotFound { ref package, .. }
                if package.name.as_str() == "add-module-identity"),
            "expected ModuleNotFound(add-module-identity), got: {err:?}",
        );
    }

    #[test]
    #[serial]
    fn add_module_reports_module_not_found_when_uncached() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());

        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module_blocking(ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("add-module-missing").unwrap(),
            ))
            .expect_err("missing module must error");

        assert!(
            matches!(err, AddModuleError::ModuleNotFound { ref package } if package.org.as_str() == "tatolab" && package.name.as_str() == "add-module-missing"),
            "expected ModuleNotFound(@tatolab/add-module-missing), got: {err:?}",
        );
    }

    // =====================================================================
    // Strategy / BuildPolicy conformance
    // =====================================================================

    #[test]
    #[serial]
    fn installed_cache_strategy_hits_cache() {
        const TYPE_NAME: &str = "InstalledCacheOnlySchema";
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        install_cached_package("tatolab", "ic-only", "1.0.0", Some(TYPE_NAME));

        let runtime = Runner::new().expect("Runner::new");
        let canonical = format!("@tatolab/ic-only/{TYPE_NAME}");
        runtime
            .add_module_with_blocking(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("ic-only").unwrap(),
                ),
                Strategy::InstalledCache,
            )
            .expect("InstalledCache strategy must hit the cache");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical).is_some()
        );
    }

    /// A Rust `streamlib.yaml` (one Rust-runtime processor) for `name`, staged
    /// into an installed slot as source needing a host build.
    fn rust_source_manifest(name: &str) -> String {
        format!(
            "package:\n  org: tatolab\n  name: {name}\n  version: \"0.1.0\"\n\
             processors:\n  - name: RustProc\n    description: \"source-only rust processor\"\n\
             \x20   runtime: rust\n    execution: manual\n    inputs: []\n    outputs: []\n"
        )
    }

    /// ACCEPTANCE (#1508): referencing an installed-but-unbuilt package yields
    /// the typed `InstalledPackageNotBuilt` fix-it AND the wired orchestrator is
    /// NEVER asked to materialize it — zero cold-build on the app's critical
    /// path (so no `Building`/`BuildLog` event can fire, since those come only
    /// from the orchestrator's build). An installed Rust slot with source but no
    /// prebuilt is the unbuilt case. Mentally revert the load-only gate in
    /// `resolve_installed_cache_strategy` and the orchestrator materializes the
    /// slot (count > 0) and the load succeeds — failing both assertions.
    #[test]
    #[serial]
    fn installed_cache_unbuilt_slot_errors_without_building() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        // Unbuilt Rust slot: a Rust-runtime manifest + Cargo.toml, no prebuilt.
        let slot = installed_package_slot_for_test("tatolab", "unbuilt-rust");
        std::fs::create_dir_all(&slot).unwrap();
        std::fs::write(slot.join("streamlib.yaml"), rust_source_manifest("unbuilt-rust")).unwrap();
        std::fs::write(slot.join("Cargo.toml"), b"[package]\nname='unbuilt-rust'\n").unwrap();

        let counts = Arc::new(parking_lot::Mutex::new(
            std::collections::HashMap::<std::path::PathBuf, usize>::new(),
        ));
        let runtime = Runner::new().expect("Runner::new");
        runtime.set_build_orchestrator(MaterializeCountingOrchestrator {
            counts: Arc::clone(&counts),
        });

        let err = runtime
            .add_module_blocking(ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("unbuilt-rust").unwrap(),
            ))
            .expect_err("an installed-but-unbuilt slot must fail loud");
        assert!(
            matches!(err, AddModuleError::InstalledPackageNotBuilt { ref package, version }
                if package.name.as_str() == "unbuilt-rust" && version == SemVer::new(0, 1, 0)),
            "expected InstalledPackageNotBuilt(unbuilt-rust 0.1.0), got: {err:?}",
        );
        assert_eq!(
            count_materializations_for_dir_named(&counts, "unbuilt-rust"),
            0,
            "the loader must NEVER cold-build an installed slot — no orchestrator call may fire",
        );
    }

    #[test]
    #[serial]
    fn path_strategy_loads_arbitrary_dir() {
        const TYPE_NAME: &str = "PathStrategySchema";
        let home = tempfile::tempdir().unwrap();
        let arbitrary = tempfile::tempdir().unwrap();
        write_schemas_only_manifest(
            arbitrary.path(),
            "tatolab",
            "md-arbitrary",
            "0.7.2",
            Some(TYPE_NAME),
        );
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let canonical = format!("@tatolab/md-arbitrary/{TYPE_NAME}");
        runtime
            .add_module_with_blocking(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("md-arbitrary").unwrap(),
                ),
                Strategy::Path {
                    path: arbitrary.path().to_path_buf(),
                    build: BuildPolicy::NeverBuild,
                },
            )
            .expect("Path strategy must load the arbitrary dir");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical).is_some()
        );
    }

    #[test]
    #[serial]
    fn path_strategy_surfaces_missing_dir() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module_with_blocking(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("md-missing").unwrap(),
                ),
                Strategy::Path {
                    path: std::path::PathBuf::from("/nonexistent/does-not-exist"),
                    build: BuildPolicy::NeverBuild,
                },
            )
            .expect_err("missing manifest dir must error");
        assert!(
            matches!(err, AddModuleError::ManifestDirectoryMissing { .. }),
            "expected ManifestDirectoryMissing, got: {err:?}",
        );
    }

    /// `AlwaysBuild` with no orchestrator wired is the strict policy: it
    /// demanded a build, so it fails loud rather than silently loading a
    /// possibly-stale artifact.
    #[test]
    #[serial]
    fn always_build_without_orchestrator_fails_loud() {
        let home = tempfile::tempdir().unwrap();
        let arbitrary = tempfile::tempdir().unwrap();
        write_schemas_only_manifest(arbitrary.path(), "tatolab", "ab-no-orch", "0.1.0", None);
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module_with_blocking(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("ab-no-orch").unwrap(),
                ),
                Strategy::Path {
                    path: arbitrary.path().to_path_buf(),
                    build: BuildPolicy::AlwaysBuild,
                },
            )
            .expect_err("AlwaysBuild without an orchestrator must fail loud");
        assert!(
            matches!(err, AddModuleError::BuildRequiredButNoOrchestrator { .. }),
            "expected BuildRequiredButNoOrchestrator, got: {err:?}",
        );
    }

    /// `IfStale` with no orchestrator fails loud — same as `AlwaysBuild`.
    /// No branching on package shape; a build-requiring policy without a
    /// builder is always an error, so future agents get a clear signal
    /// instead of a silently-loaded (possibly stale) artifact.
    #[test]
    #[serial]
    fn if_stale_without_orchestrator_fails_loud() {
        let home = tempfile::tempdir().unwrap();
        let arbitrary = tempfile::tempdir().unwrap();
        write_schemas_only_manifest(
            arbitrary.path(),
            "tatolab",
            "ifstale-no-orch",
            "0.1.0",
            None,
        );
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module_with_blocking(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("ifstale-no-orch").unwrap(),
                ),
                Strategy::Path {
                    path: arbitrary.path().to_path_buf(),
                    build: BuildPolicy::IfStale,
                },
            )
            .expect_err("IfStale without an orchestrator must fail loud");
        assert!(
            matches!(err, AddModuleError::BuildRequiredButNoOrchestrator { .. }),
            "expected BuildRequiredButNoOrchestrator, got: {err:?}",
        );
    }

    /// A source-only Python package (declares a `python` runtime processor,
    /// so resolution requires a build to materialize its `.venv` + generated
    /// vocabulary) loaded with `IfStale` and NO orchestrator wired must fail
    /// loud with `BuildRequiredButNoOrchestrator` — never silently load an
    /// unbuilt source tree. Confirms the no-orchestrator guard is
    /// package-shape-agnostic: it fires before manifest content is even read,
    /// so a Python-runtime package hits the same error as a schemas-only one.
    ///
    /// Mentally-revert: if the `None` arm of the orchestrator match in the
    /// walker were changed to fall through and load-as-is, this `expect_err`
    /// would instead succeed (or fail with a downstream Python error), and
    /// the `BuildRequiredButNoOrchestrator` match would fail.
    #[test]
    #[serial]
    fn python_source_package_without_orchestrator_fails_loud() {
        let home = tempfile::tempdir().unwrap();
        let pkg = tempfile::tempdir().unwrap();
        std::fs::write(
            pkg.path().join("streamlib.yaml"),
            r#"
package:
  org: tatolab
  name: py-no-orch
  version: "0.1.0"
processors:
  - name: PyProc
    description: "source-only python processor"
    runtime: python
    execution: manual
    entrypoint: "pyproc:PyProc"
    inputs:
      - name: in0
        schema: any
    outputs:
      - name: out0
        schema: any
"#,
        )
        .unwrap();
        std::fs::write(pkg.path().join("pyproc.py"), "class PyProc:\n    pass\n").unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        // No orchestrator wired. A path dep derives IfStale, which requires
        // a build for a runtime-bearing package.
        let err = runtime
            .add_module_with_blocking(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("py-no-orch").unwrap(),
                ),
                Strategy::Path {
                    path: pkg.path().to_path_buf(),
                    build: BuildPolicy::IfStale,
                },
            )
            .expect_err(
                "a build-requiring python source package with no orchestrator must fail loud",
            );
        assert!(
            matches!(err, AddModuleError::BuildRequiredButNoOrchestrator { ref package, .. }
                if package.name.as_str() == "py-no-orch"),
            "expected BuildRequiredButNoOrchestrator for py-no-orch, got: {err:?}",
        );
    }

    // =====================================================================
    // Recursive dep walker + cycle detection
    // =====================================================================

    #[test]
    #[serial]
    fn dep_walker_recurses_through_each_dep() {
        const TYPE_A: &str = "DepWalkAllThreeASchema";
        const TYPE_B: &str = "DepWalkAllThreeBSchema";
        const TYPE_C: &str = "DepWalkAllThreeCSchema";
        let home = tempfile::tempdir().unwrap();
        let pkg_root = tempfile::tempdir().unwrap();

        let a = pkg_root.path().join("a");
        let b = pkg_root.path().join("b");
        let c = pkg_root.path().join("c");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        std::fs::create_dir(&c).unwrap();
        std::fs::create_dir(a.join("schemas")).unwrap();
        std::fs::create_dir(b.join("schemas")).unwrap();
        std::fs::create_dir(c.join("schemas")).unwrap();
        std::fs::write(
            a.join("schemas/depwalkallthreeaschema.yaml"),
            format!("metadata:\n  type: {TYPE_A}\n  expected_payload_bytes: 4096\n"),
        )
        .unwrap();
        std::fs::write(
            b.join("schemas/depwalkallthreebschema.yaml"),
            format!("metadata:\n  type: {TYPE_B}\n  expected_payload_bytes: 4096\n"),
        )
        .unwrap();
        std::fs::write(
            c.join("schemas/depwalkallthreecschema.yaml"),
            format!("metadata:\n  type: {TYPE_C}\n  expected_payload_bytes: 4096\n"),
        )
        .unwrap();
        std::fs::write(
            a.join("streamlib.yaml"),
            format!(
                "package:\n  org: tatolab\n  name: dwa-a\n  version: \"0.1.0\"\n\
                 dependencies:\n  \"@tatolab/dwa-b\":\n    path: ../b\n\
                 schemas:\n  {TYPE_A}:\n    file: schemas/depwalkallthreeaschema.yaml\n"
            ),
        )
        .unwrap();
        std::fs::write(
            b.join("streamlib.yaml"),
            format!(
                "package:\n  org: tatolab\n  name: dwa-b\n  version: \"0.1.0\"\n\
                 dependencies:\n  \"@tatolab/dwa-c\":\n    path: ../c\n\
                 schemas:\n  {TYPE_B}:\n    file: schemas/depwalkallthreebschema.yaml\n"
            ),
        )
        .unwrap();
        std::fs::write(
            c.join("streamlib.yaml"),
            format!(
                "package:\n  org: tatolab\n  name: dwa-c\n  version: \"0.1.0\"\n\
                 schemas:\n  {TYPE_C}:\n    file: schemas/depwalkallthreecschema.yaml\n"
            ),
        )
        .unwrap();

        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        runtime.set_build_orchestrator(LoadAsIsOrchestrator);
        runtime
            .add_module_with_blocking(
                ModuleIdent::any(Org::new("tatolab").unwrap(), Package::new("dwa-a").unwrap()),
                Strategy::Path {
                    path: a.clone(),
                    build: BuildPolicy::NeverBuild,
                },
            )
            .expect("dep walker must succeed end-to-end");
        for (pkg, ty) in [("dwa-a", TYPE_A), ("dwa-b", TYPE_B), ("dwa-c", TYPE_C)] {
            let canonical = format!("@tatolab/{pkg}/{ty}");
            assert!(
                crate::core::embedded_schemas::get_embedded_schema_definition(&canonical).is_some(),
                "expected schema {canonical} registered after dep walk",
            );
        }
    }

    #[test]
    #[serial]
    fn dep_walker_detects_self_referential_cycle() {
        let home = tempfile::tempdir().unwrap();
        let pkg = tempfile::tempdir().unwrap();
        std::fs::write(
            pkg.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: cycle-self\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/cycle-self\":\n    path: .\n",
        )
        .unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module_with_blocking(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("cycle-self").unwrap(),
                ),
                Strategy::Path {
                    path: pkg.path().to_path_buf(),
                    build: BuildPolicy::NeverBuild,
                },
            )
            .expect_err("self-referential cycle must error");
        match err {
            AddModuleError::DependencyCycleDetected { cycle } => {
                assert!(
                    cycle.len() >= 2
                        && cycle.first().unwrap().name.as_str() == "cycle-self"
                        && cycle.last().unwrap().name.as_str() == "cycle-self",
                    "expected cycle starting and ending at cycle-self, got: {cycle:?}",
                );
            }
            other => panic!("expected DependencyCycleDetected, got: {other:?}"),
        }
        // Placeholder drop-guard witness: the dep-recursion failure exit
        // must clear the armed placeholder — the memo never wedges.
        assert!(
            !runtime
                .resolution_memo
                .contains_package(&streamlib_idents::PackageRef::new(
                    Org::new("tatolab").unwrap(),
                    Package::new("cycle-self").unwrap(),
                )),
            "cycle failure must clear the in-flight placeholder",
        );
    }

    #[test]
    #[serial]
    fn dep_walker_detects_mutual_cycle_a_to_b_to_a() {
        let home = tempfile::tempdir().unwrap();
        let pkg_root = tempfile::tempdir().unwrap();
        let a = pkg_root.path().join("a");
        let b = pkg_root.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        std::fs::write(
            a.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: cycle-a\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/cycle-b\":\n    path: ../b\n",
        )
        .unwrap();
        std::fs::write(
            b.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: cycle-b\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/cycle-a\":\n    path: ../a\n",
        )
        .unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        runtime.set_build_orchestrator(LoadAsIsOrchestrator);
        let err = runtime
            .add_module_with_blocking(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("cycle-a").unwrap(),
                ),
                Strategy::Path {
                    path: a.clone(),
                    build: BuildPolicy::NeverBuild,
                },
            )
            .expect_err("mutual cycle must error");
        match err {
            AddModuleError::DependencyCycleDetected { cycle } => {
                let names: Vec<&str> = cycle.iter().map(|p| p.name.as_str()).collect();
                assert_eq!(names.first(), Some(&"cycle-a"));
                assert_eq!(names.last(), Some(&"cycle-a"));
                assert!(
                    names.contains(&"cycle-b"),
                    "expected cycle path to traverse cycle-b, got: {names:?}",
                );
            }
            other => panic!("expected DependencyCycleDetected, got: {other:?}"),
        }
        // Placeholder drop-guard witness: BOTH armed placeholders (a and
        // b) must be cleared as the failure unwinds through them.
        for name in ["cycle-a", "cycle-b"] {
            assert!(
                !runtime
                    .resolution_memo
                    .contains_package(&streamlib_idents::PackageRef::new(
                        Org::new("tatolab").unwrap(),
                        Package::new(name).unwrap(),
                    )),
                "cycle failure must clear the in-flight placeholder for {name}",
            );
        }
    }

    #[test]
    #[serial]
    fn dep_walker_surfaces_version_mismatch_from_dep() {
        let home = tempfile::tempdir().unwrap();
        let pkg_root = tempfile::tempdir().unwrap();
        let a = pkg_root.path().join("a");
        let b = pkg_root.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        std::fs::write(
            a.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: vmm-a\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/vmm-b\": \"^2.0.0\"\n\
             patch:\n  \"@tatolab/vmm-b\":\n    path: ../b\n",
        )
        .unwrap();
        std::fs::write(
            b.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: vmm-b\n  version: \"1.0.0\"\n",
        )
        .unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        runtime.set_build_orchestrator(LoadAsIsOrchestrator);
        let err = runtime
            .add_module_with_blocking(
                ModuleIdent::any(Org::new("tatolab").unwrap(), Package::new("vmm-a").unwrap()),
                Strategy::Path {
                    path: a.clone(),
                    build: BuildPolicy::NeverBuild,
                },
            )
            .expect_err("version mismatch from dep must propagate");
        assert!(
            matches!(err, AddModuleError::VersionRangeUnsatisfied { ref module, found, .. }
                if module.name.as_str() == "vmm-b" && found == SemVer::new(1, 0, 0)),
            "expected VersionRangeUnsatisfied for vmm-b@1.0.0, got: {err:?}",
        );
    }

    // =========================================================================
    // Single-version-per-package gate (#1216)
    // =========================================================================

    /// A diamond where the two branches resolve the shared dependency to
    /// *different* concrete versions (B→D@1.0.0, C→D@1.1.0) must surface a
    /// typed [`AddModuleError::SingleVersionConflict`] naming both versions
    /// and both requirers — not a silent double-registration. Path deps
    /// enter with range `Any`, so the gate must compare concrete `SemVer`s.
    #[test]
    #[serial]
    fn diamond_conflicting_versions_error_single_version_conflict() {
        let home = tempfile::tempdir().unwrap();
        let pkg_root = tempfile::tempdir().unwrap();
        let a = pkg_root.path().join("a");
        let b = pkg_root.path().join("b");
        let c = pkg_root.path().join("c");
        let d1 = pkg_root.path().join("d1");
        let d2 = pkg_root.path().join("d2");
        for p in [&a, &b, &c, &d1, &d2] {
            std::fs::create_dir(p).unwrap();
        }
        std::fs::write(
            a.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: diamond-a\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/diamond-b\":\n    path: ../b\n\
             \x20 \"@tatolab/diamond-c\":\n    path: ../c\n",
        )
        .unwrap();
        std::fs::write(
            b.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: diamond-b\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/diamond-d\":\n    path: ../d1\n",
        )
        .unwrap();
        std::fs::write(
            c.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: diamond-c\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/diamond-d\":\n    path: ../d2\n",
        )
        .unwrap();
        std::fs::write(
            d1.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: diamond-d\n  version: \"1.0.0\"\n",
        )
        .unwrap();
        std::fs::write(
            d2.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: diamond-d\n  version: \"1.1.0\"\n",
        )
        .unwrap();

        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        runtime.set_build_orchestrator(LoadAsIsOrchestrator);
        let err = runtime
            .add_module_with_blocking(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("diamond-a").unwrap(),
                ),
                Strategy::Path {
                    path: a.clone(),
                    build: BuildPolicy::NeverBuild,
                },
            )
            .expect_err("diamond version conflict must error");
        match err {
            AddModuleError::SingleVersionConflict {
                package,
                existing_version,
                existing_required_by,
                conflicting_version,
                conflicting_required_by,
            } => {
                assert_eq!(package.name.as_str(), "diamond-d");
                // B is walked before C (BTreeMap key order), so D@1.0.0 lands
                // in the memo first and D@1.1.0 is the conflicting encounter.
                assert_eq!(existing_version, SemVer::new(1, 0, 0));
                assert_eq!(conflicting_version, SemVer::new(1, 1, 0));
                assert!(
                    existing_required_by.contains("diamond-b"),
                    "existing requirer should name diamond-b, got: {existing_required_by}",
                );
                assert!(
                    conflicting_required_by.contains("diamond-c"),
                    "conflicting requirer should name diamond-c, got: {conflicting_required_by}",
                );
            }
            other => panic!("expected SingleVersionConflict, got: {other:?}"),
        }
    }

    /// A diamond where both branches agree on the shared dependency's
    /// version (B→D@1.0.0, C→D@1.0.0, D→E) resolves cleanly and D's
    /// schema is registered. Two independent witnesses lock the skip:
    ///
    /// 1. `required_by.len() == 2` — C is recorded as a second requirer
    ///    of D's single resolution. NOTE: the commit preserves requirers
    ///    accumulated on the record (it never overwrite-resets them), so
    ///    this assertion locks the requirer-push in the skip arm, not the
    ///    absence of a re-walk.
    /// 2. E materialized exactly once — the re-walk witness. Reverting
    ///    the same-version skip makes C re-walk D's subtree, which
    ///    materializes E a second time and fails the count assertion.
    ///    (Schema presence can't witness this: registration is an
    ///    idempotent map overwrite.)
    #[test]
    #[serial]
    fn diamond_agreeing_versions_resolve_and_register_once() {
        const TYPE_D: &str = "DiamondAgreeDSchema";
        let home = tempfile::tempdir().unwrap();
        let pkg_root = tempfile::tempdir().unwrap();
        let a = pkg_root.path().join("a");
        let b = pkg_root.path().join("b");
        let c = pkg_root.path().join("c");
        let d = pkg_root.path().join("d");
        let e = pkg_root.path().join("e");
        for p in [&a, &b, &c, &d, &e] {
            std::fs::create_dir(p).unwrap();
        }
        std::fs::create_dir(d.join("schemas")).unwrap();
        std::fs::write(
            d.join("schemas/diamondagreedschema.yaml"),
            format!("metadata:\n  type: {TYPE_D}\n  expected_payload_bytes: 4096\n"),
        )
        .unwrap();
        std::fs::write(
            a.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: diamond-agree-a\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/diamond-agree-b\":\n    path: ../b\n\
             \x20 \"@tatolab/diamond-agree-c\":\n    path: ../c\n",
        )
        .unwrap();
        std::fs::write(
            b.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: diamond-agree-b\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/diamond-agree-d\":\n    path: ../d\n",
        )
        .unwrap();
        std::fs::write(
            c.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: diamond-agree-c\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/diamond-agree-d\":\n    path: ../d\n",
        )
        .unwrap();
        std::fs::write(
            d.join("streamlib.yaml"),
            format!(
                "package:\n  org: tatolab\n  name: diamond-agree-d\n  version: \"1.0.0\"\n\
                 dependencies:\n  \"@tatolab/diamond-agree-e\":\n    path: ../e\n\
                 schemas:\n  {TYPE_D}:\n    file: schemas/diamondagreedschema.yaml\n"
            ),
        )
        .unwrap();
        std::fs::write(
            e.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: diamond-agree-e\n  version: \"1.0.0\"\n",
        )
        .unwrap();

        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let materialize_counts = Arc::new(parking_lot::Mutex::new(std::collections::HashMap::<
            std::path::PathBuf,
            usize,
        >::new()));
        runtime.set_build_orchestrator(MaterializeCountingOrchestrator {
            counts: Arc::clone(&materialize_counts),
        });
        runtime
            .add_module_with_blocking(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("diamond-agree-a").unwrap(),
                ),
                Strategy::Path {
                    path: a.clone(),
                    build: BuildPolicy::NeverBuild,
                },
            )
            .expect("agreeing diamond must resolve cleanly");

        let canonical = format!("@tatolab/diamond-agree-d/{TYPE_D}");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical).is_some(),
            "expected D's schema {canonical} registered",
        );

        let d_ref = streamlib_idents::PackageRef::new(
            Org::new("tatolab").unwrap(),
            Package::new("diamond-agree-d").unwrap(),
        );
        let record = runtime
            .resolution_memo
            .committed_record(&d_ref)
            .expect("D must be committed in the resolution memo");
        assert_eq!(record.version, SemVer::new(1, 0, 0));
        // Locks the requirer-push in the same-version skip arm; the commit
        // preserves accumulated requirers (never overwrite-resets them).
        assert_eq!(
            record.required_by.len(),
            2,
            "both B and C must be recorded as requirers of the single D resolution",
        );
        // Re-walk witness: reverting the skip re-walks D's subtree from C
        // and materializes E a second time.
        let e_materializations = count_materializations_for_dir_named(&materialize_counts, "e");
        assert_eq!(
            e_materializations, 1,
            "D's subtree must be walked exactly once — a second E \
             materialization means the same-version skip was bypassed",
        );
    }

    /// The memo persists across successive `add_module` calls on the same
    /// runtime: adding X@1.0.0, then adding Y (which depends on X@2.0.0),
    /// conflicts even though the two loads are independent top-level calls.
    #[test]
    #[serial]
    fn cross_call_conflicting_versions_error() {
        let home = tempfile::tempdir().unwrap();
        let pkg_root = tempfile::tempdir().unwrap();
        let xa = pkg_root.path().join("xa");
        let xb = pkg_root.path().join("xb");
        let y = pkg_root.path().join("y");
        for p in [&xa, &xb, &y] {
            std::fs::create_dir(p).unwrap();
        }
        std::fs::write(
            xa.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: crosscall-x\n  version: \"1.0.0\"\n",
        )
        .unwrap();
        std::fs::write(
            xb.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: crosscall-x\n  version: \"2.0.0\"\n",
        )
        .unwrap();
        std::fs::write(
            y.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: crosscall-y\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/crosscall-x\":\n    path: ../xb\n",
        )
        .unwrap();

        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        runtime.set_build_orchestrator(LoadAsIsOrchestrator);

        runtime
            .add_module_with_blocking(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("crosscall-x").unwrap(),
                ),
                Strategy::Path {
                    path: xa.clone(),
                    build: BuildPolicy::NeverBuild,
                },
            )
            .expect("first add_module (x@1.0.0) must succeed");

        let err = runtime
            .add_module_with_blocking(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("crosscall-y").unwrap(),
                ),
                Strategy::Path {
                    path: y.clone(),
                    build: BuildPolicy::NeverBuild,
                },
            )
            .expect_err("second add_module pulling x@2.0.0 must conflict");
        match err {
            AddModuleError::SingleVersionConflict {
                package,
                existing_version,
                existing_required_by,
                conflicting_version,
                ..
            } => {
                assert_eq!(package.name.as_str(), "crosscall-x");
                assert_eq!(existing_version, SemVer::new(1, 0, 0));
                assert_eq!(conflicting_version, SemVer::new(2, 0, 0));
                assert!(
                    existing_required_by.contains("top-level add_module"),
                    "existing requirer must name the top-level add_module call, \
                     got: {existing_required_by}",
                );
            }
            other => panic!("expected SingleVersionConflict, got: {other:?}"),
        }
    }

    /// Re-adding the same package at the same version is a cheap idempotent
    /// no-op: both calls succeed, and the memo records the single X
    /// resolution with both top-level `add_module` calls as requirers.
    #[test]
    #[serial]
    fn idempotent_re_add_same_version_succeeds() {
        let home = tempfile::tempdir().unwrap();
        let pkg = tempfile::tempdir().unwrap();
        std::fs::write(
            pkg.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: readd-x\n  version: \"1.0.0\"\n",
        )
        .unwrap();

        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        runtime.set_build_orchestrator(LoadAsIsOrchestrator);

        let ident = || {
            ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("readd-x").unwrap(),
            )
        };
        let strategy = || Strategy::Path {
            path: pkg.path().to_path_buf(),
            build: BuildPolicy::NeverBuild,
        };
        runtime
            .add_module_with_blocking(ident(), strategy())
            .expect("first add_module must succeed");
        runtime
            .add_module_with_blocking(ident(), strategy())
            .expect("idempotent re-add at same version must succeed");

        let x_ref = streamlib_idents::PackageRef::new(
            Org::new("tatolab").unwrap(),
            Package::new("readd-x").unwrap(),
        );
        let record = runtime
            .resolution_memo
            .committed_record(&x_ref)
            .expect("X must be committed");
        assert_eq!(record.version, SemVer::new(1, 0, 0));
        assert_eq!(
            record.required_by.len(),
            2,
            "both top-level add_module calls must be recorded as requirers",
        );
    }

    /// A load that fails mid-registration must NOT poison the memo: the
    /// in-flight placeholder guard clears the entry on the failure exit,
    /// so a retry re-runs the full registration instead of hitting the
    /// same-version skip and silently returning `Ok` for a
    /// never-registered package. Here X declares a schema whose file is
    /// missing, so `add_module` fails between the gate and the commit;
    /// after fixing the file, the retry succeeds and the schema actually
    /// registers — the user-facing retry property. Mentally reverting the
    /// guard (commit-at-gate) leaves X in the memo and makes the retry a
    /// silent no-op with no schema registered.
    #[test]
    #[serial]
    fn failed_load_does_not_poison_the_memo_and_retry_succeeds() {
        const TYPE_POISON: &str = "PoisonRetrySchema";
        let home = tempfile::tempdir().unwrap();
        let pkg = tempfile::tempdir().unwrap();
        // Declares a schema pointing at a file that does not exist →
        // `register_package_schemas` errors between the gate and the
        // commit at the end of the walk body.
        std::fs::write(
            pkg.path().join("streamlib.yaml"),
            format!(
                "package:\n  org: tatolab\n  name: poison-x\n  version: \"1.0.0\"\n\
                 schemas:\n  {TYPE_POISON}:\n    file: schemas/poisonretryschema.yaml\n"
            ),
        )
        .unwrap();

        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        runtime.set_build_orchestrator(LoadAsIsOrchestrator);
        let ident = || {
            ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("poison-x").unwrap(),
            )
        };
        let strategy = || Strategy::Path {
            path: pkg.path().to_path_buf(),
            build: BuildPolicy::NeverBuild,
        };
        let err = runtime
            .add_module_with_blocking(ident(), strategy())
            .expect_err("missing schema file must fail the load");
        assert!(
            matches!(err, AddModuleError::LoadProjectFailed { .. }),
            "expected LoadProjectFailed from the missing schema, got: {err:?}",
        );

        let x_ref = streamlib_idents::PackageRef::new(
            Org::new("tatolab").unwrap(),
            Package::new("poison-x").unwrap(),
        );
        assert!(
            !runtime.resolution_memo.contains_package(&x_ref),
            "a failed load must not leave a resolution-memo entry",
        );

        // Fix the package and retry on the SAME runtime — the retry must
        // re-run registration (not skip), so the schema becomes visible.
        std::fs::create_dir(pkg.path().join("schemas")).unwrap();
        std::fs::write(
            pkg.path().join("schemas/poisonretryschema.yaml"),
            format!("metadata:\n  type: {TYPE_POISON}\n  expected_payload_bytes: 4096\n"),
        )
        .unwrap();
        runtime
            .add_module_with_blocking(ident(), strategy())
            .expect("retry after fixing the schema file must succeed");
        let canonical = format!("@tatolab/poison-x/{TYPE_POISON}");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical).is_some(),
            "retry must actually register the schema {canonical}",
        );
        assert!(
            runtime.resolution_memo.committed_record(&x_ref).is_some(),
            "retry must commit the resolution",
        );
    }

    // =====================================================================
    // Concurrent loads — single-version gate under overlap. Deterministic:
    // orchestrator barriers / gates control the overlap window, never
    // timing luck.
    // =====================================================================

    /// Two CONCURRENT top-level loads sharing a transitive dep at the
    /// same version (TA→D, TB→D, D→E) both succeed with a single
    /// resolution of D. The rendezvous barrier forces both walks past
    /// D's materialize before either reaches the gate, so the two gate
    /// calls overlap deterministically: exactly one wins the placeholder
    /// insert; the other skips (in-flight or committed — both correct).
    /// E's materialize count == 1 witnesses that D's subtree was walked
    /// exactly once — the double-registration TOCTOU this gate closes.
    #[test]
    #[serial]
    fn concurrent_loads_agreeing_shared_dep_register_once() {
        const TYPE_D: &str = "ConcAgreeDSchema";
        let home = tempfile::tempdir().unwrap();
        let pkg_root = tempfile::tempdir().unwrap();
        let ta = pkg_root.path().join("ta");
        let tb = pkg_root.path().join("tb");
        let d = pkg_root.path().join("d");
        let e = pkg_root.path().join("e");
        for p in [&ta, &tb, &d, &e] {
            std::fs::create_dir(p).unwrap();
        }
        std::fs::create_dir(d.join("schemas")).unwrap();
        std::fs::write(
            d.join("schemas/concagreedschema.yaml"),
            format!("metadata:\n  type: {TYPE_D}\n  expected_payload_bytes: 4096\n"),
        )
        .unwrap();
        std::fs::write(
            ta.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-agree-ta\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/conc-agree-d\":\n    path: ../d\n",
        )
        .unwrap();
        std::fs::write(
            tb.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-agree-tb\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/conc-agree-d\":\n    path: ../d\n",
        )
        .unwrap();
        std::fs::write(
            d.join("streamlib.yaml"),
            format!(
                "package:\n  org: tatolab\n  name: conc-agree-d\n  version: \"1.0.0\"\n\
                 dependencies:\n  \"@tatolab/conc-agree-e\":\n    path: ../e\n\
                 schemas:\n  {TYPE_D}:\n    file: schemas/concagreedschema.yaml\n"
            ),
        )
        .unwrap();
        std::fs::write(
            e.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-agree-e\n  version: \"1.0.0\"\n",
        )
        .unwrap();

        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let materialize_counts = Arc::new(parking_lot::Mutex::new(std::collections::HashMap::<
            std::path::PathBuf,
            usize,
        >::new()));
        runtime.set_build_orchestrator(RendezvousLoadAsIsOrchestrator {
            rendezvous_dir_names: vec!["d".to_string()],
            rendezvous: std::sync::Barrier::new(2),
            counts: Arc::clone(&materialize_counts),
        });

        let strategy = |path: &std::path::Path| Strategy::Path {
            path: path.to_path_buf(),
            build: BuildPolicy::NeverBuild,
        };
        let added_ta = runtime.add_module_with(
            ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("conc-agree-ta").unwrap(),
            ),
            strategy(&ta),
        );
        let added_tb = runtime.add_module_with(
            ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("conc-agree-tb").unwrap(),
            ),
            strategy(&tb),
        );
        let handle = runtime.tokio_runtime_variant.handle();
        let (result_ta, result_tb) = handle.block_on(async { tokio::join!(added_ta, added_tb) });
        result_ta.expect("TA load must succeed");
        result_tb.expect("TB load must succeed");

        let d_ref = streamlib_idents::PackageRef::new(
            Org::new("tatolab").unwrap(),
            Package::new("conc-agree-d").unwrap(),
        );
        let record = runtime
            .resolution_memo
            .committed_record(&d_ref)
            .expect("D must be committed");
        assert_eq!(record.version, SemVer::new(1, 0, 0));
        assert_eq!(
            record.required_by.len(),
            2,
            "both concurrent loads must be recorded as requirers of D",
        );
        assert_eq!(
            count_materializations_for_dir_named(&materialize_counts, "e"),
            1,
            "D's subtree must be walked exactly once across both concurrent loads",
        );
        let canonical = format!("@tatolab/conc-agree-d/{TYPE_D}");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical).is_some(),
            "expected D's schema {canonical} registered",
        );
    }

    /// Two CONCURRENT top-level loads pulling DIFFERENT versions of the
    /// same package (TA→D@1.0.0, TB→D@1.1.0) produce exactly one
    /// SingleVersionConflict — never a silent last-commit-wins
    /// double-registration. The rendezvous barrier guarantees both walks
    /// hit the gate window together; whichever loses the insert race
    /// conflicts against the winner's placeholder or committed record.
    #[test]
    #[serial]
    fn concurrent_loads_conflicting_shared_dep_exactly_one_conflict() {
        let home = tempfile::tempdir().unwrap();
        let pkg_root = tempfile::tempdir().unwrap();
        let ta = pkg_root.path().join("ta");
        let tb = pkg_root.path().join("tb");
        let d1 = pkg_root.path().join("d1");
        let d2 = pkg_root.path().join("d2");
        for p in [&ta, &tb, &d1, &d2] {
            std::fs::create_dir(p).unwrap();
        }
        std::fs::write(
            ta.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-conflict-ta\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/conc-conflict-d\":\n    path: ../d1\n",
        )
        .unwrap();
        std::fs::write(
            tb.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-conflict-tb\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/conc-conflict-d\":\n    path: ../d2\n",
        )
        .unwrap();
        std::fs::write(
            d1.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-conflict-d\n  version: \"1.0.0\"\n",
        )
        .unwrap();
        std::fs::write(
            d2.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-conflict-d\n  version: \"1.1.0\"\n",
        )
        .unwrap();

        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let materialize_counts = Arc::new(parking_lot::Mutex::new(std::collections::HashMap::<
            std::path::PathBuf,
            usize,
        >::new()));
        runtime.set_build_orchestrator(RendezvousLoadAsIsOrchestrator {
            rendezvous_dir_names: vec!["d1".to_string(), "d2".to_string()],
            rendezvous: std::sync::Barrier::new(2),
            counts: Arc::clone(&materialize_counts),
        });

        let added_ta = runtime.add_module_with(
            ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("conc-conflict-ta").unwrap(),
            ),
            Strategy::Path {
                path: ta.clone(),
                build: BuildPolicy::NeverBuild,
            },
        );
        let added_tb = runtime.add_module_with(
            ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("conc-conflict-tb").unwrap(),
            ),
            Strategy::Path {
                path: tb.clone(),
                build: BuildPolicy::NeverBuild,
            },
        );
        let handle = runtime.tokio_runtime_variant.handle();
        let (result_ta, result_tb) = handle.block_on(async { tokio::join!(added_ta, added_tb) });

        let results = [result_ta.map(|_| ()), result_tb.map(|_| ())];
        let conflict_count = results
            .iter()
            .filter(|r| {
                matches!(
                    r,
                    Err(AddModuleError::SingleVersionConflict { package, .. })
                        if package.name.as_str() == "conc-conflict-d"
                )
            })
            .count();
        let ok_count = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(
            (ok_count, conflict_count),
            (1, 1),
            "exactly one load must succeed and one must conflict on D, got: {results:?}",
        );
        // The winner's resolution of D is committed; the loser's own
        // top-level placeholder was cleared by its drop-guard.
        let d_ref = streamlib_idents::PackageRef::new(
            Org::new("tatolab").unwrap(),
            Package::new("conc-conflict-d").unwrap(),
        );
        assert!(
            runtime.resolution_memo.committed_record(&d_ref).is_some(),
            "the winning load's D resolution must be committed",
        );
    }

    /// Deterministic in-flight skip: TA owns D (held in flight because
    /// D's dep E blocks in the orchestrator), TB gates D, sees the
    /// same-version placeholder, skips locally, and verifies at the end
    /// of its walk. After the owner commits, the waiter returns Ok. The
    /// requirer-count poll makes the interleaving deterministic — TB is
    /// PROVEN to have gated against the in-flight placeholder before the
    /// release.
    #[test]
    #[serial]
    fn concurrent_load_waiter_succeeds_after_owner_commits() {
        let home = tempfile::tempdir().unwrap();
        let pkg_root = tempfile::tempdir().unwrap();
        let ta = pkg_root.path().join("ta");
        let tb = pkg_root.path().join("tb");
        let d = pkg_root.path().join("d");
        let e = pkg_root.path().join("e");
        for p in [&ta, &tb, &d, &e] {
            std::fs::create_dir(p).unwrap();
        }
        std::fs::write(
            ta.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-wait-ta\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/conc-wait-d\":\n    path: ../d\n",
        )
        .unwrap();
        std::fs::write(
            tb.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-wait-tb\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/conc-wait-d\":\n    path: ../d\n",
        )
        .unwrap();
        std::fs::write(
            d.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-wait-d\n  version: \"1.0.0\"\n\
             dependencies:\n  \"@tatolab/conc-wait-e\":\n    path: ../e\n",
        )
        .unwrap();
        std::fs::write(
            e.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-wait-e\n  version: \"1.0.0\"\n",
        )
        .unwrap();

        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        runtime.set_build_orchestrator(GatedSubDependencyOrchestrator {
            gated_dir_name: "e".to_string(),
            release: parking_lot::Mutex::new(Some(release_rx)),
            fail_after_release: false,
        });

        let d_ref = streamlib_idents::PackageRef::new(
            Org::new("tatolab").unwrap(),
            Package::new("conc-wait-d").unwrap(),
        );
        let added_ta = runtime.add_module_with(
            ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("conc-wait-ta").unwrap(),
            ),
            Strategy::Path {
                path: ta.clone(),
                build: BuildPolicy::NeverBuild,
            },
        );
        // TA holds D in flight (blocked inside E's materialize).
        assert!(
            poll_until(std::time::Duration::from_secs(10), || {
                runtime.resolution_memo.in_flight_requirer_count(&d_ref) == Some(1)
            }),
            "TA must reach D's in-flight placeholder before TB starts",
        );
        let added_tb = runtime.add_module_with(
            ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("conc-wait-tb").unwrap(),
            ),
            Strategy::Path {
                path: tb.clone(),
                build: BuildPolicy::NeverBuild,
            },
        );
        // TB gated against the IN-FLIGHT placeholder (its requirer landed
        // while D was still in flight) — the deterministic skip witness.
        assert!(
            poll_until(std::time::Duration::from_secs(10), || {
                runtime.resolution_memo.in_flight_requirer_count(&d_ref) == Some(2)
            }),
            "TB must record its requirer on D's in-flight placeholder",
        );
        release_tx.send(()).unwrap();

        let handle = runtime.tokio_runtime_variant.handle();
        let (result_ta, result_tb) = handle.block_on(async { tokio::join!(added_ta, added_tb) });
        result_ta.expect("owner load must succeed");
        result_tb.expect("waiter load must succeed after the owner commits");

        let record = runtime
            .resolution_memo
            .committed_record(&d_ref)
            .expect("D must be committed");
        assert_eq!(record.required_by.len(), 2);
    }

    /// Owner-failure verification: same shape as the success twin, but
    /// E's materialize fails after release. The owner load fails; the
    /// waiter — which skipped D as in-flight — must fail LOUDLY with the
    /// typed concurrent-load error (never Ok over an unregistered
    /// package, never a hang), and the drop-guard must have cleared D.
    #[test]
    #[serial]
    fn concurrent_load_owner_failure_fails_waiter_loudly() {
        let home = tempfile::tempdir().unwrap();
        let pkg_root = tempfile::tempdir().unwrap();
        let ta = pkg_root.path().join("ta");
        let tb = pkg_root.path().join("tb");
        let d = pkg_root.path().join("d");
        let e = pkg_root.path().join("e");
        for p in [&ta, &tb, &d, &e] {
            std::fs::create_dir(p).unwrap();
        }
        std::fs::write(
            ta.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-fail-ta\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/conc-fail-d\":\n    path: ../d\n",
        )
        .unwrap();
        std::fs::write(
            tb.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-fail-tb\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/conc-fail-d\":\n    path: ../d\n",
        )
        .unwrap();
        std::fs::write(
            d.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-fail-d\n  version: \"1.0.0\"\n\
             dependencies:\n  \"@tatolab/conc-fail-e\":\n    path: ../e\n",
        )
        .unwrap();
        std::fs::write(
            e.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-fail-e\n  version: \"1.0.0\"\n",
        )
        .unwrap();

        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        runtime.set_build_orchestrator(GatedSubDependencyOrchestrator {
            gated_dir_name: "e".to_string(),
            release: parking_lot::Mutex::new(Some(release_rx)),
            fail_after_release: true,
        });

        let d_ref = streamlib_idents::PackageRef::new(
            Org::new("tatolab").unwrap(),
            Package::new("conc-fail-d").unwrap(),
        );
        let added_ta = runtime.add_module_with(
            ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("conc-fail-ta").unwrap(),
            ),
            Strategy::Path {
                path: ta.clone(),
                build: BuildPolicy::NeverBuild,
            },
        );
        assert!(
            poll_until(std::time::Duration::from_secs(10), || {
                runtime.resolution_memo.in_flight_requirer_count(&d_ref) == Some(1)
            }),
            "TA must reach D's in-flight placeholder before TB starts",
        );
        let added_tb = runtime.add_module_with(
            ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("conc-fail-tb").unwrap(),
            ),
            Strategy::Path {
                path: tb.clone(),
                build: BuildPolicy::NeverBuild,
            },
        );
        assert!(
            poll_until(std::time::Duration::from_secs(10), || {
                runtime.resolution_memo.in_flight_requirer_count(&d_ref) == Some(2)
            }),
            "TB must record its requirer on D's in-flight placeholder",
        );
        release_tx.send(()).unwrap();

        let handle = runtime.tokio_runtime_variant.handle();
        let (result_ta, result_tb) = handle.block_on(async { tokio::join!(added_ta, added_tb) });
        let owner_err = result_ta.expect_err("owner load must fail (injected E failure)");
        assert!(
            matches!(owner_err, AddModuleError::MaterializeFailed { .. }),
            "owner must surface the injected materialize failure, got: {owner_err:?}",
        );
        let waiter_err = result_tb.expect_err("waiter must fail loudly when the owner fails");
        assert!(
            matches!(
                waiter_err,
                AddModuleError::ConcurrentLoadOfSkippedDependencyFailed { ref package, version }
                    if package.name.as_str() == "conc-fail-d"
                        && version == SemVer::new(1, 0, 0)
            ),
            "expected ConcurrentLoadOfSkippedDependencyFailed for D, got: {waiter_err:?}",
        );
        assert!(
            !runtime.resolution_memo.contains_package(&d_ref),
            "the failed owner's drop-guard must clear D's placeholder",
        );
    }

    /// `start()` must refuse to run the graph while a module load is
    /// still in flight. The loading set is `pub(crate)` and is populated
    /// by `add_module_with` at call time; simulating an in-flight entry
    /// directly tests the guard deterministically (no spawn/race).
    /// Reverting the guard (the `pending_module_loads` check at the top
    /// of `start`) makes `start()` proceed and this `expect_err` fails.
    #[test]
    #[serial]
    fn start_refuses_while_a_module_is_still_loading() {
        let runtime = Runner::new().expect("Runner::new");
        let pkg = streamlib_idents::PackageRef::new(
            Org::new("tatolab").unwrap(),
            Package::new("guard-pkg").unwrap(),
        );
        let ident = ModuleIdent::any(
            Org::new("tatolab").unwrap(),
            Package::new("guard-pkg").unwrap(),
        );
        runtime.loading_modules.lock().insert(pkg, (0, ident));

        let err = runtime
            .start()
            .expect_err("start() must refuse while a module is still loading");
        let msg = format!("{err}");
        assert!(
            msg.contains("guard-pkg") && msg.to_lowercase().contains("still loading"),
            "expected a ModulesStillLoading error naming guard-pkg, got: {msg}"
        );

        runtime.loading_modules.lock().clear();
    }

    /// `add_module_blocking` from inside a tokio runtime must return a
    /// typed `BlockingCallFromAsyncContext` error — NEVER panic (a naive
    /// `block_on` would). Locks the `ExternalTokioHandle` variant guard.
    #[test]
    fn add_module_blocking_in_tokio_context_returns_typed_error() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // Inside block_on, Runner::new auto-detects the external tokio
            // handle.
            let runtime = Runner::new().expect("Runner::new");
            let err = runtime
                .add_module_blocking(ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("blocking-in-async").unwrap(),
                ))
                .expect_err("blocking load from async context must error, not panic");
            assert!(
                matches!(err, AddModuleError::BlockingCallFromAsyncContext { .. }),
                "expected BlockingCallFromAsyncContext, got: {err:?}",
            );
        });
    }

    #[test]
    #[serial]
    fn remove_module_unknown_package_is_module_not_loaded() {
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .remove_module(ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("remove-module-unknown").unwrap(),
            ))
            .expect_err("remove_module of a never-loaded package must error");
        assert!(
            matches!(
                err,
                RemoveModuleError::ModuleNotLoaded { ref module, loaded_version: None }
                    if module.name.as_str() == "remove-module-unknown"
            ),
            "got: {err:?}",
        );
    }

    // =====================================================================
    // Transactional registration: a failed load leaves zero partial state
    // =====================================================================

    /// Byte-equivalence snapshot of every registry a module load touches:
    /// schema bodies, processor descriptors, the processor factory's
    /// port-schema universe, ledger packages, and the calling Runner's
    /// resolution-memo package set. Two snapshots compare equal iff a
    /// failed load left zero residue.
    #[derive(Debug, PartialEq, Eq)]
    struct RegistrySnapshot {
        schema_bodies_by_canonical_id: std::collections::BTreeMap<String, String>,
        processor_descriptor_debug_by_ident: std::collections::BTreeMap<String, String>,
        port_schema_universe: std::collections::BTreeSet<String>,
        ledger_packages: std::collections::BTreeSet<String>,
        resolution_memo_packages: std::collections::BTreeSet<String>,
    }

    impl RegistrySnapshot {
        fn capture(resolution_memo: &ResolutionMemo) -> Self {
            let schema_bodies_by_canonical_id =
                crate::core::embedded_schemas::list_embedded_schema_names()
                    .into_iter()
                    .map(|name| {
                        let body =
                            crate::core::embedded_schemas::get_embedded_schema_definition(&name)
                                .map(|b| b.to_string())
                                .unwrap_or_default();
                        (name, body)
                    })
                    .collect();
            let processor_descriptor_debug_by_ident = crate::core::processors::PROCESSOR_REGISTRY
                .list_registered()
                .into_iter()
                .map(|desc| (desc.name.to_string(), format!("{desc:?}")))
                .collect();
            let port_schema_universe = crate::core::processors::PROCESSOR_REGISTRY
                .known_schemas()
                .into_iter()
                .map(|spec| format!("{spec:?}"))
                .collect();
            let ledger_packages = ledger::loaded_module_registration_ledger_packages()
                .into_iter()
                .map(|p| p.to_string())
                .collect();
            let resolution_memo_packages = resolution_memo
                .resolved_package_refs()
                .into_iter()
                .map(|p| p.to_string())
                .collect();
            Self {
                schema_bodies_by_canonical_id,
                processor_descriptor_debug_by_ident,
                port_schema_universe,
                ledger_packages,
                resolution_memo_packages,
            }
        }
    }

    fn tatolab_ident(name: &str) -> ModuleIdent {
        ModuleIdent::any(Org::new("tatolab").unwrap(), Package::new(name).unwrap())
    }

    fn path_strategy_never_build(dir: &std::path::Path) -> Strategy {
        Strategy::Path {
            path: dir.to_path_buf(),
            build: BuildPolicy::NeverBuild,
        }
    }

    /// Failure injected at the SCHEMA phase, after the first schema file
    /// staged: the second schema file has no `metadata` block, so the walk
    /// fails mid-schema-staging. Zero residue; the fixed package reloads.
    #[test]
    #[serial]
    fn failed_load_at_schema_phase_leaves_zero_registry_residue() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("txn-schema-fail");
        std::fs::create_dir_all(pkg.join("schemas")).unwrap();
        // BTreeMap key order stages TxnAGoodSchema BEFORE TxnBBadSchema
        // fails — the injection point sits after partial staging.
        std::fs::write(
            pkg.join("schemas/a_good.yaml"),
            "metadata:\n  type: TxnAGoodSchema\n",
        )
        .unwrap();
        std::fs::write(pkg.join("schemas/b_bad.yaml"), "properties: {}\n").unwrap();
        std::fs::write(
            pkg.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: txn-schema-fail\n  version: \"1.0.0\"\n\
             schemas:\n  TxnAGoodSchema:\n    file: schemas/a_good.yaml\n\
             \x20 TxnBBadSchema:\n    file: schemas/b_bad.yaml\n",
        )
        .unwrap();

        let before = RegistrySnapshot::capture(&runtime.resolution_memo);
        runtime
            .add_module_with_blocking(
                tatolab_ident("txn-schema-fail"),
                path_strategy_never_build(&pkg),
            )
            .expect_err("a schema without a metadata block must fail the load");
        let after = RegistrySnapshot::capture(&runtime.resolution_memo);
        assert_eq!(
            before, after,
            "a failed load must leave the registries byte-equivalent to \
             before the attempt"
        );
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(
                "@tatolab/txn-schema-fail/TxnAGoodSchema"
            )
            .is_none(),
            "the schema staged before the failure must not be visible"
        );

        // Fix the broken schema; the same package must now load cleanly.
        std::fs::write(
            pkg.join("schemas/b_bad.yaml"),
            "metadata:\n  type: TxnBBadSchema\n",
        )
        .unwrap();
        runtime
            .add_module_with_blocking(
                tatolab_ident("txn-schema-fail"),
                path_strategy_never_build(&pkg),
            )
            .expect("reload of the fixed package must succeed");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(
                "@tatolab/txn-schema-fail/TxnAGoodSchema"
            )
            .is_some(),
            "both schemas must be visible after the successful reload"
        );
    }

    /// Failure injected at the DEP-WALK phase: the root's schemas staged,
    /// then a dependency with no manifest fails the walk. The root's
    /// schemas must not be visible.
    #[test]
    #[serial]
    fn failed_load_at_dep_walk_phase_rolls_back_root_schemas() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        runtime.set_build_orchestrator(LoadAsIsOrchestrator);
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("txn-depfail-root");
        std::fs::create_dir_all(root.join("schemas")).unwrap();
        std::fs::write(
            root.join("schemas/root_schema.yaml"),
            "metadata:\n  type: TxnDepFailRootSchema\n",
        )
        .unwrap();
        std::fs::write(
            root.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: txn-depfail-root\n  version: \"1.0.0\"\n\
             schemas:\n  TxnDepFailRootSchema:\n    file: schemas/root_schema.yaml\n\
             dependencies:\n  \"@tatolab/txn-depfail-missing\":\n    path: ../missing\n",
        )
        .unwrap();

        let before = RegistrySnapshot::capture(&runtime.resolution_memo);
        runtime
            .add_module_with_blocking(
                tatolab_ident("txn-depfail-root"),
                path_strategy_never_build(&root),
            )
            .expect_err("a dep with no manifest must fail the load");
        let after = RegistrySnapshot::capture(&runtime.resolution_memo);
        assert_eq!(before, after, "dep-walk failure must leave zero residue");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(
                "@tatolab/txn-depfail-root/TxnDepFailRootSchema"
            )
            .is_none(),
            "the root's schemas (staged before the dep walk) must not be visible"
        );
    }

    /// Write a package with one schema + two TypeScript processors where
    /// the SECOND processor's config schema is unresolvable — the walk
    /// stages the schema and the first processor, then fails.
    fn write_processor_phase_failure_package(pkg: &std::path::Path) {
        std::fs::create_dir_all(pkg.join("schemas")).unwrap();
        std::fs::write(
            pkg.join("schemas/txn_proc_config.yaml"),
            "metadata:\n  type: TxnProcConfigSchema\n",
        )
        .unwrap();
        std::fs::write(
            pkg.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: txn-procfail\n  version: \"1.0.0\"\n\
             schemas:\n  TxnProcConfigSchema:\n    file: schemas/txn_proc_config.yaml\n\
             processors:\n\
             \x20 - name: TxnAlphaProcessor\n\
             \x20   runtime: typescript\n\
             \x20   entrypoint: main.ts\n\
             \x20   execution: manual\n\
             \x20   config:\n\
             \x20     name: config\n\
             \x20     schema: TxnProcConfigSchema\n\
             \x20 - name: TxnBetaProcessor\n\
             \x20   runtime: typescript\n\
             \x20   entrypoint: main.ts\n\
             \x20   execution: manual\n\
             \x20   config:\n\
             \x20     name: config\n\
             \x20     schema: TxnMissingConfigSchema\n",
        )
        .unwrap();
    }

    /// Failure injected at the PROCESSOR phase, after the first processor
    /// staged: no processors and no schemas may be visible.
    #[test]
    #[serial]
    fn failed_load_at_processor_phase_rolls_back_schemas_and_processors() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("txn-procfail");
        write_processor_phase_failure_package(&pkg);

        let before = RegistrySnapshot::capture(&runtime.resolution_memo);
        runtime
            .add_module_with_blocking(
                tatolab_ident("txn-procfail"),
                path_strategy_never_build(&pkg),
            )
            .expect_err("an unresolvable config schema must fail the load");
        let after = RegistrySnapshot::capture(&runtime.resolution_memo);
        assert_eq!(
            before, after,
            "processor-phase failure must leave zero residue"
        );
        assert!(
            !crate::core::processors::PROCESSOR_REGISTRY
                .list_registered()
                .iter()
                .any(|d| d.name.r#type.as_str() == "TxnAlphaProcessor"),
            "the processor staged before the failure must not be visible"
        );
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(
                "@tatolab/txn-procfail/TxnProcConfigSchema"
            )
            .is_none(),
            "the package's schema must not be visible after the failure"
        );
    }

    /// Two subprocess (TypeScript) processors declared with the SAME
    /// short name compose the same `processor_type` ident. The end-of-walk
    /// Dynamic-collision gate must fail the load loud with a typed
    /// `DuplicateProcessorTypeInModule`, leaving zero residue — never a
    /// silently-incomplete Ok (which the old commit-time `register_dynamic`
    /// Err + bug-grade-log-and-continue would have produced). Mentally
    /// revert `validate_no_dynamic_processor_collisions` and this fails:
    /// the load returns Ok with only the first processor registered.
    #[test]
    #[serial]
    fn duplicate_dynamic_processor_name_fails_loud_zero_residue() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("txn-dupname");
        std::fs::create_dir_all(&pkg).unwrap();
        // Both processors share the name TxnDupProcessor → identical ident.
        std::fs::write(
            pkg.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: txn-dupname\n  version: \"1.0.0\"\n\
             processors:\n\
             \x20 - name: TxnDupProcessor\n\
             \x20   runtime: typescript\n\
             \x20   entrypoint: main.ts\n\
             \x20   execution: manual\n\
             \x20 - name: TxnDupProcessor\n\
             \x20   runtime: typescript\n\
             \x20   entrypoint: main.ts\n\
             \x20   execution: manual\n",
        )
        .unwrap();

        let before = RegistrySnapshot::capture(&runtime.resolution_memo);
        let err = runtime
            .add_module_with_blocking(
                tatolab_ident("txn-dupname"),
                path_strategy_never_build(&pkg),
            )
            .expect_err("a duplicate subprocess processor name must fail the load");
        assert!(
            matches!(
                err,
                AddModuleError::DuplicateProcessorTypeInModule { ref package, ref processor_type }
                    if package.name.as_str() == "txn-dupname"
                        && processor_type.r#type.as_str() == "TxnDupProcessor"
            ),
            "expected DuplicateProcessorTypeInModule, got: {err:?}",
        );
        let after = RegistrySnapshot::capture(&runtime.resolution_memo);
        assert_eq!(
            before, after,
            "a duplicate-name load must leave zero registry residue"
        );
        assert!(
            !crate::core::processors::PROCESSOR_REGISTRY
                .list_registered()
                .iter()
                .any(|d| d.name.r#type.as_str() == "TxnDupProcessor"),
            "neither copy of the duplicated processor may be registered"
        );
    }

    /// A subprocess processor whose composed ident is ALREADY globally
    /// registered (by non-module-load code) must be refused with the typed
    /// `ProcessorTypeAlreadyRegistered`, zero residue. Locks the (b) arm of
    /// the collision gate.
    #[test]
    #[serial]
    fn dynamic_processor_colliding_with_global_registration_fails_loud() {
        use crate::core::descriptors::{ProcessorDescriptor, SchemaIdent, TypeName};

        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");

        // Pre-register @tatolab/txn-global/TxnGlobalProcessor@1.0.0 out of
        // band, so a later module load that composes the same ident collides.
        let colliding_ident = SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("txn-global").unwrap(),
            TypeName::new("TxnGlobalProcessor").unwrap(),
            SemVer::new(1, 0, 0),
        );
        crate::core::processors::PROCESSOR_REGISTRY
            .register_dynamic(
                ProcessorDescriptor::new(colliding_ident.clone(), "pre-registered collision"),
                Box::new(|_node| {
                    Err(crate::core::Error::Configuration(
                        "test constructor never invoked".into(),
                    ))
                }),
            )
            .expect("out-of-band pre-registration must succeed");

        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("txn-global");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: txn-global\n  version: \"1.0.0\"\n\
             processors:\n\
             \x20 - name: TxnGlobalProcessor\n\
             \x20   runtime: typescript\n\
             \x20   entrypoint: main.ts\n\
             \x20   execution: manual\n",
        )
        .unwrap();

        let before = RegistrySnapshot::capture(&runtime.resolution_memo);
        let err = runtime
            .add_module_with_blocking(
                tatolab_ident("txn-global"),
                path_strategy_never_build(&pkg),
            )
            .expect_err("a globally-colliding subprocess processor must fail the load");
        assert!(
            matches!(
                err,
                AddModuleError::ProcessorTypeAlreadyRegistered { ref processor_type, .. }
                    if processor_type.r#type.as_str() == "TxnGlobalProcessor"
            ),
            "expected ProcessorTypeAlreadyRegistered, got: {err:?}",
        );
        let after = RegistrySnapshot::capture(&runtime.resolution_memo);
        assert_eq!(
            before, after,
            "a globally-colliding load must leave zero residue (the pre-registered \
             ident stays, nothing new lands)"
        );

        // Cleanup: unregister the out-of-band ident so the process-global
        // registry is clean for later #[serial] tests.
        crate::core::processors::PROCESSOR_REGISTRY
            .unregister_processor_types(std::slice::from_ref(&colliding_ident));
    }

    /// Same-load diamond regression: A→{B,C}→D resolves D once, with no
    /// self-wait. With the whole-load memo commit, D's placeholder stays
    /// in flight for the entire walk — the second encounter (via C) must
    /// take the SkipOwnedByThisLoad arm. Mentally revert that arm and the
    /// load records a wait entry against its OWN completion signal, which
    /// only publishes after the wait phase — the timeout below fires.
    #[test]
    #[serial]
    fn same_load_diamond_dependency_registers_once_without_self_wait() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let counts = std::sync::Arc::new(parking_lot::Mutex::new(std::collections::HashMap::<
            std::path::PathBuf,
            usize,
        >::new()));
        runtime.set_build_orchestrator(MaterializeCountingOrchestrator {
            counts: std::sync::Arc::clone(&counts),
        });
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        let b = tmp.path().join("b");
        let c = tmp.path().join("c");
        let d = tmp.path().join("d");
        let e = tmp.path().join("e");
        for p in [&a, &b, &c, &d, &e] {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(
            a.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: diamond-a\n  version: \"1.0.0\"\n\
             dependencies:\n  \"@tatolab/diamond-b\":\n    path: ../b\n\
             \x20 \"@tatolab/diamond-c\":\n    path: ../c\n",
        )
        .unwrap();
        std::fs::write(
            b.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: diamond-b\n  version: \"1.0.0\"\n\
             dependencies:\n  \"@tatolab/diamond-d\":\n    path: ../d\n",
        )
        .unwrap();
        std::fs::write(
            c.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: diamond-c\n  version: \"1.0.0\"\n\
             dependencies:\n  \"@tatolab/diamond-d\":\n    path: ../d\n",
        )
        .unwrap();
        // D carries a sub-dependency E. Materialization runs BEFORE the
        // single-version gate (the gate needs the resolved on-disk
        // version), so D itself materializes once per encounter by
        // design — E's materialize count is the "subtree walked once"
        // witness.
        std::fs::create_dir_all(d.join("schemas")).unwrap();
        std::fs::write(
            d.join("schemas/diamond_d_schema.yaml"),
            "metadata:\n  type: DiamondDSchema\n",
        )
        .unwrap();
        std::fs::write(
            d.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: diamond-d\n  version: \"1.0.0\"\n\
             schemas:\n  DiamondDSchema:\n    file: schemas/diamond_d_schema.yaml\n\
             dependencies:\n  \"@tatolab/diamond-e\":\n    path: ../e\n",
        )
        .unwrap();
        write_schemas_only_manifest(&e, "tatolab", "diamond-e", "1.0.0", None);

        let added =
            runtime.add_module_with(tatolab_ident("diamond-a"), path_strategy_never_build(&a));
        let handle = runtime.tokio_runtime_variant.handle();
        let result = handle
            .block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(30), added).await
            })
            .expect(
                "diamond load must complete without waiting on itself — a \
                 timeout here is the SkipOwnedByThisLoad regression",
            );
        result.expect("diamond load must succeed");

        assert_eq!(
            count_materializations_for_dir_named(&counts, "e"),
            1,
            "D's subtree must be walked exactly once"
        );
        let d_ref = streamlib_idents::PackageRef::new(
            Org::new("tatolab").unwrap(),
            Package::new("diamond-d").unwrap(),
        );
        let record = runtime
            .resolution_memo
            .committed_record(&d_ref)
            .expect("D must be committed");
        assert_eq!(
            record.required_by.len(),
            2,
            "both diamond branches must be recorded as requirers"
        );
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(
                "@tatolab/diamond-d/DiamondDSchema"
            )
            .is_some(),
            "D's schema must be registered exactly once via the commit"
        );
    }

    /// Semantic strengthening: a skipped dependency only counts as
    /// committed when its owner's WHOLE load commits. The owner (TA) walks
    /// shared dep D successfully, then fails at its OWN processor phase —
    /// the waiter (TB) must get ConcurrentLoadOfSkippedDependencyFailed,
    /// and D must be fully rolled back. Under the old per-subtree commit
    /// semantics D would have committed when its subtree unwound and TB
    /// would have succeeded over a registration that no longer exists.
    #[test]
    #[serial]
    fn concurrent_load_root_failure_after_shared_dep_walked_fails_waiter() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        let pkg_root = tempfile::tempdir().unwrap();
        let ta = pkg_root.path().join("ta");
        let tb = pkg_root.path().join("tb");
        let d = pkg_root.path().join("d");
        let e = pkg_root.path().join("e");
        for p in [&ta, &tb, &d, &e] {
            std::fs::create_dir_all(p).unwrap();
        }
        // TA: deps {D, E} (D sorts before E), plus a processor whose
        // config schema is unresolvable — TA's own processor phase fails
        // AFTER both dep subtrees walked.
        std::fs::write(
            ta.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-root-ta\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/conc-root-d\":\n    path: ../d\n\
             \x20 \"@tatolab/conc-root-e\":\n    path: ../e\n\
             processors:\n\
             \x20 - name: ConcRootFailingProcessor\n\
             \x20   runtime: typescript\n\
             \x20   entrypoint: main.ts\n\
             \x20   execution: manual\n\
             \x20   config:\n\
             \x20     name: config\n\
             \x20     schema: ConcRootNoSuchConfigSchema\n",
        )
        .unwrap();
        std::fs::write(
            tb.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: conc-root-tb\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/conc-root-d\":\n    path: ../d\n",
        )
        .unwrap();
        write_schemas_only_manifest(
            &d,
            "tatolab",
            "conc-root-d",
            "1.0.0",
            Some("ConcRootDSchema"),
        );
        write_schemas_only_manifest(&e, "tatolab", "conc-root-e", "1.0.0", None);

        let runtime = Runner::new().expect("Runner::new");
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        runtime.set_build_orchestrator(GatedSubDependencyOrchestrator {
            gated_dir_name: "e".to_string(),
            release: parking_lot::Mutex::new(Some(release_rx)),
            fail_after_release: false,
        });

        let d_ref = streamlib_idents::PackageRef::new(
            Org::new("tatolab").unwrap(),
            Package::new("conc-root-d").unwrap(),
        );
        let added_ta = runtime.add_module_with(
            tatolab_ident("conc-root-ta"),
            path_strategy_never_build(&ta),
        );
        assert!(
            poll_until(std::time::Duration::from_secs(10), || {
                runtime.resolution_memo.in_flight_requirer_count(&d_ref) == Some(1)
            }),
            "TA must hold D in flight (whole-load commit) while blocked on E",
        );
        let added_tb = runtime.add_module_with(
            tatolab_ident("conc-root-tb"),
            path_strategy_never_build(&tb),
        );
        assert!(
            poll_until(std::time::Duration::from_secs(10), || {
                runtime.resolution_memo.in_flight_requirer_count(&d_ref) == Some(2)
            }),
            "TB must record its requirer on D's in-flight placeholder",
        );
        release_tx.send(()).unwrap();

        let handle = runtime.tokio_runtime_variant.handle();
        let (result_ta, result_tb) = handle.block_on(async { tokio::join!(added_ta, added_tb) });
        let owner_err = result_ta
            .expect_err("TA must fail at its own processor phase after D's subtree walked");
        assert!(
            matches!(owner_err, AddModuleError::LoadProjectFailed { .. }),
            "TA must surface the config-schema resolution failure, got: {owner_err:?}",
        );
        let waiter_err = result_tb.expect_err(
            "TB must fail loudly when the owner's WHOLE load fails — even \
             though D's own subtree walked cleanly",
        );
        assert!(
            matches!(
                waiter_err,
                AddModuleError::ConcurrentLoadOfSkippedDependencyFailed { ref package, version }
                    if package.name.as_str() == "conc-root-d"
                        && version == SemVer::new(1, 0, 0)
            ),
            "expected ConcurrentLoadOfSkippedDependencyFailed for D, got: {waiter_err:?}",
        );
        assert!(
            !runtime.resolution_memo.contains_package(&d_ref),
            "the failed owner's guards must clear D's placeholder",
        );
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(
                "@tatolab/conc-root-d/ConcRootDSchema"
            )
            .is_none(),
            "D's schema must be rolled back with the owner's whole load",
        );
    }

    // =====================================================================
    // remove_module
    // =====================================================================

    #[test]
    #[serial]
    fn remove_module_version_range_mismatch_names_loaded_version() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("rm-range");
        std::fs::create_dir_all(&pkg).unwrap();
        write_schemas_only_manifest(&pkg, "tatolab", "rm-range", "1.0.0", None);
        runtime
            .add_module_with_blocking(tatolab_ident("rm-range"), path_strategy_never_build(&pkg))
            .expect("load must succeed");

        let err = runtime
            .remove_module(ModuleIdent::new(
                Org::new("tatolab").unwrap(),
                Package::new("rm-range").unwrap(),
                SemVerRange::Caret(SemVer::new(2, 0, 0)),
            ))
            .expect_err("a non-matching range must refuse");
        assert!(
            matches!(
                err,
                RemoveModuleError::ModuleNotLoaded { loaded_version: Some(v), .. }
                    if v == SemVer::new(1, 0, 0)
            ),
            "got: {err:?}",
        );

        // Cleanup for later #[serial] tests: matching range removes.
        runtime
            .remove_module(tatolab_ident("rm-range"))
            .expect("matching range must remove");
    }

    #[test]
    #[serial]
    fn remove_module_refuses_required_dependency_then_removes_in_order() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        runtime.set_build_orchestrator(LoadAsIsOrchestrator);
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("rm-root");
        let dep = tmp.path().join("rm-dep");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&dep).unwrap();
        std::fs::write(
            root.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: rm-root\n  version: \"1.0.0\"\n\
             dependencies:\n  \"@tatolab/rm-dep\":\n    path: ../rm-dep\n",
        )
        .unwrap();
        write_schemas_only_manifest(&dep, "tatolab", "rm-dep", "1.0.0", Some("RmDepSchema"));

        runtime
            .add_module_with_blocking(tatolab_ident("rm-root"), path_strategy_never_build(&root))
            .expect("root+dep load must succeed");

        // Dep removal refused while the root requires it — no cascade.
        let err = runtime
            .remove_module(tatolab_ident("rm-dep"))
            .expect_err("dep removal must refuse while the root requires it");
        assert!(
            matches!(
                err,
                RemoveModuleError::RequiredByLoadedModules { ref requirers, .. }
                    if requirers.iter().any(|r| r.name.as_str() == "rm-root")
            ),
            "got: {err:?}",
        );
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(
                "@tatolab/rm-dep/RmDepSchema"
            )
            .is_some(),
            "a refused removal must leave the dep's schema registered"
        );

        // Root first, then the dep — requirer pruning unblocks the dep.
        runtime
            .remove_module(tatolab_ident("rm-root"))
            .expect("root removal must succeed");
        runtime
            .remove_module(tatolab_ident("rm-dep"))
            .expect("dep removal must succeed after the root was removed");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(
                "@tatolab/rm-dep/RmDepSchema"
            )
            .is_none(),
            "the removed dep's schema must be unregistered"
        );

        // The full graph reloads cleanly after removal.
        runtime
            .add_module_with_blocking(tatolab_ident("rm-root"), path_strategy_never_build(&root))
            .expect("reload after removal must succeed");
        runtime.remove_module(tatolab_ident("rm-root")).unwrap();
        runtime.remove_module(tatolab_ident("rm-dep")).unwrap();
    }

    #[test]
    #[serial]
    fn remove_module_processors_in_use_then_removable_after_graph_removal() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("rm-inuse");
        std::fs::create_dir_all(pkg.join("schemas")).unwrap();
        std::fs::write(
            pkg.join("schemas/rm_inuse_config.yaml"),
            "metadata:\n  type: RmInUseConfigSchema\n",
        )
        .unwrap();
        std::fs::write(
            pkg.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: rm-inuse\n  version: \"1.0.0\"\n\
             schemas:\n  RmInUseConfigSchema:\n    file: schemas/rm_inuse_config.yaml\n\
             processors:\n\
             \x20 - name: RmInUseProcessor\n\
             \x20   runtime: typescript\n\
             \x20   entrypoint: main.ts\n\
             \x20   execution: manual\n\
             \x20   config:\n\
             \x20     name: config\n\
             \x20     schema: RmInUseConfigSchema\n",
        )
        .unwrap();

        runtime
            .add_module_with_blocking(tatolab_ident("rm-inuse"), path_strategy_never_build(&pkg))
            .expect("processor package load must succeed");

        let descriptor = crate::core::processors::PROCESSOR_REGISTRY
            .list_registered()
            .into_iter()
            .find(|d| d.name.r#type.as_str() == "RmInUseProcessor")
            .expect("RmInUseProcessor must be registered");
        let spec = crate::core::processors::ProcessorSpec::new(
            descriptor.name.clone(),
            serde_json::json!({}),
        );
        let processor_id = runtime
            .add_processor(spec.clone())
            .expect("add_processor against the loaded type must succeed");

        // In use: the graph node blocks removal, and the refusal restores
        // the registration (a follow-up add_processor still works).
        let err = runtime
            .remove_module(tatolab_ident("rm-inuse"))
            .expect_err("removal must refuse while a graph node uses the type");
        match &err {
            RemoveModuleError::ProcessorsInUse {
                processor_ids,
                processor_types,
                ..
            } => {
                assert!(processor_ids.contains(&processor_id));
                assert!(
                    processor_types
                        .iter()
                        .any(|t| t.r#type.as_str() == "RmInUseProcessor")
                );
            }
            other => panic!("expected ProcessorsInUse, got {other:?}"),
        }
        assert!(
            crate::core::processors::PROCESSOR_REGISTRY.is_registered(&descriptor.name),
            "a refused removal must restore the processor registration"
        );

        // Remove the graph node; module removal now succeeds.
        runtime
            .remove_processor(&processor_id)
            .expect("remove_processor must succeed");
        runtime
            .remove_module(tatolab_ident("rm-inuse"))
            .expect("module removal must succeed after the graph node is gone");

        // Post-removal add_processor gets the typed registry miss.
        let err = runtime
            .add_processor(spec)
            .expect_err("add_processor after removal must miss the registry");
        assert!(
            matches!(err, crate::core::Error::UnknownProcessorType { .. }),
            "expected UnknownProcessorType, got: {err:?}",
        );
    }

    /// Load/unload/reload ×2 of the same package on one Runner: the
    /// registry snapshot after every reload equals the snapshot after the
    /// first load.
    #[test]
    #[serial]
    fn load_unload_reload_cycle_is_registry_idempotent() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().join("cycle-pkg");
        std::fs::create_dir_all(&pkg).unwrap();
        write_schemas_only_manifest(
            &pkg,
            "tatolab",
            "cycle-pkg",
            "1.0.0",
            Some("CyclePkgSchema"),
        );

        runtime
            .add_module_with_blocking(tatolab_ident("cycle-pkg"), path_strategy_never_build(&pkg))
            .expect("first load must succeed");
        let after_first_load = RegistrySnapshot::capture(&runtime.resolution_memo);

        for cycle in 0..2 {
            runtime
                .remove_module(tatolab_ident("cycle-pkg"))
                .unwrap_or_else(|e| panic!("cycle {cycle}: remove must succeed: {e:?}"));
            assert!(
                crate::core::embedded_schemas::get_embedded_schema_definition(
                    "@tatolab/cycle-pkg/CyclePkgSchema"
                )
                .is_none(),
                "cycle {cycle}: schema must be gone after removal"
            );
            runtime
                .add_module_with_blocking(
                    tatolab_ident("cycle-pkg"),
                    path_strategy_never_build(&pkg),
                )
                .unwrap_or_else(|e| panic!("cycle {cycle}: reload must succeed: {e:?}"));
            let after_reload = RegistrySnapshot::capture(&runtime.resolution_memo);
            assert_eq!(
                after_first_load, after_reload,
                "cycle {cycle}: reload must reproduce the first load's registry state"
            );
        }

        // Leave the process-global registries clean for later tests.
        runtime.remove_module(tatolab_ident("cycle-pkg")).unwrap();
    }

    // =====================================================================
    // install / run split (#1221)
    //
    // Exercises the full resolver handoff: `install` resolves range→concrete,
    // materializes every package into the app's streamlib_modules/ slots, and
    // writes the application lockfile; a locked run consumes that lockfile
    // strictly from those slots with NO live re-resolution — so it works offline
    // even against a poisoned / unreachable registry.
    // =====================================================================

    use std::io::Write as _;

    /// Orchestrator that stages a `PackageDir` into the co-located slot the
    /// install seam injects (`<app-root>/streamlib_modules/@org/name/`) — where
    /// a locked run looks — by copying the resolved source tree. No toolchain:
    /// enough to prove the resolve→materialize→lock→locked-run handoff with
    /// schemas-only packages. The real `PolyglotBuildOrchestrator` stages
    /// into the identical slot (plus builds cdylibs / venvs / native hosts).
    struct StageIntoCacheOrchestrator;
    impl BuildOrchestrator for StageIntoCacheOrchestrator {
        fn materialize(
            &self,
            request: &BuildRequest,
            _sink: &dyn BuildEventSink,
        ) -> std::result::Result<StagedArtifact, BuildError> {
            let src = match &request.source {
                BuildSource::PackageDir(d) => d.clone(),
                other => return Err(BuildError::UnsupportedSource(format!("{other:?}"))),
            };
            // Honor the co-located slot the install seam injected (#1517) rather
            // than self-deriving — write==read only holds when the orchestrator
            // stages into the exact dir the locked read later resolves.
            let slot = request.staging_destination_slot_dir.clone();
            let _ = std::fs::remove_dir_all(&slot);
            copy_dir_recursive(&src, &slot).map_err(|e| BuildError::Other {
                package: request.package.to_string(),
                detail: format!("staging into cache: {e}"),
            })?;
            Ok(StagedArtifact {
                staged_dir: slot,
                rebuilt: false,
            })
        }
    }

    fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dst)?;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let from = entry.path();
            let to = dst.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_dir_recursive(&from, &to)?;
            } else {
                std::fs::copy(&from, &to)?;
            }
        }
        Ok(())
    }

    struct NoopBuildSink;
    impl BuildEventSink for NoopBuildSink {
        fn emit(&self, _event: BuildEvent) {}
    }

    /// Write a schemas-only `.slpkg` into a `file://` registry mirror at
    /// `<mirror>/slpkg/<name>/<version>/<name>.slpkg` (the base URL is the
    /// tree root; the registry client prepends the `slpkg/` subtree). `dep`
    /// optionally adds a registry-flavored dependency edge
    /// (`@tatolab/<dep>: "^<range>"`).
    fn write_mirror_slpkg(
        mirror: &std::path::Path,
        name: &str,
        version: &str,
        type_name: &str,
        dep: Option<(&str, &str)>,
    ) {
        let dir = mirror.join("slpkg").join(name).join(version);
        std::fs::create_dir_all(&dir).unwrap();
        let archive = dir.join(format!("{name}.slpkg"));
        let stem = type_name.to_ascii_lowercase();
        let mut manifest = format!(
            "package:\n  org: tatolab\n  name: {name}\n  version: \"{version}\"\n\
             schemas:\n  {type_name}:\n    file: schemas/{stem}.yaml\n"
        );
        if let Some((dep_name, dep_range)) = dep {
            manifest.push_str(&format!(
                "dependencies:\n  \"@tatolab/{dep_name}\": \"^{dep_range}\"\n"
            ));
        }
        let schema = format!("metadata:\n  type: {type_name}\n  expected_payload_bytes: 4096\n");
        let mut zw = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let opts: zip::write::FileOptions<()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zw.start_file("streamlib.yaml", opts).unwrap();
        zw.write_all(manifest.as_bytes()).unwrap();
        zw.start_file(format!("schemas/{stem}.yaml"), opts).unwrap();
        zw.write_all(schema.as_bytes()).unwrap();
        zw.finish().unwrap();
    }

    /// THE key test: install from a `file://` registry, then run **strictly
    /// from the lockfile** against a POISONED (unreachable) registry. The
    /// locked run must load the full graph offline — including a
    /// registry-flavored transitive dep edge, which the locked walker forces
    /// to the pinned cache slot instead of a live registry fetch.
    ///
    /// Mentally-revert: drop the `locked` forcing in the recursive walker and
    /// `app-lib`'s `@tatolab/lockrun-core` registry edge would resolve live,
    /// hit the poisoned registry, and fail — the run would NOT be offline.
    #[test]
    #[serial]
    fn install_then_locked_run_is_offline_against_poisoned_registry() {
        let sandbox = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("STREAMLIB_HOME");
        unsafe {
            std::env::set_var("STREAMLIB_HOME", sandbox.path());
        }
        let _restore = StreamlibHomeRestore(prev_home);
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL"]);

        // Two-level registry tree: lockrun-lib depends on lockrun-core.
        let mirror = tempfile::tempdir().unwrap();
        write_mirror_slpkg(
            mirror.path(),
            "lockrun-core",
            "0.1.0",
            "LockrunCoreSchema",
            None,
        );
        write_mirror_slpkg(
            mirror.path(),
            "lockrun-lib",
            "0.1.0",
            "LockrunLibSchema",
            Some(("lockrun-core", "0.1.0")),
        );

        // Root project declares a registry dep on lockrun-lib.
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("streamlib.yaml"),
            "dependencies:\n  \"@tatolab/lockrun-lib\": \"^0.1.0\"\n",
        )
        .unwrap();

        // ---- install (registry reachable) ----
        unsafe {
            std::env::set_var(
                "STREAMLIB_REGISTRY_URL",
                format!("file://{}", mirror.path().display()),
            );
        }
        let report = crate::core::runtime::install(
            project.path(),
            &StageIntoCacheOrchestrator,
            &NoopBuildSink,
            &crate::core::runtime::InstallOptions::default(),
        )
        .expect("install must resolve + materialize + lock");

        // Lockfile pins the full transitive set (lib + its transitive core).
        assert_eq!(report.packages.len(), 2, "pins: {:?}", report.packages);
        let names: Vec<String> = report.packages.iter().map(|(p, _)| p.to_string()).collect();
        assert!(
            names.contains(&"@tatolab/lockrun-lib".to_string()),
            "{names:?}"
        );
        assert!(
            names.contains(&"@tatolab/lockrun-core".to_string()),
            "{names:?}"
        );
        assert!(report.lockfile_path.ends_with("streamlib-app.lock"));

        // ---- poison the registry ----
        // Any live registry touch now fails (connection refused).
        unsafe {
            std::env::set_var("STREAMLIB_REGISTRY_URL", "http://127.0.0.1:1");
        }

        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(
                "@tatolab/lockrun-lib/LockrunLibSchema"
            )
            .is_none(),
            "schema must not be registered before the locked run"
        );

        // ---- locked run (offline) ----
        let runtime = Runner::new().unwrap();
        runtime
            .add_modules_from_lockfile_blocking(&report.lockfile_path)
            .expect("locked run must load the full graph offline against a poisoned registry");

        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(
                "@tatolab/lockrun-lib/LockrunLibSchema"
            )
            .is_some(),
            "lib schema must register from the locked cache"
        );
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(
                "@tatolab/lockrun-core/LockrunCoreSchema"
            )
            .is_some(),
            "transitive core schema must register from the locked cache. (The \
             offline/transitive-forcing guarantee itself is proven by the \
             .expect() above not failing against the poisoned registry — this \
             assert only confirms the dep's registration side effect.)"
        );
    }

    /// `install` is byte-deterministic: identical inputs → identical lockfile
    /// bytes. Uses a path dep (hermetic, no registry) so the run is fast and
    /// the only variable is the resolver output.
    #[test]
    #[serial]
    fn install_writes_byte_identical_lockfile() {
        let sandbox = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("STREAMLIB_HOME");
        unsafe {
            std::env::set_var("STREAMLIB_HOME", sandbox.path());
        }
        let _restore = StreamlibHomeRestore(prev_home);
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL"]);

        // A local package + a project that path-deps it.
        let work = tempfile::tempdir().unwrap();
        let dep = work.path().join("det-dep");
        std::fs::create_dir_all(dep.join("schemas")).unwrap();
        std::fs::write(
            dep.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: det-dep\n  version: \"1.0.0\"\n\
             schemas:\n  DetDepSchema:\n    file: schemas/detdepschema.yaml\n",
        )
        .unwrap();
        std::fs::write(
            dep.join("schemas/detdepschema.yaml"),
            "metadata:\n  type: DetDepSchema\n  expected_payload_bytes: 1024\n",
        )
        .unwrap();
        let project = work.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("streamlib.yaml"),
            "dependencies:\n  \"@tatolab/det-dep\":\n    path: ../det-dep\n",
        )
        .unwrap();

        let install_to = |lock: std::path::PathBuf| {
            crate::core::runtime::install(
                &project,
                &StageIntoCacheOrchestrator,
                &NoopBuildSink,
                &crate::core::runtime::InstallOptions {
                    lockfile_path: Some(lock),
                    ..Default::default()
                },
            )
            .expect("install")
        };

        let lock_a = work.path().join("a.lock");
        let lock_b = work.path().join("b.lock");
        install_to(lock_a.clone());
        install_to(lock_b.clone());
        let a = std::fs::read(&lock_a).unwrap();
        let b = std::fs::read(&lock_b).unwrap();
        assert_eq!(
            a, b,
            "two installs of identical inputs must produce identical lockfile bytes"
        );

        // Path deps are recorded as `path:` sources (the link-bridge shape:
        // a linked/local tree records Path entries).
        let text = String::from_utf8(a).unwrap();
        assert!(
            text.contains("kind: path"),
            "path dep must record a path source:\n{text}"
        );
    }

    /// A locked run whose manifest declares a dep the lockfile does NOT pin
    /// fails loud with the typed [`AddModuleError::LockfileMiss`] naming
    /// `streamlib install` — never a silent live resolve.
    #[test]
    #[serial]
    fn locked_run_lockfile_miss_is_typed_error() {
        let sandbox = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("STREAMLIB_HOME");
        unsafe {
            std::env::set_var("STREAMLIB_HOME", sandbox.path());
        }
        let _restore = StreamlibHomeRestore(prev_home);
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL"]);

        // Stage and read against one app root: the lockfile lives under
        // `sandbox`, so the co-located slot must too.
        let _modules_root = AppModulesRootOverrideGuard::install(sandbox.path());

        // Hand-stage a package whose manifest declares a dep on `miss-dep`,
        // and a lockfile that pins ONLY the package (stale relative to the
        // manifest graph).
        let slot = installed_package_slot_for_test("tatolab", "miss-pkg");
        std::fs::create_dir_all(slot.join("schemas")).unwrap();
        std::fs::write(
            slot.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: miss-pkg\n  version: \"0.1.0\"\n\
             schemas:\n  MissPkgSchema:\n    file: schemas/misspkgschema.yaml\n\
             dependencies:\n  \"@tatolab/miss-dep\": \"^0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            slot.join("schemas/misspkgschema.yaml"),
            "metadata:\n  type: MissPkgSchema\n  expected_payload_bytes: 1024\n",
        )
        .unwrap();

        // Pin the REAL slot hash so the content-integrity gate passes and
        // the walk reaches the dep edge where LockfileMiss fires.
        let slot_hash = streamlib_idents::content_hash_for_package_dir(&slot).unwrap();
        let lock = sandbox.path().join("stale.lock");
        std::fs::write(
            &lock,
            format!(
                r#"version: 1
packages:
  "@tatolab/miss-pkg":
    version: 0.1.0
    source:
      kind: registry
      url: file:///x
    content_hash: "{slot_hash}"
"#
            ),
        )
        .unwrap();

        let runtime = Runner::new().unwrap();
        let err = runtime
            .add_modules_from_lockfile_blocking(&lock)
            .expect_err("a dep missing from the lockfile must fail loud");
        assert!(
            matches!(err, AddModuleError::LockfileMiss { ref package, .. }
                if package.name.as_str() == "miss-dep"),
            "expected LockfileMiss naming miss-dep, got: {err:?}"
        );
    }

    /// A lockfile that pins a package whose cache slot was never materialized
    /// fails loud with [`AddModuleError::LockedSlotMissing`] naming
    /// `streamlib install`.
    #[test]
    #[serial]
    fn locked_run_uninstalled_slot_is_typed_error() {
        let sandbox = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("STREAMLIB_HOME");
        unsafe {
            std::env::set_var("STREAMLIB_HOME", sandbox.path());
        }
        let _restore = StreamlibHomeRestore(prev_home);
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL"]);

        // Lockfile pins a package but nothing was ever staged into its slot.
        let lock = sandbox.path().join("uninstalled.lock");
        std::fs::write(
            &lock,
            r#"version: 1
packages:
  "@tatolab/uninstalled-xyz":
    version: 9.9.9
    source:
      kind: registry
      url: file:///x
    content_hash: "sha256:0"
"#,
        )
        .unwrap();

        let runtime = Runner::new().unwrap();
        let err = runtime
            .add_modules_from_lockfile_blocking(&lock)
            .expect_err("a pinned-but-uninstalled package must fail loud");
        assert!(
            matches!(err, AddModuleError::LockedSlotMissing { ref package, .. }
                if package.name.as_str() == "uninstalled-xyz"),
            "expected LockedSlotMissing, got: {err:?}"
        );
    }

    /// Hand-stage a schemas-only package into the co-located streamlib_modules
    /// slot for `name`@`version` and return `(slot_dir, content_hash)`.
    fn stage_schemas_only_slot(
        name: &str,
        version: &str,
        type_name: &str,
    ) -> (std::path::PathBuf, String) {
        let pkg_ref = streamlib_idents::PackageRef::new(
            streamlib_idents::Org::new("tatolab").unwrap(),
            streamlib_idents::Package::new(name).unwrap(),
        );
        let slot = crate::core::installed_package_slot_dir(None, &pkg_ref);
        let stem = type_name.to_ascii_lowercase();
        std::fs::create_dir_all(slot.join("schemas")).unwrap();
        std::fs::write(
            slot.join("streamlib.yaml"),
            format!(
                "package:\n  org: tatolab\n  name: {name}\n  version: \"{version}\"\n\
                 schemas:\n  {type_name}:\n    file: schemas/{stem}.yaml\n"
            ),
        )
        .unwrap();
        std::fs::write(
            slot.join(format!("schemas/{stem}.yaml")),
            format!("metadata:\n  type: {type_name}\n  expected_payload_bytes: 1024\n"),
        )
        .unwrap();
        let hash = streamlib_idents::content_hash_for_package_dir(&slot).unwrap();
        (slot, hash)
    }

    /// Write a minimal single-package app lockfile pinning `name`@`version`
    /// with `content_hash`, returning its path.
    fn write_single_pin_lockfile(
        dir: &std::path::Path,
        name: &str,
        version: &str,
        content_hash: &str,
    ) -> std::path::PathBuf {
        let lock = dir.join(format!("{name}.lock"));
        std::fs::write(
            &lock,
            format!(
                r#"version: 1
packages:
  "@tatolab/{name}":
    version: {version}
    source:
      kind: registry
      url: file:///x
    content_hash: "{content_hash}"
"#
            ),
        )
        .unwrap();
        lock
    }

    /// The content-hash run-time integrity gate: a slot whose manifest /
    /// schema bytes were modified AFTER install no longer hashes to the
    /// lockfile pin, and the locked run fails typed with
    /// [`AddModuleError::LockedSlotContentMismatch`] naming the remedy.
    /// Mentally-revert: drop the hash comparison in
    /// `LockedResolution::resolve` and the tampered slot loads silently.
    #[test]
    #[serial]
    fn locked_run_tampered_slot_is_content_mismatch() {
        let sandbox = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("STREAMLIB_HOME");
        unsafe {
            std::env::set_var("STREAMLIB_HOME", sandbox.path());
        }
        let _restore = StreamlibHomeRestore(prev_home);
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL"]);

        // Stage and read against one app root: the lockfile lives under
        // `sandbox`, so the co-located slot must too.
        let _modules_root = AppModulesRootOverrideGuard::install(sandbox.path());

        let (slot, hash) = stage_schemas_only_slot("tamper-pkg", "0.1.0", "TamperPkgSchema");
        let lock = write_single_pin_lockfile(sandbox.path(), "tamper-pkg", "0.1.0", &hash);

        // Untampered: the pinned hash matches and the locked run succeeds.
        {
            let runtime = Runner::new().unwrap();
            runtime
                .add_modules_from_lockfile_blocking(&lock)
                .expect("untampered slot must pass the content-hash gate");
        }

        // Tamper the slot's schema post-install → hash busts → typed error.
        std::fs::write(
            slot.join("schemas/tamperpkgschema.yaml"),
            "metadata:\n  type: TamperPkgSchema\n  expected_payload_bytes: 9999\n",
        )
        .unwrap();
        let runtime = Runner::new().unwrap();
        let err = runtime
            .add_modules_from_lockfile_blocking(&lock)
            .expect_err("tampered slot must fail the content-hash gate");
        assert!(
            matches!(err, AddModuleError::LockedSlotContentMismatch { ref package, .. }
                if package.name.as_str() == "tamper-pkg"),
            "expected LockedSlotContentMismatch, got: {err:?}"
        );
    }

    /// Version drift between the lockfile pin and the slot's on-disk
    /// manifest fails typed: the locked ident carries an Exact pin, so the
    /// walker's version check rejects a slot whose manifest version moved.
    #[test]
    #[serial]
    fn locked_run_version_drift_is_range_unsatisfied() {
        let sandbox = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("STREAMLIB_HOME");
        unsafe {
            std::env::set_var("STREAMLIB_HOME", sandbox.path());
        }
        let _restore = StreamlibHomeRestore(prev_home);
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL"]);

        // Stage and read against one app root: the lockfile lives under
        // `sandbox`, so the co-located slot must too.
        let _modules_root = AppModulesRootOverrideGuard::install(sandbox.path());

        // The slot lives at the co-located `@org/name` dir, but its manifest
        // inside claims 1.0.1 while the lock pins 1.0.0 — an in-place republish
        // that kept the dir. Pin the drifted slot's REAL hash so the content
        // gate passes and the walker's version check is the one that fires.
        let pkg_ref = streamlib_idents::PackageRef::new(
            streamlib_idents::Org::new("tatolab").unwrap(),
            streamlib_idents::Package::new("drift-pkg").unwrap(),
        );
        let slot = crate::core::installed_package_slot_dir(Some(sandbox.path()), &pkg_ref);
        std::fs::create_dir_all(slot.join("schemas")).unwrap();
        std::fs::write(
            slot.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: drift-pkg\n  version: \"1.0.1\"\n\
             schemas:\n  DriftPkgSchema:\n    file: schemas/driftpkgschema.yaml\n",
        )
        .unwrap();
        std::fs::write(
            slot.join("schemas/driftpkgschema.yaml"),
            "metadata:\n  type: DriftPkgSchema\n  expected_payload_bytes: 1024\n",
        )
        .unwrap();
        let hash = streamlib_idents::content_hash_for_package_dir(&slot).unwrap();
        let lock = write_single_pin_lockfile(sandbox.path(), "drift-pkg", "1.0.0", &hash);

        let runtime = Runner::new().unwrap();
        let err = runtime
            .add_modules_from_lockfile_blocking(&lock)
            .expect_err("a slot whose on-disk version drifted from the pin must fail");
        assert!(
            matches!(err, AddModuleError::VersionRangeUnsatisfied { ref found, .. }
                if found.to_string() == "1.0.1"),
            "expected VersionRangeUnsatisfied at 1.0.1, got: {err:?}"
        );
    }

    /// A corrupted lockfile file surfaces the typed
    /// [`AddModuleError::LockfileReadFailed`] through the REAL
    /// `add_modules_from_lockfile` entry (not just the parser unit).
    #[test]
    #[serial]
    fn locked_run_corrupted_lockfile_is_read_failed() {
        let sandbox = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("STREAMLIB_HOME");
        unsafe {
            std::env::set_var("STREAMLIB_HOME", sandbox.path());
        }
        let _restore = StreamlibHomeRestore(prev_home);
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL"]);

        let lock = sandbox.path().join("corrupt.lock");
        std::fs::write(&lock, "{ this is: [not, a lockfile").unwrap();

        let runtime = Runner::new().unwrap();
        let err = match runtime.add_modules_from_lockfile(&lock) {
            Err(e) => e,
            Ok(_) => panic!("a corrupted lockfile must fail typed at the run entry"),
        };
        assert!(
            matches!(err, AddModuleError::LockfileReadFailed { .. }),
            "expected LockfileReadFailed, got: {err:?}"
        );
    }

    /// Registry mutated after install, registry REACHABLE at run time: the
    /// locked run must still load the pinned version — proving the walker's
    /// pin-forcing is version-selection-ignoring independent of
    /// connectivity (the poisoned-registry test proves the offline half).
    #[test]
    #[serial]
    fn locked_run_ignores_newer_version_on_reachable_registry() {
        let sandbox = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("STREAMLIB_HOME");
        unsafe {
            std::env::set_var("STREAMLIB_HOME", sandbox.path());
        }
        let _restore = StreamlibHomeRestore(prev_home);
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL"]);

        let mirror = tempfile::tempdir().unwrap();
        write_mirror_slpkg(
            mirror.path(),
            "mutate-pkg",
            "0.1.0",
            "MutatePkgSchema",
            None,
        );

        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("streamlib.yaml"),
            "dependencies:\n  \"@tatolab/mutate-pkg\": \"^0.1.0\"\n",
        )
        .unwrap();

        // Install with only 0.1.0 published.
        unsafe {
            std::env::set_var(
                "STREAMLIB_REGISTRY_URL",
                format!("file://{}", mirror.path().display()),
            );
        }
        let report = crate::core::runtime::install(
            project.path(),
            &StageIntoCacheOrchestrator,
            &NoopBuildSink,
            &crate::core::runtime::InstallOptions::default(),
        )
        .expect("install must pin 0.1.0");

        // Mutate the registry AFTER install: publish a newer in-range
        // version. The registry stays reachable for the run.
        write_mirror_slpkg(
            mirror.path(),
            "mutate-pkg",
            "0.2.0",
            "MutatePkgSchema",
            None,
        );

        let runtime = Runner::new().unwrap();
        runtime
            .add_modules_from_lockfile_blocking(&report.lockfile_path)
            .expect("locked run must succeed against the mutated (reachable) registry");

        // The committed resolution is the PINNED 0.1.0, not the newer 0.2.0
        // a live range resolve would have selected.
        let pkg = streamlib_idents::PackageRef::new(
            Org::new("tatolab").unwrap(),
            Package::new("mutate-pkg").unwrap(),
        );
        let record = runtime
            .resolution_memo
            .committed_record(&pkg)
            .expect("mutate-pkg must be committed");
        assert_eq!(
            record.version.to_string(),
            "0.1.0",
            "locked run must load the pinned version, ignoring the newer registry version"
        );
    }
}
