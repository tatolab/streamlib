// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration: the REAL `emit_static_registry` path (→
//! `emit_slpkg_and_manifest` → `build_and_flip`) writes the per-package
//! catalog + owned JTDs + the `catalog/index.ndjson` aggregate INSIDE the
//! staged tree, so the served tree carries them after the atomic flip.
//! Runs against a hermetic fixture workspace (schema-only + processor
//! packages, no compilable runtimes) so no toolchain beyond `cargo metadata`
//! is exercised. This is the CI lock for the staging-relativity of the
//! catalog emit — the workflow's `cargo test -p streamlib-pack` runs it.

use std::path::Path;

use streamlib_idents::{CatalogClient, Org, Package, PackageRef, SemVer};
use streamlib_pack::static_registry::{EmitOptions, emit_static_registry};

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// Fabricate a minimal cargo workspace root: `[workspace.package].version`
/// for `workspace_version`, one trivial non-closure member so `cargo
/// metadata` yields a resolve graph, and a `packages/` dir holding the
/// fixture streamlib packages.
fn fixture_workspace(root: &Path) {
    write(
        root,
        "Cargo.toml",
        r#"[workspace]
members = ["fixture-member"]
resolver = "2"

[workspace.package]
version = "0.9.9"
"#,
    );
    write(
        root,
        "fixture-member/Cargo.toml",
        "[package]\nname = \"fixture-member\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(root, "fixture-member/src/lib.rs", "");

    // Schema-only package (the @tatolab/core shape).
    let core = root.join("packages/fixcore");
    write(
        &core,
        "streamlib.yaml",
        r#"
package:
  org: tatolab
  name: fixcore
  version: 1.4.0
schemas:
  FixFrame:
    file: schemas/fix_frame.yaml
"#,
    );
    write(
        &core,
        "schemas/fix_frame.yaml",
        "metadata:\n  type: FixFrame\nproperties:\n  width:\n    type: uint32\n",
    );

    // Processor package importing the schema from the sibling (python
    // runtime — nothing is compiled for a source-only `.slpkg`).
    let cam = root.join("packages/fixcam");
    write(
        &cam,
        "streamlib.yaml",
        r#"
package:
  org: tatolab
  name: fixcam
  version: 2.1.0
dependencies:
  '@tatolab/fixcore':
    version: ^1.0.0
schemas:
  FixFrame:
    package: '@tatolab/fixcore'
processors:
- name: FixSource
  runtime: python
  entrypoint: src.fix:FixSource
  execution: reactive
  outputs:
  - name: out
    schema: FixFrame
"#,
    );
    write(&cam, "src/fix.py", "class FixSource:\n    pass\n");
}

#[test]
fn real_emit_writes_catalog_inside_the_flipped_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    fixture_workspace(&workspace);
    let out = tmp.path().join("served");

    emit_static_registry(&EmitOptions {
        workspace_root: workspace,
        out: out.clone(),
        dev: None,
    })
    .expect("real emit path (slpkg + catalog + manifest) succeeds");

    // Everything landed in the SERVED tree via the atomic flip: the .slpkg
    // store, the release manifest (completion marker), the per-package
    // catalogs, the owned JTD, and the aggregate — all present together.
    assert!(out.join("slpkg/fixcam/2.1.0/fixcam.slpkg").is_file());
    assert!(
        out.join("slpkg/streamlib-release/0.9.9/manifest.json")
            .is_file()
    );
    assert!(out.join("slpkg/fixcam/2.1.0/fixcam.catalog.json").is_file());
    assert!(
        out.join("slpkg/fixcore/1.4.0/fixcore.catalog.json")
            .is_file()
    );
    assert!(
        out.join("slpkg/fixcore/1.4.0/schemas/FixFrame.jtd.json")
            .is_file()
    );
    assert!(out.join("catalog/index.ndjson").is_file());
    // No staging remnant beside the served tree.
    let remnants: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".staging."))
        .collect();
    assert!(
        remnants.is_empty(),
        "staging remnant left behind: {remnants:?}"
    );

    // The catalog surface is queryable off the served tree.
    let client = CatalogClient::new(format!("file://{}", out.display()), None);
    let index = client.fetch_processor_index().unwrap();
    assert_eq!(index.len(), 1, "one processor across the release");
    assert_eq!(index[0].processor.name, "FixSource");
    let out_schema = index[0].processor.outputs[0].schema.schema().unwrap();
    assert_eq!(out_schema.to_string(), "@tatolab/fixcore/FixFrame@1.4.0");
    let jtd = client
        .fetch_schema_type_definition(out_schema)
        .unwrap()
        .unwrap();
    assert_eq!(jtd["metadata"]["type"], "FixFrame");
    let cam_ref = PackageRef::new(
        Org::new("tatolab").unwrap(),
        Package::new("fixcam").unwrap(),
    );
    assert!(
        client
            .fetch_package_catalog(&cam_ref, &SemVer::new(2, 1, 0))
            .unwrap()
            .is_some()
    );
}
