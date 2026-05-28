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

#[test]
#[serial]
fn path_strategy_resolves_registry_dep_via_installed_cache() {
    let sandbox = tempfile::tempdir().unwrap();
    let prev_home = std::env::var_os("STREAMLIB_HOME");
    unsafe {
        std::env::set_var("STREAMLIB_HOME", sandbox.path());
    }
    let _restore = StreamlibHomeRestore(prev_home);

    let cache_root = sandbox.path().join("cache/packages");
    std::fs::create_dir_all(&cache_root).unwrap();
    let dep_cache_dir = cache_root.join("b-0.1.0");
    std::fs::create_dir(&dep_cache_dir).unwrap();
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
        .expect("registry dep must resolve via installed-package cache");
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
    fn install_cached_package(
        home_root: &std::path::Path,
        org: &str,
        name: &str,
        version: &str,
        schema: Option<&str>,
    ) {
        let cache_dir_name = format!("{name}-{version}");
        let dep_cache_dir = home_root.join("cache/packages").join(&cache_dir_name);
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
        install_cached_package(home.path(), "tatolab", "add-module-cache", "1.2.3", Some(TYPE_NAME));

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
        install_cached_package(home.path(), "tatolab", "add-module-range", "1.0.0", None);

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
        // manifest declares a different identity (clobbered cache).
        let dep_cache_dir = home.path().join("cache/packages").join("add-module-identity-1.0.0");
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
        install_cached_package(home.path(), "tatolab", "ic-only", "1.0.0", Some(TYPE_NAME));

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
}
