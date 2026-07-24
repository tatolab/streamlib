// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration: publish two packages' catalogs into a static `file://` tree,
//! then reconstruct the full processor/port/schema wiring graph by querying
//! the catalog surface ONLY — never opening a `.slpkg` tarball. Exercises the
//! exact writer the tree emit uses (`write_package_catalog` + the aggregate
//! NDJSON) against the exact reader a client uses (`CatalogClient`).

use std::path::{Path, PathBuf};

use streamlib_idents::{
    CATALOG_INDEX_PATH, CatalogClient, CatalogRuntime, CatalogSchemaRef, Org, Package, PackageRef,
    PackageSourceClient, PackageSource, SemVer, render_catalog_index_ndjson,
};
use streamlib_pack::catalog::{build_package_catalog, build_sibling_versions};
use streamlib_pack::static_package_source::{build_and_flip, write_package_catalog};

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn pkg_ref(name: &str) -> PackageRef {
    PackageRef::new(Org::new("tatolab").unwrap(), Package::new(name).unwrap())
}

/// Lay down two package source dirs: `@tatolab/core@1.4.0` (owns VideoFrame)
/// and `@tatolab/camera@2.1.0` (imports VideoFrame from core, owns
/// CameraConfig). Returns the two package dirs.
fn author_two_packages(src: &Path) -> (PathBuf, PathBuf) {
    let core = src.join("core");
    write(
        &core,
        "streamlib.yaml",
        r#"
package:
  org: tatolab
  name: core
  version: 1.4.0
schemas:
  VideoFrame:
    file: schemas/video_frame.yaml
"#,
    );
    write(
        &core,
        "schemas/video_frame.yaml",
        "metadata:\n  type: VideoFrame\n  description: A frame\nproperties:\n  width:\n    type: uint32\n  height:\n    type: uint32\n",
    );

    let camera = src.join("camera");
    write(
        &camera,
        "streamlib.yaml",
        r#"
package:
  org: tatolab
  name: camera
  version: 2.1.0
dependencies:
  '@tatolab/core':
    version: ^1.0.0
schemas:
  CameraConfig:
    file: schemas/camera_config.yaml
  VideoFrame:
    package: '@tatolab/core'
processors:
- name: Camera
  description: Captures video
  runtime: rust
  execution: manual
  config:
    name: config
    schema: CameraConfig
  outputs:
  - name: video
    schema: VideoFrame
    description: Live frames
- name: Sink
  runtime: python
  entrypoint: src.sink:Sink
  execution: reactive
  inputs:
  - name: video_in
    schema: VideoFrame
    delivery_profile: latest
"#,
    );
    write(
        &camera,
        "schemas/camera_config.yaml",
        "metadata:\n  type: CameraConfig\n  description: cfg\noptionalProperties:\n  device_id:\n    type: string\n",
    );
    (core, camera)
}

/// Emit a static tree the way `emit_slpkg_and_manifest` does — upload each
/// `.slpkg` into the store, write each package's catalog + JTD, then the
/// tree-wide aggregate — but hermetically (no cargo/uv/deno).
fn emit_catalog_tree(root: &Path, pkg_dirs: &[PathBuf]) {
    let slpkg = root.join("slpkg");
    std::fs::create_dir_all(&slpkg).unwrap();
    // Package source client rooted at the tree root; it writes under `slpkg/`.
    let cfg = PackageSource {
        base_url: format!("file://{}", root.display()),
    };
    let siblings = build_sibling_versions(pkg_dirs).unwrap();

    let mut index = Vec::new();
    for dir in pkg_dirs {
        let arts = build_package_catalog(dir, &siblings).unwrap();
        // Upload a DUMMY `.slpkg` — the query path must never read it.
        PackageSourceClient::new(&cfg)
            .upload_slpkg(
                &arts.catalog.package,
                arts.catalog.version,
                b"OPAQUE-SLPKG-TARBALL-BYTES-DO-NOT-READ",
            )
            .unwrap();
        write_package_catalog(&slpkg, &arts).unwrap();
        index.extend(arts.index_lines);
    }
    write(
        root,
        CATALOG_INDEX_PATH,
        &render_catalog_index_ndjson(&index),
    );
}

#[test]
fn reconstruct_wiring_graph_from_catalog_without_touching_slpkg() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("packages");
    let tree = tmp.path().join("package-source");
    let (core, camera) = author_two_packages(&src);
    emit_catalog_tree(&tree, &[core, camera]);

    let client = CatalogClient::new(format!("file://{}", tree.display()), None);

    // 1. The tree-wide aggregate carries one line per processor across
    //    both packages — the whole node palette in one fetch.
    let index = client.fetch_processor_index().unwrap();
    let names: Vec<(String, String)> = index
        .iter()
        .map(|l| (l.package.to_string(), l.processor.name.clone()))
        .collect();
    assert!(names.contains(&("@tatolab/camera".into(), "Camera".into())));
    assert!(names.contains(&("@tatolab/camera".into(), "Sink".into())));
    assert_eq!(index.len(), 2, "two processors published");

    // 2. Reconstruct the wiring edge: Camera.video (output) → Sink.video_in
    //    (input). Both carry the SAME resolved release-core ident, sourced
    //    from core's version (1.4.0), NOT camera's (2.1.0).
    let camera_line = index.iter().find(|l| l.processor.name == "Camera").unwrap();
    let sink_line = index.iter().find(|l| l.processor.name == "Sink").unwrap();
    let out_schema = camera_line.processor.outputs[0].schema.schema().unwrap();
    let in_schema = sink_line.processor.inputs[0].schema.schema().unwrap();
    assert_eq!(
        out_schema, in_schema,
        "the wiring edge shares a schema identity"
    );
    assert_eq!(out_schema.to_string(), "@tatolab/core/VideoFrame@1.4.0");
    assert_eq!(
        sink_line.processor.inputs[0].delivery_profile.as_deref(),
        Some("latest")
    );
    assert_eq!(camera_line.processor.runtime, CatalogRuntime::Rust);
    assert_eq!(sink_line.processor.runtime, CatalogRuntime::Python);
    assert_eq!(
        sink_line.processor.entrypoint.as_deref(),
        Some("src.sink:Sink")
    );

    // 3. Per-package catalog fetch (browse-UI path) agrees with the aggregate.
    let cam_catalog = client
        .fetch_package_catalog(&pkg_ref("camera"), &SemVer::new(2, 1, 0))
        .unwrap()
        .unwrap();
    assert_eq!(cam_catalog.processors.len(), 2);
    let cfg_ident = cam_catalog.processors[0]
        .config
        .as_ref()
        .unwrap()
        .schema
        .clone();
    assert_eq!(cfg_ident.to_string(), "@tatolab/camera/CameraConfig@2.1.0");

    // 4. Fetch the field-level schema shape via JTD, resolved from the OWNING
    //    package's version dir — core owns VideoFrame, camera owns CameraConfig.
    let vf_jtd = client
        .fetch_schema_type_definition(out_schema)
        .unwrap()
        .unwrap();
    assert_eq!(vf_jtd["metadata"]["type"], "VideoFrame");
    assert!(vf_jtd["properties"].get("width").is_some());
    let cfg_jtd = client
        .fetch_schema_type_definition(&cfg_ident)
        .unwrap()
        .unwrap();
    assert_eq!(cfg_jtd["metadata"]["type"], "CameraConfig");

    // 5. Prove we reconstructed everything WITHOUT reading any `.slpkg`: the
    //    only bytes in a `.slpkg` are the opaque sentinel, and nothing above
    //    parsed them. Assert directly that the tarball is the opaque sentinel
    //    (i.e. the graph did not come from it).
    let slpkg_bytes = std::fs::read(tree.join("slpkg/camera/2.1.0/camera.slpkg")).unwrap();
    assert_eq!(slpkg_bytes, b"OPAQUE-SLPKG-TARBALL-BYTES-DO-NOT-READ");

    // A concrete port never collapses to the `any` wildcard.
    assert_ne!(
        camera_line.processor.outputs[0].schema,
        CatalogSchemaRef::Any
    );
}

/// GATING regression: a `-dev.N` publisher's JTDs must be fetchable by the
/// release-core ident. The writer places JTDs under the RELEASE-CORE version
/// dir because `SchemaIdent` versions are release-core by invariant and the
/// reader derives the JTD path from the ident. Mentally revert
/// `write_package_catalog` to place JTDs under the full prerelease dir and
/// `fetch_schema_type_definition` silently returns `Ok(None)` — this test
/// fails on the `expect`.
#[test]
fn prerelease_publisher_jtd_resolves_by_release_core_ident() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("packages");
    let tree = tmp.path().join("package-source");

    let widget = src.join("widget");
    write(
        &widget,
        "streamlib.yaml",
        r#"
package:
  org: tatolab
  name: widget
  version: 2.1.0-dev.3
schemas:
  WidgetConfig:
    file: schemas/widget_config.yaml
processors:
- name: Widget
  runtime: rust
  execution: reactive
  config:
    name: config
    schema: WidgetConfig
"#,
    );
    write(
        &widget,
        "schemas/widget_config.yaml",
        "metadata:\n  type: WidgetConfig\nproperties: {}\n",
    );
    emit_catalog_tree(&tree, &[widget]);

    let client = CatalogClient::new(format!("file://{}", tree.display()), None);

    // The per-package catalog is fetched by the FULL published version…
    let index = client.fetch_processor_index().unwrap();
    assert_eq!(index.len(), 1);
    assert_eq!(index[0].version.to_string(), "2.1.0-dev.3");
    let catalog = client
        .fetch_package_catalog(&pkg_ref("widget"), &index[0].version)
        .unwrap()
        .expect("per-package catalog under the full prerelease version dir");

    // …while the JTD is fetched by the release-core ident and MUST resolve.
    let cfg_ident = catalog.processors[0]
        .config
        .as_ref()
        .unwrap()
        .schema
        .clone();
    assert_eq!(cfg_ident.to_string(), "@tatolab/widget/WidgetConfig@2.1.0");
    let jtd = client
        .fetch_schema_type_definition(&cfg_ident)
        .unwrap()
        .expect("JTD must resolve for a -dev.N publisher via the release-core ident");
    assert_eq!(jtd["metadata"]["type"], "WidgetConfig");
}

/// The catalog aggregate is written at a STAGING-relative path
/// (`CATALOG_INDEX_PATH` joined onto the staging root inside
/// `emit_slpkg_and_manifest`), so it rides the same `build_and_flip` seam as
/// the release manifest. This test drives that exact seam: it writes the
/// release marker + the catalog index into the staging closure, and asserts
/// both land together after the atomic flip. It locks that the flip carries
/// the catalog aggregate atomically alongside the release — the property the
/// emit relies on. (It does NOT re-run the full `emit_slpkg_and_manifest`,
/// which needs a live workspace `cargo metadata`; the emit's own aggregate
/// write is a plain `staging.join(CATALOG_INDEX_PATH)`, exercised end-to-end
/// against `file://` by the wiring-graph test above.)
#[test]
fn catalog_aggregate_written_into_staging_rides_the_atomic_flip() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("served");

    build_and_flip(&out, |staging| {
        // Mirror the emit ordering: release marker first, aggregate last.
        let rel = staging.join("slpkg/streamlib-release/0.5.1");
        std::fs::create_dir_all(&rel)?;
        std::fs::write(
            rel.join("manifest.json"),
            b"{\"release_version\":\"0.5.1\"}",
        )?;
        let idx = staging.join(CATALOG_INDEX_PATH);
        std::fs::create_dir_all(idx.parent().unwrap())?;
        std::fs::write(&idx, "{\"package\":\"@tatolab/x\"}\n")?;
        Ok(())
    })
    .unwrap();

    // After the flip both are present together.
    assert!(out.join(CATALOG_INDEX_PATH).is_file());
    assert!(
        out.join("slpkg/streamlib-release/0.5.1/manifest.json")
            .is_file()
    );
}
