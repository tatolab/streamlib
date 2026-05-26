// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Module-loading tests. Runner-lifecycle tests stay in
//! `runtime.rs`'s `tests` module; everything else (strategy
//! resolver, parse_canonical_package_id, resolve_workspace_root,
//! list_available_triples, load_project test fixtures,
//! add_module_tests) lives here.

use super::processor_registration::list_available_triples;
use super::workspace::{parse_canonical_package_id, resolve_workspace_root};
use super::*;
use serial_test::serial;

// =========================================================================
// parse_canonical_package_id
// =========================================================================

#[test]
fn parse_canonical_package_id_accepts_well_formed_input() {
    // Tightest happy-path lock: every component round-trips
    // (post-`@`, pre-`/`, post-`/`) into the parsed slices. The
    // parser is the contract for what `load_workspace_packages`
    // treats as a legal id — a regression that flipped `org` and
    // `name` would break the lookup silently.
    let parsed = parse_canonical_package_id("@tatolab/camera").unwrap();
    assert_eq!(parsed.org_str, "tatolab");
    assert_eq!(parsed.name_str, "camera");
}

#[test]
fn parse_canonical_package_id_rejects_missing_at_prefix() {
    let err = parse_canonical_package_id("tatolab/camera").unwrap_err();
    assert!(matches!(
        err,
        LoadWorkspacePackagesError::InvalidPackageId(ref s) if s == "tatolab/camera"
    ));
}

#[test]
fn parse_canonical_package_id_rejects_missing_slash() {
    let err = parse_canonical_package_id("@tatolab").unwrap_err();
    assert!(matches!(
        err,
        LoadWorkspacePackagesError::InvalidPackageId(ref s) if s == "@tatolab"
    ));
}

#[test]
fn parse_canonical_package_id_rejects_empty_org_or_name() {
    for bad in ["@/camera", "@tatolab/", "@/"] {
        let err = parse_canonical_package_id(bad).unwrap_err();
        assert!(matches!(err, LoadWorkspacePackagesError::InvalidPackageId(_)));
    }
}

#[test]
fn parse_canonical_package_id_rejects_extra_slashes() {
    let err = parse_canonical_package_id("@org/sub/name").unwrap_err();
    assert!(matches!(err, LoadWorkspacePackagesError::InvalidPackageId(_)));
}

#[test]
fn parse_canonical_package_id_rejects_uppercase_via_typed_validator() {
    let err = parse_canonical_package_id("@TaToLaB/camera").unwrap_err();
    assert!(matches!(err, LoadWorkspacePackagesError::InvalidPackageId(_)));
    let err = parse_canonical_package_id("@tatolab/CAMERA").unwrap_err();
    assert!(matches!(err, LoadWorkspacePackagesError::InvalidPackageId(_)));
}

// =========================================================================
// resolve_workspace_root
// =========================================================================

#[test]
#[serial]
fn resolve_workspace_root_honors_streamlib_workspace_root_env_var() {
    // Test fixture: tempdir set via STREAMLIB_WORKSPACE_ROOT must
    // win over the cargo-locate-project fallback.
    let tmp = tempfile::tempdir().unwrap();
    let key = "STREAMLIB_WORKSPACE_ROOT";
    let prev = std::env::var_os(key);
    // SAFETY: protected by `#[serial]` against parallel test mutation.
    unsafe {
        std::env::set_var(key, tmp.path());
    }
    let resolved = resolve_workspace_root().unwrap();
    unsafe {
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
    assert_eq!(resolved, tmp.path());
}

#[test]
#[serial]
fn resolve_workspace_root_errors_when_env_var_path_does_not_exist() {
    let key = "STREAMLIB_WORKSPACE_ROOT";
    let prev = std::env::var_os(key);
    unsafe {
        std::env::set_var(key, "/nonexistent/path/that/does/not/exist");
    }
    let err = resolve_workspace_root().unwrap_err();
    unsafe {
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
    assert!(matches!(err, LoadWorkspacePackagesError::WorkspaceRootNotFound));
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
// load_workspace_packages
// =========================================================================

#[test]
#[serial]
fn load_workspace_packages_reports_not_staged_when_dir_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let key = "STREAMLIB_WORKSPACE_ROOT";
    let prev = std::env::var_os(key);
    unsafe {
        std::env::set_var(key, tmp.path());
    }
    let runtime = Runner::new().expect("Runner::new");
    let err = runtime
        .load_workspace_packages(["@tatolab/camera"])
        .unwrap_err();
    unsafe {
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
    assert!(matches!(
        err,
        LoadWorkspacePackagesError::PackageNotStaged { ref name, ref expected_path }
            if name == "@tatolab/camera"
            && expected_path.ends_with("tatolab__camera")
    ));
}

#[test]
#[serial]
fn load_workspace_packages_returns_invalid_id_before_filesystem_probe() {
    let tmp = tempfile::tempdir().unwrap();
    let bogus = tmp.path().join("nowhere");
    let key = "STREAMLIB_WORKSPACE_ROOT";
    let prev = std::env::var_os(key);
    unsafe {
        std::env::set_var(key, &bogus);
    }
    let runtime = Runner::new().expect("Runner::new");
    let err = runtime
        .load_workspace_packages(["bad-no-at"])
        .unwrap_err();
    unsafe {
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
    assert!(
        matches!(
            err,
            LoadWorkspacePackagesError::InvalidPackageId(ref s) if s == "bad-no-at"
        ),
        "expected InvalidPackageId surfaced before workspace resolution, got: {err:?}"
    );
}

// =========================================================================
// load_project dep walker
// =========================================================================

/// Path-style dep recursion: `runtime.load_project(A)` must walk into
/// `B` (declared as `path: ../b`) and parse its manifest.
#[test]
#[serial]
fn test_load_project_recurses_into_path_dep() {
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
        .load_project(&a)
        .expect("load_project should recurse into path dep without error");
}

#[test]
#[serial]
fn test_load_project_registers_package_schemas_for_runtime_lookup() {
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

    let canonical =
        "@tatolab/test-load-project-registers-schemas/MyTestConfig";

    assert!(
        crate::core::embedded_schemas::get_embedded_schema_definition(canonical).is_none(),
        "fresh canonical id must not exist before load_project"
    );

    runtime
        .load_project(&pkg)
        .expect("load_project must succeed for schemas-only package");

    let body = crate::core::embedded_schemas::get_embedded_schema_definition(canonical)
        .expect("registered schema must be discoverable post-load");
    assert!(body.contains("MyTestConfig"));
    let port_spec = streamlib_processor_schema::PortSchemaSpec::Specific(
        streamlib_idents::SchemaIdent::new(
            streamlib_idents::Org::new("tatolab").unwrap(),
            streamlib_idents::Package::new(
                "test-load-project-registers-schemas",
            )
            .unwrap(),
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
fn test_load_project_path_dep_missing_manifest_propagates_error() {
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

    let result = runtime.load_project(&a);
    assert!(
        result.is_err(),
        "load_project must error when a path dep target has no streamlib.yaml"
    );
}

#[test]
#[serial]
fn test_load_project_resolves_registry_dep_via_consumer_patch() {
    let runtime = Runner::new().unwrap();
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
        .load_project(&a)
        .expect("consumer-scoped patch must resolve the registry dep to ../b/");
}

#[test]
#[serial]
fn test_load_project_resolves_git_patch_via_shared_helper() {
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

    let runtime = Runner::new().unwrap();
    runtime
        .load_project(consumer.path())
        .expect("git patch must clone the local repo and recurse into it");
}

#[test]
#[serial]
fn test_load_project_strict_errors_on_missing_patch_path() {
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
        .load_project(tmp.path())
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

/// Drops a previously-saved `STREAMLIB_HOME` environment variable
/// state when the test scope ends, so a sandboxed `STREAMLIB_HOME`
/// override doesn't leak into the next `#[serial]` test.
struct StreamlibHomeRestore(Option<std::ffi::OsString>);
impl Drop for StreamlibHomeRestore {
    fn drop(&mut self) {
        // SAFETY: `#[serial]` makes every test in this module
        // exclusive — no concurrent reader of `STREAMLIB_HOME`.
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
fn test_load_project_resolves_registry_dep_via_installed_cache() {
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
        .load_project(consumer.path())
        .expect("registry dep must resolve via installed-package cache");
}

#[test]
#[serial]
fn test_load_project_unresolvable_registry_dep_errors_actionably() {
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
        .load_project(&a)
        .expect_err("unresolvable registry dep must error");
    let msg = format!("{}", err);
    assert!(
        msg.contains("@tatolab/missing"),
        "error must surface the canonical `@org/name` key, got: {msg}"
    );
    assert!(
        msg.contains("streamlib pkg install") || msg.contains("workspace"),
        "error must point at the resolution paths the user can act on, got: {msg}"
    );
}

#[test]
#[serial]
fn test_load_project_rust_dylib_missing_host_triple_surfaces_available_triples() {
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
        .load_project(&pkg)
        .expect_err("missing host-triple subdir must error");
    let msg = format!("{}", err);
    assert!(
        msg.contains(host_target_triple()),
        "error must name the host triple so the user sees what was expected, got: {msg}"
    );
    assert!(
        msg.contains(wrong_triple),
        "error must list the triples that ARE present so the user sees what the slpkg was packed for, got: {msg}"
    );
}

#[test]
#[serial]
fn test_load_project_rust_dylib_resolves_host_triple_then_dlopens() {
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
        .load_project(&pkg)
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
        "error must NOT be the 'no dylib found' variant (path resolution succeeded), got: {msg}"
    );
}

#[test]
#[serial]
fn test_load_project_schemas_only_skips_lib_lookup() {
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
        .load_project(&pkg)
        .expect("schemas-only package must load without touching lib/");
}

// =========================================================================
// add_module / remove_module — imperative module API
// =========================================================================

mod add_module_tests {
    use super::*;
    use streamlib_idents::{ModuleIdent, Org, Package, SemVer, SemVerRange};

    /// RAII guard that restores `STREAMLIB_WORKSPACE_ROOT` and
    /// `STREAMLIB_HOME` to their pre-test values on drop. Every
    /// `add_module` test isolates both env vars to a tempdir to
    /// keep the host's real workspace / installed cache out of the
    /// test surface (per the side-effect-cleanup discipline in
    /// `docs/testing.md` — flag tests that mutate global state).
    struct AddModuleEnvGuard {
        workspace_prev: Option<std::ffi::OsString>,
        home_prev: Option<std::ffi::OsString>,
    }

    impl AddModuleEnvGuard {
        fn install(workspace_root: &std::path::Path, home_root: &std::path::Path) -> Self {
            let workspace_prev = std::env::var_os("STREAMLIB_WORKSPACE_ROOT");
            let home_prev = std::env::var_os("STREAMLIB_HOME");
            unsafe {
                std::env::set_var("STREAMLIB_WORKSPACE_ROOT", workspace_root);
                std::env::set_var("STREAMLIB_HOME", home_root);
            }
            Self { workspace_prev, home_prev }
        }
    }

    impl Drop for AddModuleEnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.workspace_prev.take() {
                    Some(v) => std::env::set_var("STREAMLIB_WORKSPACE_ROOT", v),
                    None => std::env::remove_var("STREAMLIB_WORKSPACE_ROOT"),
                }
                match self.home_prev.take() {
                    Some(v) => std::env::set_var("STREAMLIB_HOME", v),
                    None => std::env::remove_var("STREAMLIB_HOME"),
                }
            }
        }
    }

    /// Build a schemas-only `streamlib.yaml` at `dir` with the
    /// given org/name/version. Schemas-only avoids the dylib-
    /// loading branch — `add_module` resolution + version-range
    /// matching is what we want to lock here, not the cdylib
    /// mechanics that `load_workspace_packages` already covers.
    ///
    /// When `schema` is provided, emits a `schemas:` block plus
    /// the named schema YAML under `schemas/<type_snake>.yaml`
    /// so post-load callers can verify the schema actually
    /// registered (locks "`load_project` ran" — not just "resolver
    /// + range check ran").
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
                format!(
                    "metadata:\n  type: {type_name}\n  max_payload_bytes: 4096\n"
                ),
            )
            .unwrap();
            format!(
                "package:\n  org: {org}\n  name: {name}\n  version: \"{version}\"\n\
                 schemas:\n  {type_name}:\n    file: schemas/{stem}.yaml\n"
            )
        } else {
            format!(
                "package:\n  org: {org}\n  name: {name}\n  version: \"{version}\"\n"
            )
        };
        std::fs::write(dir.join("streamlib.yaml"), body)
            .expect("write streamlib.yaml");
    }

    /// Stage a package under
    /// `<workspace>/target/streamlib-plugins/<org>__<name>/`
    /// and return the staged dir. Mirrors what
    /// `cargo xtask build-plugins` produces.
    fn stage_workspace_package(
        workspace_root: &std::path::Path,
        org: &str,
        name: &str,
        version: &str,
        schema: Option<&str>,
    ) -> std::path::PathBuf {
        let staged = workspace_root
            .join("target")
            .join("streamlib-plugins")
            .join(format!("{org}__{name}"));
        std::fs::create_dir_all(&staged).expect("mkdir staged");
        write_schemas_only_manifest(&staged, org, name, version, schema);
        staged
    }

    #[test]
    #[serial]
    fn add_module_resolves_workspace_stage_and_loads() {
        const TYPE_NAME: &str = "AddModuleResolvesStageSchema";
        let tmp = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _staged = stage_workspace_package(
            tmp.path(),
            "tatolab",
            "add-module-stage",
            "1.2.3",
            Some(TYPE_NAME),
        );

        let _guard = AddModuleEnvGuard::install(tmp.path(), home.path());
        let runtime = Runner::new().expect("Runner::new");

        let canonical = format!("@tatolab/add-module-stage/{TYPE_NAME}");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical)
                .is_none(),
            "schema must not exist before add_module"
        );

        runtime
            .add_module(ModuleIdent::new(
                Org::new("tatolab").unwrap(),
                Package::new("add-module-stage").unwrap(),
                SemVerRange::Caret(SemVer::new(1, 0, 0)),
            ))
            .expect("workspace-staged add_module must succeed");

        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical)
                .is_some(),
            "schema must be registered after add_module — load_project did not run if this fails"
        );
    }

    #[test]
    #[serial]
    fn add_module_any_version_succeeds_against_staged_v1() {
        const TYPE_NAME: &str = "AddModuleAnyVersionSchema";
        let tmp = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _staged = stage_workspace_package(
            tmp.path(),
            "tatolab",
            "add-module-any",
            "0.4.0",
            Some(TYPE_NAME),
        );

        let _guard = AddModuleEnvGuard::install(tmp.path(), home.path());
        let runtime = Runner::new().expect("Runner::new");

        let canonical = format!("@tatolab/add-module-any/{TYPE_NAME}");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical)
                .is_none(),
            "schema must not exist before add_module"
        );

        runtime
            .add_module(ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("add-module-any").unwrap(),
            ))
            .expect("any-version add_module must succeed against staged 0.4.0");

        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical)
                .is_some(),
            "any-version add_module must invoke load_project; schema registry is the witness",
        );
    }

    #[test]
    #[serial]
    fn add_module_rejects_version_range_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _staged = stage_workspace_package(
            tmp.path(),
            "tatolab",
            "add-module-range",
            "1.0.0",
            None,
        );

        let _guard = AddModuleEnvGuard::install(tmp.path(), home.path());
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module(ModuleIdent::new(
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
    fn add_module_rejects_identity_mismatch_on_staged_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let staged = tmp
            .path()
            .join("target")
            .join("streamlib-plugins")
            .join("tatolab__add-module-identity");
        std::fs::create_dir_all(&staged).unwrap();
        write_schemas_only_manifest(&staged, "vendor", "other", "1.0.0", None);

        let _guard = AddModuleEnvGuard::install(tmp.path(), home.path());
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module(ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("add-module-identity").unwrap(),
            ))
            .expect_err("clobbered staged manifest must error");

        assert!(
            matches!(err, AddModuleError::ManifestIdentityMismatch { ref actual, .. } if actual == "@vendor/other"),
            "expected ManifestIdentityMismatch(@vendor/other), got: {err:?}",
        );
    }

    #[test]
    #[serial]
    fn add_module_reports_module_not_found_when_unstaged_and_uncached() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _guard = AddModuleEnvGuard::install(tmp.path(), home.path());

        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module(ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("add-module-missing").unwrap(),
            ))
            .expect_err("missing module must error");

        assert!(
            matches!(err, AddModuleError::ModuleNotFound { ref package } if package.org.as_str() == "tatolab" && package.name.as_str() == "add-module-missing"),
            "expected ModuleNotFound(@tatolab/add-module-missing), got: {err:?}",
        );
    }

    #[test]
    #[serial]
    fn add_module_surfaces_workspace_root_typo() {
        let home = tempfile::tempdir().unwrap();
        let placeholder_workspace = tempfile::tempdir().unwrap();
        let _guard =
            AddModuleEnvGuard::install(placeholder_workspace.path(), home.path());
        unsafe {
            std::env::set_var(
                "STREAMLIB_WORKSPACE_ROOT",
                "/nonexistent/path/that/does/not/exist",
            );
        }

        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module(ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("add-module-typo-canary").unwrap(),
            ))
            .expect_err("typo'd env var must error");

        assert!(
            matches!(err, AddModuleError::WorkspaceRootInvalid { ref env_value } if env_value == "/nonexistent/path/that/does/not/exist"),
            "expected WorkspaceRootInvalid, got: {err:?}",
        );
    }

    // =====================================================================
    // ModuleResolverStrategy conformance — one test per variant.
    // =====================================================================

    #[test]
    #[serial]
    fn add_module_with_default_chain_falls_through_workspace_to_installed_cache() {
        const TYPE_NAME: &str = "DefaultChainFallsThroughSchema";
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _guard = AddModuleEnvGuard::install(workspace.path(), home.path());

        let cache_root = home.path().join("cache/packages");
        std::fs::create_dir_all(&cache_root).unwrap();
        let dep_cache_dir = cache_root.join("dc-fallthrough-0.1.0");
        std::fs::create_dir(&dep_cache_dir).unwrap();
        write_schemas_only_manifest(
            &dep_cache_dir,
            "tatolab",
            "dc-fallthrough",
            "0.1.0",
            Some(TYPE_NAME),
        );
        let mut installed =
            crate::core::config::InstalledPackageManifest::default();
        installed.add(crate::core::config::InstalledPackageEntry {
            name: streamlib_idents::PackageRef::new(
                Org::new("tatolab").unwrap(),
                Package::new("dc-fallthrough").unwrap(),
            ),
            version: SemVer::new(0, 1, 0),
            description: None,
            installed_from: "test".into(),
            installed_at: "1970-01-01T00:00:00Z".into(),
            cache_dir: "dc-fallthrough-0.1.0".to_string(),
        });
        installed.save().unwrap();

        let runtime = Runner::new().expect("Runner::new");
        let canonical = format!("@tatolab/dc-fallthrough/{TYPE_NAME}");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical)
                .is_none()
        );

        runtime
            .add_module_with(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("dc-fallthrough").unwrap(),
                ),
                ModuleResolverStrategy::DefaultChain,
            )
            .expect("DefaultChain must reach installed cache when workspace has no stage");

        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical)
                .is_some(),
            "DefaultChain's installed-cache tier did not actually run load",
        );
    }

    #[test]
    #[serial]
    fn add_module_with_workspace_staged_strategy_hits_stage_dir_only() {
        const TYPE_NAME: &str = "WorkspaceStagedOnlySchema";
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _staged = stage_workspace_package(
            workspace.path(),
            "tatolab",
            "ws-only",
            "1.0.0",
            Some(TYPE_NAME),
        );
        let _guard = AddModuleEnvGuard::install(workspace.path(), home.path());
        let runtime = Runner::new().expect("Runner::new");
        let canonical = format!("@tatolab/ws-only/{TYPE_NAME}");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical)
                .is_none()
        );
        runtime
            .add_module_with(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("ws-only").unwrap(),
                ),
                ModuleResolverStrategy::WorkspaceStaged,
            )
            .expect("WorkspaceStaged must hit the stage dir");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical)
                .is_some()
        );
    }

    #[test]
    #[serial]
    fn add_module_with_workspace_staged_strategy_surfaces_miss() {
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _guard = AddModuleEnvGuard::install(workspace.path(), home.path());
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module_with(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("ws-only-miss").unwrap(),
                ),
                ModuleResolverStrategy::WorkspaceStaged,
            )
            .expect_err("missing stage dir must error");
        assert!(
            matches!(
                err,
                AddModuleError::WorkspaceStageMiss { ref package, ref expected_path }
                    if package.name.as_str() == "ws-only-miss"
                        && expected_path.ends_with("tatolab__ws-only-miss")
            ),
            "expected WorkspaceStageMiss, got: {err:?}",
        );
    }

    #[test]
    #[serial]
    fn add_module_with_installed_cache_strategy_hits_cache_only() {
        const TYPE_NAME: &str = "InstalledCacheOnlySchema";
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let cache_root = home.path().join("cache/packages");
        std::fs::create_dir_all(&cache_root).unwrap();
        let dep_cache_dir = cache_root.join("ic-only-1.0.0");
        std::fs::create_dir(&dep_cache_dir).unwrap();
        write_schemas_only_manifest(
            &dep_cache_dir,
            "tatolab",
            "ic-only",
            "1.0.0",
            Some(TYPE_NAME),
        );
        let mut installed =
            crate::core::config::InstalledPackageManifest::default();
        installed.add(crate::core::config::InstalledPackageEntry {
            name: streamlib_idents::PackageRef::new(
                Org::new("tatolab").unwrap(),
                Package::new("ic-only").unwrap(),
            ),
            version: SemVer::new(1, 0, 0),
            description: None,
            installed_from: "test".into(),
            installed_at: "1970-01-01T00:00:00Z".into(),
            cache_dir: "ic-only-1.0.0".to_string(),
        });
        let _guard = AddModuleEnvGuard::install(workspace.path(), home.path());
        installed.save().unwrap();

        let runtime = Runner::new().expect("Runner::new");
        let canonical = format!("@tatolab/ic-only/{TYPE_NAME}");
        runtime
            .add_module_with(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("ic-only").unwrap(),
                ),
                ModuleResolverStrategy::InstalledCache,
            )
            .expect("InstalledCache strategy must hit the cache");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical)
                .is_some()
        );
    }

    #[test]
    #[serial]
    fn add_module_with_manifest_directory_loads_arbitrary_dir() {
        const TYPE_NAME: &str = "ManifestDirectorySchema";
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let arbitrary = tempfile::tempdir().unwrap();
        write_schemas_only_manifest(
            arbitrary.path(),
            "tatolab",
            "md-arbitrary",
            "0.7.2",
            Some(TYPE_NAME),
        );
        let _guard = AddModuleEnvGuard::install(workspace.path(), home.path());
        let runtime = Runner::new().expect("Runner::new");
        let canonical = format!("@tatolab/md-arbitrary/{TYPE_NAME}");
        runtime
            .add_module_with(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("md-arbitrary").unwrap(),
                ),
                ModuleResolverStrategy::ManifestDirectory {
                    path: arbitrary.path().to_path_buf(),
                },
            )
            .expect("ManifestDirectory must load the arbitrary dir");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical)
                .is_some()
        );
    }

    #[test]
    #[serial]
    fn add_module_with_manifest_directory_surfaces_missing_dir() {
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _guard = AddModuleEnvGuard::install(workspace.path(), home.path());
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module_with(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("md-missing").unwrap(),
                ),
                ModuleResolverStrategy::ManifestDirectory {
                    path: std::path::PathBuf::from("/nonexistent/does-not-exist"),
                },
            )
            .expect_err("missing manifest dir must error");
        assert!(
            matches!(err, AddModuleError::ManifestDirectoryMissing { .. }),
            "expected ManifestDirectoryMissing, got: {err:?}",
        );
    }

    #[test]
    #[serial]
    fn add_module_with_explicit_strategy_overrides_default_chain() {
        const TYPE_NAME: &str = "PerCallOverrideSchema";
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();

        let _staged = stage_workspace_package(
            workspace.path(),
            "tatolab",
            "per-call-override",
            "0.1.0",
            None,
        );

        let overridden = tempfile::tempdir().unwrap();
        write_schemas_only_manifest(
            overridden.path(),
            "tatolab",
            "per-call-override",
            "9.0.0",
            Some(TYPE_NAME),
        );

        let _guard = AddModuleEnvGuard::install(workspace.path(), home.path());
        let runtime = Runner::new().expect("Runner::new");

        runtime
            .add_module_with(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("per-call-override").unwrap(),
                ),
                ModuleResolverStrategy::ManifestDirectory {
                    path: overridden.path().to_path_buf(),
                },
            )
            .expect("explicit override must reach the resolver");

        let canonical = format!("@tatolab/per-call-override/{TYPE_NAME}");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical)
                .is_some(),
            "explicit strategy didn't actually divert the resolver — schema from \
             overridden path is missing",
        );
    }

    // =====================================================================
    // Recursive dep walker
    // =====================================================================

    #[test]
    #[serial]
    fn add_module_recurses_through_add_module_with_for_each_dep() {
        const TYPE_A: &str = "DepWalkAllThreeASchema";
        const TYPE_B: &str = "DepWalkAllThreeBSchema";
        const TYPE_C: &str = "DepWalkAllThreeCSchema";
        let workspace = tempfile::tempdir().unwrap();
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

        let _guard = AddModuleEnvGuard::install(workspace.path(), home.path());
        let runtime = Runner::new().expect("Runner::new");
        runtime
            .add_module_with(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("dwa-a").unwrap(),
                ),
                ModuleResolverStrategy::ManifestDirectory { path: a.clone() },
            )
            .expect("dep walker must succeed end-to-end");
        for (pkg, ty) in [
            ("dwa-a", TYPE_A),
            ("dwa-b", TYPE_B),
            ("dwa-c", TYPE_C),
        ] {
            let canonical = format!("@tatolab/{pkg}/{ty}");
            assert!(
                crate::core::embedded_schemas::get_embedded_schema_definition(&canonical)
                    .is_some(),
                "expected schema {canonical} registered after dep walk",
            );
        }
    }

    #[test]
    #[serial]
    fn add_module_detects_self_referential_cycle() {
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let pkg = tempfile::tempdir().unwrap();
        std::fs::write(
            pkg.path().join("streamlib.yaml"),
            "package:\n  org: tatolab\n  name: cycle-self\n  version: \"0.1.0\"\n\
             dependencies:\n  \"@tatolab/cycle-self\":\n    path: .\n",
        )
        .unwrap();
        let _guard = AddModuleEnvGuard::install(workspace.path(), home.path());
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module_with(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("cycle-self").unwrap(),
                ),
                ModuleResolverStrategy::ManifestDirectory {
                    path: pkg.path().to_path_buf(),
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
    fn add_module_detects_mutual_cycle_a_to_b_to_a() {
        let workspace = tempfile::tempdir().unwrap();
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
        let _guard = AddModuleEnvGuard::install(workspace.path(), home.path());
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module_with(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("cycle-a").unwrap(),
                ),
                ModuleResolverStrategy::ManifestDirectory { path: a.clone() },
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
    fn add_module_surfaces_version_mismatch_propagating_from_dep() {
        let workspace = tempfile::tempdir().unwrap();
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
        let _guard = AddModuleEnvGuard::install(workspace.path(), home.path());
        let runtime = Runner::new().expect("Runner::new");
        let err = runtime
            .add_module_with(
                ModuleIdent::any(
                    Org::new("tatolab").unwrap(),
                    Package::new("vmm-a").unwrap(),
                ),
                ModuleResolverStrategy::ManifestDirectory { path: a.clone() },
            )
            .expect_err("version mismatch from dep must propagate");
        assert!(
            matches!(err, AddModuleError::VersionRangeUnsatisfied { ref module, found, .. }
                if module.name.as_str() == "vmm-b" && found == SemVer::new(1, 0, 0)),
            "expected VersionRangeUnsatisfied for vmm-b@1.0.0, got: {err:?}",
        );
    }

    #[test]
    #[serial]
    fn add_module_default_path_keeps_default_chain_when_no_override() {
        const TYPE_NAME: &str = "DefaultChainStaysSchema";
        let workspace = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _staged = stage_workspace_package(
            workspace.path(),
            "tatolab",
            "default-chain-stays",
            "1.0.0",
            Some(TYPE_NAME),
        );
        let _guard = AddModuleEnvGuard::install(workspace.path(), home.path());
        let runtime = Runner::new().expect("Runner::new");
        runtime
            .add_module(ModuleIdent::any(
                Org::new("tatolab").unwrap(),
                Package::new("default-chain-stays").unwrap(),
            ))
            .expect("bare add_module must keep DefaultChain behavior");
        let canonical = format!("@tatolab/default-chain-stays/{TYPE_NAME}");
        assert!(
            crate::core::embedded_schemas::get_embedded_schema_definition(&canonical)
                .is_some()
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
