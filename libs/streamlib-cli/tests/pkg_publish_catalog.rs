// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! End-to-end `streamlib pkg publish` against a scratch `file://` registry
//! tree, driving the real `streamlib` binary. Proves the publish path emits the
//! catalog artifacts (per-package `<name>.catalog.json`, owned schema JTD at the
//! release-core path, and the tree-wide aggregate) alongside the `.slpkg` — so a
//! registry populated purely by `pkg publish` is catalog-fetchable, not degraded
//! to "no metadata."
//!
//! This is the CLI-wiring counterpart to `streamlib-pack`'s
//! `catalog_pkg_publish.rs` (which drives the pack functions directly): here the
//! whole command runs, including the sibling-resolution + tree-root derivation
//! `publish()` does. It uses a schema-only package (no processors) so the
//! source-only assemble needs no Rust/Python toolchain.

use std::path::Path;
use std::process::Command;

use streamlib_idents::{CatalogClient, Org, Package, SchemaIdent, SemVer, TypeName};

const BIN: &str = env!("CARGO_BIN_EXE_streamlib");

/// A schema-only `foo` package (one owned schema, no processors) — the smallest
/// publishable package that assembles without a toolchain build.
fn write_foo_package(dir: &Path) {
    std::fs::create_dir_all(dir.join("schemas")).unwrap();
    std::fs::write(
        dir.join("streamlib.yaml"),
        "package:\n  org: tatolab\n  name: foo\n  version: 1.1.0\n  \
         description: a demo publish package\nschemas:\n  FooFrame:\n    file: schemas/foo_frame.yaml\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("schemas/foo_frame.yaml"),
        "metadata:\n  type: FooFrame\n  description: \"A demo frame\"\nproperties:\n  \
         width:\n    type: uint32\n  height:\n    type: uint32\n",
    )
    .unwrap();
}

#[test]
fn pkg_publish_writes_fetchable_catalog_and_owned_jtd() {
    let tree = tempfile::tempdir().unwrap();
    // The package lives inside a `packages/` parent so publish's sibling scan
    // has a realistic layout to enumerate.
    let workspace = tempfile::tempdir().unwrap();
    let pkg_dir = workspace.path().join("packages").join("foo");
    write_foo_package(&pkg_dir);

    let registry = format!("file://{}", tree.path().display());
    let out = Command::new(BIN)
        .args(["pkg", "publish"])
        .current_dir(&pkg_dir)
        .env("STREAMLIB_REGISTRY_URL", &registry)
        .output()
        .expect("spawn streamlib binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "pkg publish failed: status={:?}\nstdout={stdout}\nstderr={stderr}",
        out.status
    );

    // The `.slpkg` and the per-package catalog both land in the version dir.
    let ver_dir = tree.path().join("slpkg/foo/1.1.0");
    assert!(
        ver_dir.join("foo.slpkg").is_file(),
        "the .slpkg was published"
    );
    assert!(
        ver_dir.join("foo.catalog.json").is_file(),
        "pkg publish must write the per-package catalog"
    );
    // Release-core == full here (no prerelease); the owned JTD lands under it.
    assert!(
        ver_dir.join("schemas/FooFrame.jtd.json").is_file(),
        "pkg publish must write the owned schema JTD"
    );

    // The catalog is fetchable by a client (NOT None — the degraded state this
    // feature closes). foo is schema-only, so it contributes zero processors.
    let client = CatalogClient::new(&registry, None);
    let catalog = client
        .fetch_package_catalog(
            &streamlib_idents::PackageRef::new(
                Org::new("tatolab").unwrap(),
                Package::new("foo").unwrap(),
            ),
            &SemVer::new(1, 1, 0),
        )
        .unwrap()
        .expect("catalog fetchable from a pkg-publish-only tree");
    assert_eq!(catalog.package.to_string(), "@tatolab/foo");
    assert!(
        catalog.processors.is_empty(),
        "schema-only package has no processors"
    );

    // The owned JTD resolves by its release-core ident.
    let jtd = client
        .fetch_schema_type_definition(&SchemaIdent::new(
            Org::new("tatolab").unwrap(),
            Package::new("foo").unwrap(),
            TypeName::new("FooFrame").unwrap(),
            SemVer::new(1, 1, 0),
        ))
        .unwrap()
        .expect("owned FooFrame JTD fetchable");
    assert_eq!(jtd["metadata"]["type"], "FooFrame");
}

/// A package that imports a schema from a dependency not resolvable at publish
/// time must fail loud AND write nothing — the catalog is assembled before the
/// `.slpkg` is uploaded, so an unresolvable external ref can't leave an orphan
/// artifact in the tree. (Mentally move the catalog build after `upload_slpkg`
/// and the `.slpkg` would land before the failure — this test catches that.)
#[test]
fn pkg_publish_fails_before_writing_when_external_ref_unresolvable() {
    let tree = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    // `bar` alone in its parent dir — its `@tatolab/core` import has no local
    // sibling to resolve against and is not published, so catalog assembly must
    // surface `ExternalDepMissing`.
    let pkg_dir = workspace.path().join("packages").join("bar");
    std::fs::create_dir_all(&pkg_dir).unwrap();
    std::fs::write(
        pkg_dir.join("streamlib.yaml"),
        r#"
package:
  org: tatolab
  name: bar
  version: 1.0.0
schemas:
  VideoFrame:
    package: '@tatolab/core'
processors:
- name: Bar
  version: 1.0.0
  runtime: rust
  execution: reactive
  outputs:
  - name: out
    schema: VideoFrame
"#,
    )
    .unwrap();

    let registry = format!("file://{}", tree.path().display());
    let out = Command::new(BIN)
        .args(["pkg", "publish"])
        .current_dir(&pkg_dir)
        .env("STREAMLIB_REGISTRY_URL", &registry)
        .output()
        .expect("spawn streamlib binary");
    assert!(
        !out.status.success(),
        "publish must fail when an external schema ref can't be resolved"
    );
    // Nothing landed in the tree — the failure preceded the `.slpkg` upload.
    assert!(
        !tree.path().join("slpkg/bar").exists(),
        "no .slpkg / catalog should be written on a fail-fast catalog error"
    );
}
