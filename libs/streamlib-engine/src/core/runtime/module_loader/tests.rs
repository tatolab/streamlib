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
    counts: std::sync::Arc<
        parking_lot::Mutex<std::collections::HashMap<std::path::PathBuf, usize>>,
    >,
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
    counts: std::sync::Arc<
        parking_lot::Mutex<std::collections::HashMap<std::path::PathBuf, usize>>,
    >,
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
                    .is_some_and(|name| {
                        self.rendezvous_dir_names.iter().any(|r| r == name)
                    });
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
        "metadata:\n  type: MyTestConfig\n  max_payload_bytes: 8192\n",
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
    let port_spec = streamlib_processor_schema::PortSchemaSpec::Specific(
        streamlib_idents::SchemaIdent::new(
            streamlib_idents::Org::new("tatolab").unwrap(),
            streamlib_idents::Package::new("test-load-project-registers-schemas").unwrap(),
            streamlib_idents::TypeName::new("MyTestConfig").unwrap(),
            streamlib_idents::SemVer::new(1, 0, 0),
        ),
    );
    assert_eq!(
        crate::core::embedded_schemas::max_payload_bytes_for_port_spec(&port_spec).unwrap(),
        8192,
        "max_payload_bytes_for_port_spec must read metadata declared by the loaded package"
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
    let rev = String::from_utf8(rev_output.stdout).unwrap().trim().to_string();

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
    let schema = format!("metadata:\n  type: {type_name}\n  max_payload_bytes: 4096\n");

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
    // No ambient registry config, so the routing is observable regardless of
    // the developer / CI shell.
    let _no_registry = EnvVarsCleared::new(&[
        "STREAMLIB_REGISTRY_URL",
        "STREAMLIB_REGISTRY_URL",
        "STREAMLIB_REGISTRY_TOKEN",
    ]);

    // An installed-cache entry for @tatolab/b that WOULD satisfy `^0.1.0` —
    // resolved through the production accessor so it lands at the real layout.
    let dep_cache_dir = crate::core::get_cached_package_dir("b-0.1.0");
    std::fs::create_dir_all(&dep_cache_dir).unwrap();
    std::fs::write(
        dep_cache_dir.join("streamlib.yaml"),
        "package:\n  org: tatolab\n  name: b\n  version: \"0.1.0\"\n",
    )
    .unwrap();
    let mut installed = crate::core::config::InstalledPackageManifest::default();
    installed.add(crate::core::config::InstalledPackageEntry {
        name: streamlib_idents::PackageRef::new(
            streamlib_processor_schema::Org::new("tatolab").unwrap(),
            streamlib_processor_schema::Package::new("b").unwrap(),
        ),
        version: streamlib_processor_schema::SemVer::new(0, 1, 0),
        description: None,
        installed_from: "test".into(),
        installed_at: "1970-01-01T00:00:00Z".into(),
        cache_dir: "b-0.1.0".to_string(),
    });
    installed.save().unwrap();

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
    version: 1.0.0
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
    version: 1.0.0
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

// =========================================================================
// add_module / add_module_with — imperative module API + BuildPolicy
// =========================================================================

mod add_module_tests {
    use std::sync::Arc;

    use super::*;
    use streamlib_idents::{ModuleIdent, Org, Package, SemVer, SemVerRange};

    /// RAII guard that restores `STREAMLIB_HOME` to its pre-test value on
    /// drop. Every cache-backed test isolates the installed-package cache
    /// to a tempdir to keep the host's real cache out of the test surface.
    struct HomeGuard {
        home_prev: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn install(home_root: &std::path::Path) -> Self {
            let home_prev = std::env::var_os("STREAMLIB_HOME");
            unsafe {
                std::env::set_var("STREAMLIB_HOME", home_root);
            }
            Self { home_prev }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
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
                format!("metadata:\n  type: {type_name}\n  max_payload_bytes: 4096\n"),
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

    /// Install a schemas-only package into the sandboxed installed-package
    /// cache so bare `add_module` (which resolves cache-only) can find it.
    fn install_cached_package(org: &str, name: &str, version: &str, schema: Option<&str>) {
        let cache_dir_name = format!("{name}-{version}");
        // Resolve the cache dir through the production accessor so the fixture
        // can't drift from the real layout — `get_cached_package_dir` →
        // `<STREAMLIB_HOME>/.streamlib/cache/packages/<name>-<version>`, and the
        // caller's HomeGuard points STREAMLIB_HOME at the test tempdir. A
        // hardcoded `.streamlib/cache/packages` literal here is exactly what
        // drifted when the cache moved under `.streamlib`.
        let dep_cache_dir = crate::core::get_cached_package_dir(&cache_dir_name);
        std::fs::create_dir_all(&dep_cache_dir).unwrap();
        write_schemas_only_manifest(&dep_cache_dir, org, name, version, schema);

        let (maj, min, pat) = {
            let mut it = version.split('.').map(|p| p.parse::<u32>().unwrap());
            (it.next().unwrap(), it.next().unwrap(), it.next().unwrap())
        };
        let mut installed = crate::core::config::InstalledPackageManifest::default();
        installed.add(crate::core::config::InstalledPackageEntry {
            name: streamlib_idents::PackageRef::new(
                Org::new(org).unwrap(),
                Package::new(name).unwrap(),
            ),
            version: SemVer::new(maj, min, pat),
            description: None,
            installed_from: "test".into(),
            installed_at: "1970-01-01T00:00:00Z".into(),
            cache_dir: cache_dir_name,
        });
        installed.save().unwrap();
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
    fn add_module_rejects_identity_mismatch_on_cached_yaml() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        // Cache entry keyed @tatolab/add-module-identity but the on-disk
        // manifest declares a different identity (clobbered cache). Resolve
        // through the production accessor so the fixture tracks the real layout.
        let dep_cache_dir = crate::core::get_cached_package_dir("add-module-identity-1.0.0");
        std::fs::create_dir_all(&dep_cache_dir).unwrap();
        write_schemas_only_manifest(&dep_cache_dir, "vendor", "other", "1.0.0", None);
        let mut installed = crate::core::config::InstalledPackageManifest::default();
        installed.add(crate::core::config::InstalledPackageEntry {
            name: streamlib_idents::PackageRef::new(
                Org::new("tatolab").unwrap(),
                Package::new("add-module-identity").unwrap(),
            ),
            version: SemVer::new(1, 0, 0),
            description: None,
            installed_from: "test".into(),
            installed_at: "1970-01-01T00:00:00Z".into(),
            cache_dir: "add-module-identity-1.0.0".to_string(),
        });
        installed.save().unwrap();

        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module_blocking(ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("add-module-identity").unwrap(),
            ))
            .expect_err("clobbered cached manifest must error");

        assert!(
            matches!(err, AddModuleError::ManifestIdentityMismatch { ref actual, .. } if actual == "@vendor/other"),
            "expected ManifestIdentityMismatch(@vendor/other), got: {err:?}",
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
                ModuleIdent::any(Org::new("tatolab").unwrap(), Package::new("ic-only").unwrap()),
                Strategy::InstalledCache,
            )
            .expect("InstalledCache strategy must hit the cache");
        assert!(crate::core::embedded_schemas::get_embedded_schema_definition(&canonical).is_some());
    }

    #[test]
    #[serial]
    fn path_strategy_loads_arbitrary_dir() {
        const TYPE_NAME: &str = "PathStrategySchema";
        let home = tempfile::tempdir().unwrap();
        let arbitrary = tempfile::tempdir().unwrap();
        write_schemas_only_manifest(arbitrary.path(), "tatolab", "md-arbitrary", "0.7.2", Some(TYPE_NAME));
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let canonical = format!("@tatolab/md-arbitrary/{TYPE_NAME}");
        runtime
            .add_module_with_blocking(
                ModuleIdent::any(Org::new("tatolab").unwrap(), Package::new("md-arbitrary").unwrap()),
                Strategy::Path {
                    path: arbitrary.path().to_path_buf(),
                    build: BuildPolicy::NeverBuild,
                },
            )
            .expect("Path strategy must load the arbitrary dir");
        assert!(crate::core::embedded_schemas::get_embedded_schema_definition(&canonical).is_some());
    }

    #[test]
    #[serial]
    fn path_strategy_surfaces_missing_dir() {
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module_with_blocking(
                ModuleIdent::any(Org::new("tatolab").unwrap(), Package::new("md-missing").unwrap()),
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
                ModuleIdent::any(Org::new("tatolab").unwrap(), Package::new("ab-no-orch").unwrap()),
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
        write_schemas_only_manifest(arbitrary.path(), "tatolab", "ifstale-no-orch", "0.1.0", None);
        let _guard = HomeGuard::install(home.path());
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module_with_blocking(
                ModuleIdent::any(Org::new("tatolab").unwrap(), Package::new("ifstale-no-orch").unwrap()),
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
    version: 1.0.0
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
                ModuleIdent::any(Org::new("tatolab").unwrap(), Package::new("py-no-orch").unwrap()),
                Strategy::Path {
                    path: pkg.path().to_path_buf(),
                    build: BuildPolicy::IfStale,
                },
            )
            .expect_err("a build-requiring python source package with no orchestrator must fail loud");
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
            format!("metadata:\n  type: {TYPE_A}\n  max_payload_bytes: 4096\n"),
        )
        .unwrap();
        std::fs::write(
            b.join("schemas/depwalkallthreebschema.yaml"),
            format!("metadata:\n  type: {TYPE_B}\n  max_payload_bytes: 4096\n"),
        )
        .unwrap();
        std::fs::write(
            c.join("schemas/depwalkallthreecschema.yaml"),
            format!("metadata:\n  type: {TYPE_C}\n  max_payload_bytes: 4096\n"),
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
                ModuleIdent::any(Org::new("tatolab").unwrap(), Package::new("cycle-self").unwrap()),
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
            !runtime.resolution_memo.contains_package(&streamlib_idents::PackageRef::new(
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
                ModuleIdent::any(Org::new("tatolab").unwrap(), Package::new("cycle-a").unwrap()),
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
                !runtime.resolution_memo.contains_package(&streamlib_idents::PackageRef::new(
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
            format!("metadata:\n  type: {TYPE_D}\n  max_payload_bytes: 4096\n"),
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
        let materialize_counts = Arc::new(parking_lot::Mutex::new(
            std::collections::HashMap::<std::path::PathBuf, usize>::new(),
        ));
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
            format!("metadata:\n  type: {TYPE_POISON}\n  max_payload_bytes: 4096\n"),
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
            format!("metadata:\n  type: {TYPE_D}\n  max_payload_bytes: 4096\n"),
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
        let materialize_counts = Arc::new(parking_lot::Mutex::new(
            std::collections::HashMap::<std::path::PathBuf, usize>::new(),
        ));
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
        let (result_ta, result_tb) =
            handle.block_on(async { tokio::join!(added_ta, added_tb) });
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
        let materialize_counts = Arc::new(parking_lot::Mutex::new(
            std::collections::HashMap::<std::path::PathBuf, usize>::new(),
        ));
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
        let (result_ta, result_tb) =
            handle.block_on(async { tokio::join!(added_ta, added_tb) });

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
        let (result_ta, result_tb) =
            handle.block_on(async { tokio::join!(added_ta, added_tb) });
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
        let (result_ta, result_tb) =
            handle.block_on(async { tokio::join!(added_ta, added_tb) });
        let owner_err = result_ta.expect_err("owner load must fail (injected E failure)");
        assert!(
            matches!(owner_err, AddModuleError::MaterializeFailed { .. }),
            "owner must surface the injected materialize failure, got: {owner_err:?}",
        );
        let waiter_err =
            result_tb.expect_err("waiter must fail loudly when the owner fails");
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
        let ident =
            ModuleIdent::any(Org::new("tatolab").unwrap(), Package::new("guard-pkg").unwrap());
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
    fn remove_module_returns_hot_reload_lifecycle_deferral() {
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .remove_module(ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("remove-module-stub").unwrap(),
            ))
            .expect_err("remove_module must error until hot-reload ships");
        assert!(
            matches!(
                err,
                RemoveModuleError::HotReloadLifecycleNotYetImplemented { ref module }
                    if module.name.as_str() == "remove-module-stub"
            ),
            "got: {err:?}",
        );
    }

    // =====================================================================
    // install / run split (#1221)
    //
    // Exercises the full resolver handoff: `install` resolves range→concrete,
    // materializes every package into the installed-package cache, and writes
    // the application lockfile; a locked run consumes that lockfile strictly
    // from the cache with NO live re-resolution — so it works offline even
    // against a poisoned / unreachable registry.
    // =====================================================================

    use std::io::Write as _;

    /// Orchestrator that stages a `PackageDir` into the installed-package
    /// cache slot `cache/packages/<name>-<version>/` — where a locked run
    /// looks — by copying the resolved source tree. No toolchain: enough to
    /// prove the resolve→materialize→lock→locked-run handoff with
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
            let cfg = crate::core::config::ProjectConfig::load(&src).map_err(|e| {
                BuildError::Other {
                    package: request.package.to_string(),
                    detail: e.to_string(),
                }
            })?;
            let pkg = cfg.package.as_ref().ok_or_else(|| BuildError::Other {
                package: request.package.to_string(),
                detail: "no [package] block".into(),
            })?;
            let slot = crate::core::get_cached_package_dir(&format!(
                "{}-{}",
                pkg.name.as_str(),
                pkg.version
            ));
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
    /// `<mirror>/<name>/<version>/<name>.slpkg`. `dep` optionally adds a
    /// registry-flavored dependency edge (`@tatolab/<dep>: "^<range>"`).
    fn write_mirror_slpkg(
        mirror: &std::path::Path,
        name: &str,
        version: &str,
        type_name: &str,
        dep: Option<(&str, &str)>,
    ) {
        let dir = mirror.join(name).join(version);
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
        let schema = format!("metadata:\n  type: {type_name}\n  max_payload_bytes: 4096\n");
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
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL", "STREAMLIB_REGISTRY_URL"]);

        // Two-level registry tree: lockrun-lib depends on lockrun-core.
        let mirror = tempfile::tempdir().unwrap();
        write_mirror_slpkg(mirror.path(), "lockrun-core", "0.1.0", "LockrunCoreSchema", None);
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
        assert!(names.contains(&"@tatolab/lockrun-lib".to_string()), "{names:?}");
        assert!(names.contains(&"@tatolab/lockrun-core".to_string()), "{names:?}");
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
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL", "STREAMLIB_REGISTRY_URL"]);

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
            "metadata:\n  type: DetDepSchema\n  max_payload_bytes: 1024\n",
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
        assert_eq!(a, b, "two installs of identical inputs must produce identical lockfile bytes");

        // Path deps are recorded as `path:` sources (the link-bridge shape:
        // a linked/local tree records Path entries).
        let text = String::from_utf8(a).unwrap();
        assert!(text.contains("kind: path"), "path dep must record a path source:\n{text}");
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
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL", "STREAMLIB_REGISTRY_URL"]);

        // Hand-stage a package whose manifest declares a dep on `miss-dep`,
        // and a lockfile that pins ONLY the package (stale relative to the
        // manifest graph).
        let slot = crate::core::get_cached_package_dir("miss-pkg-0.1.0");
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
            "metadata:\n  type: MissPkgSchema\n  max_payload_bytes: 1024\n",
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
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL", "STREAMLIB_REGISTRY_URL"]);

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

    /// Hand-stage a schemas-only package into the installed cache slot for
    /// `name`@`version` and return `(slot_dir, content_hash)`.
    fn stage_schemas_only_slot(
        name: &str,
        version: &str,
        type_name: &str,
    ) -> (std::path::PathBuf, String) {
        let slot = crate::core::get_cached_package_dir_for_name_version(name, version);
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
            format!("metadata:\n  type: {type_name}\n  max_payload_bytes: 1024\n"),
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
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL", "STREAMLIB_REGISTRY_URL"]);

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
            "metadata:\n  type: TamperPkgSchema\n  max_payload_bytes: 9999\n",
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
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL", "STREAMLIB_REGISTRY_URL"]);

        // The slot lives at the LOCKED version's key (drift-pkg-1.0.0), but
        // its manifest inside claims 1.0.1 — an in-place republish that kept
        // the dir name. Pin the drifted slot's REAL hash so the content gate
        // passes and the walker's version check is the one that fires.
        let slot = crate::core::get_cached_package_dir_for_name_version("drift-pkg", "1.0.0");
        std::fs::create_dir_all(slot.join("schemas")).unwrap();
        std::fs::write(
            slot.join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: drift-pkg\n  version: \"1.0.1\"\n\
             schemas:\n  DriftPkgSchema:\n    file: schemas/driftpkgschema.yaml\n",
        )
        .unwrap();
        std::fs::write(
            slot.join("schemas/driftpkgschema.yaml"),
            "metadata:\n  type: DriftPkgSchema\n  max_payload_bytes: 1024\n",
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
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL", "STREAMLIB_REGISTRY_URL"]);

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
        let _clear = EnvVarsCleared::new(&["STREAMLIB_REGISTRY_URL", "STREAMLIB_REGISTRY_URL"]);

        let mirror = tempfile::tempdir().unwrap();
        write_mirror_slpkg(mirror.path(), "mutate-pkg", "0.1.0", "MutatePkgSchema", None);

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
        write_mirror_slpkg(mirror.path(), "mutate-pkg", "0.2.0", "MutatePkgSchema", None);

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
