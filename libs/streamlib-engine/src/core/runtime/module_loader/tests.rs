// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Module-loading tests. Runner-lifecycle tests stay in
//! `runtime.rs`'s `tests` module; everything else (strategy
//! resolver, resolve_workspace_root, list_available_triples,
//! `add_module_with` dep-walker fixtures) lives here.

use super::processor_registration::list_available_triples;
use super::workspace::resolve_workspace_root;
use super::*;
use serial_test::serial;

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
    assert!(matches!(err, AddModuleError::WorkspaceRootInvalid { .. }));
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
// add_module_with(WorkspaceStaged)
// =========================================================================

/// Workspace stage dir missing for the requested package surfaces as
/// the typed [`AddModuleError::WorkspaceStageMiss`] with the expected
/// path the resolver looked at. Exercises the same failure mode the
/// old `load_workspace_packages` wrapper translated into its
/// `PackageNotStaged` variant.
#[test]
#[serial]
fn add_module_with_workspace_staged_reports_stage_miss_when_dir_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let key = "STREAMLIB_WORKSPACE_ROOT";
    let prev = std::env::var_os(key);
    unsafe {
        std::env::set_var(key, tmp.path());
    }
    let runtime = Runner::new().expect("Runner::new");
    let ident = streamlib_idents::ModuleIdent::any(
        streamlib_idents::Org::new("tatolab").unwrap(),
        streamlib_idents::Package::new("camera").unwrap(),
    );
    let err = runtime
        .add_module_with(ident, ModuleResolverStrategy::WorkspaceStaged)
        .unwrap_err();
    unsafe {
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
    assert!(matches!(
        err,
        AddModuleError::WorkspaceStageMiss { ref package, ref expected_path }
            if package.org.as_str() == "tatolab"
            && package.name.as_str() == "camera"
            && expected_path.ends_with("tatolab__camera")
    ));
}

// =========================================================================
// add_module_with(ManifestDirectory) dep walker
// =========================================================================

/// Path-style dep recursion: `add_module_with(ManifestDirectory(A))`
/// must walk into `B` (declared as `path: ../b`) and parse its manifest.
#[test]
#[serial]
fn test_add_module_with_manifest_directory_recurses_into_path_dep() {
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
        .add_module_with(
            streamlib_idents::ModuleIdent::any(
                streamlib_idents::Org::new("tatolab").unwrap(),
                streamlib_idents::Package::new("a").unwrap(),
            ),
            ModuleResolverStrategy::ManifestDirectory { path: a.clone() },
        )
        .expect("add_module_with should recurse into path dep without error");
}
