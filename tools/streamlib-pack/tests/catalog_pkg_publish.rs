// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration: the incremental `streamlib pkg publish` catalog flow. Where
//! `catalog_publish_query.rs` drives the WHOLE-tree emit's aggregate (write the
//! full index once), this drives the SINGLE-package publish path the CLI wires:
//! `build_package_catalog` → `write_package_catalog` → `merge_catalog_index_lines`,
//! one package at a time, then reads it all back through `CatalogClient`.
//!
//! It exercises exactly the pack functions `streamlib pkg publish` calls after
//! `upload_slpkg`, so a registry populated purely by `pkg publish` is proven
//! catalog-identical to one built by the whole-tree emit — the property the
//! `streamlib add` discovery summary depends on universally.

use std::path::{Path, PathBuf};

use streamlib_idents::CatalogClient;
use streamlib_idents::{
    CATALOG_INDEX_PATH, CatalogRuntime, CatalogSchemaRef, Org, Package, PackageRef, RegistryClient,
    RegistryConfig, SchemaIdent, SemVer, TypeName, parse_catalog_index_ndjson,
};
use streamlib_pack::catalog::{SiblingVersions, build_package_catalog, build_sibling_versions};
use streamlib_pack::static_registry::{merge_catalog_index_lines, write_package_catalog};

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn pkg_ref(name: &str) -> PackageRef {
    PackageRef::new(Org::new("tatolab").unwrap(), Package::new(name).unwrap())
}

fn ident(pkg: &str, ty: &str, v: SemVer) -> SchemaIdent {
    SchemaIdent::new(
        Org::new("tatolab").unwrap(),
        Package::new(pkg).unwrap(),
        TypeName::new(ty).unwrap(),
        v,
    )
}

/// `@tatolab/core@1.4.0` (owns VideoFrame) + `@tatolab/camera@2.1.0` (imports
/// VideoFrame from core, owns CameraConfig, declares two processors).
fn author_core_and_camera(src: &Path) -> (PathBuf, PathBuf) {
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
    write(&camera, "streamlib.yaml", CAMERA_MANIFEST_TWO_PROCESSORS);
    write(
        &camera,
        "schemas/camera_config.yaml",
        "metadata:\n  type: CameraConfig\n  description: cfg\noptionalProperties:\n  device_id:\n    type: string\n",
    );
    (core, camera)
}

const CAMERA_MANIFEST_TWO_PROCESSORS: &str = r#"
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
    read_mode: skip_to_latest
"#;

/// Camera manifest re-authored with ONLY the `Camera` processor (Sink removed) —
/// used to prove a republish drops the stale processor's aggregate line.
const CAMERA_MANIFEST_ONE_PROCESSOR: &str = r#"
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
"#;

/// Mirror `streamlib pkg publish`'s post-upload steps for one package: upload a
/// dummy `.slpkg` (so the store + version index exist, as a real publish would),
/// then write the per-package catalog + owned JTDs and merge the package's lines
/// into the tree-wide aggregate. `siblings` is the resolution universe the CLI
/// builds from the package's neighbors.
fn publish_package(tree_root: &Path, pkg_dir: &Path, siblings: &SiblingVersions) {
    let arts = build_package_catalog(pkg_dir, siblings)
        .unwrap_or_else(|e| panic!("build catalog for {}: {e}", pkg_dir.display()));
    let cfg = RegistryConfig {
        base_url: format!("file://{}", tree_root.display()),
    };
    // A real publish writes the `.slpkg` first; the catalog query path never
    // reads it, so opaque bytes suffice.
    RegistryClient::new(&cfg)
        .upload_slpkg(
            &arts.catalog.package,
            arts.catalog.version,
            b"OPAQUE-SLPKG-DO-NOT-READ",
        )
        .unwrap();
    write_package_catalog(&tree_root.join("slpkg"), &arts).unwrap();
    merge_catalog_index_lines(
        tree_root,
        &arts.catalog.package,
        &arts.catalog.version,
        &arts.index_lines,
    )
    .unwrap();
}

/// The exit-criteria test: publish core then camera one at a time into a scratch
/// tree, and prove the catalog is fetchable exactly as an emit-built tree's is —
/// `.catalog.json` at the full-version path, `fetch_package_catalog` returns the
/// resolved processors/ports, the aggregate carries the processor lines, the
/// owned JTDs land (core's at core's dir, camera's at camera's), and a republish
/// of the same version does not duplicate the aggregate lines.
#[test]
fn pkg_publish_emits_fetchable_catalog_and_republish_does_not_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("packages");
    let tree = tmp.path().join("registry");
    let (core, camera) = author_core_and_camera(&src);

    // Resolution universe = every local package (what the CLI derives from the
    // package's parent dir). Publish in dependency order, one package at a time.
    let siblings = build_sibling_versions(&[core.clone(), camera.clone()]).unwrap();
    publish_package(&tree, &core, &siblings);
    publish_package(&tree, &camera, &siblings);

    // 1. The per-package catalog exists on disk at the FULL-version path.
    let camera_catalog_path = tree.join("slpkg/camera/2.1.0/camera.catalog.json");
    assert!(
        camera_catalog_path.is_file(),
        "expected {} to exist",
        camera_catalog_path.display()
    );

    let client = CatalogClient::new(format!("file://{}", tree.display()), None);

    // 2. `fetch_package_catalog` returns Some with the resolved processors/ports
    //    (NOT None — the degraded "no metadata" state this feature closes).
    let cam = client
        .fetch_package_catalog(&pkg_ref("camera"), &SemVer::new(2, 1, 0))
        .unwrap()
        .expect("camera catalog must be fetchable from a pkg-publish-only tree");
    assert_eq!(cam.processors.len(), 2);
    let camera_proc = &cam.processors[0];
    assert_eq!(camera_proc.name, "Camera");
    assert_eq!(camera_proc.runtime, CatalogRuntime::Rust);
    // Config ref is Local → camera's own version.
    assert_eq!(
        camera_proc.config.as_ref().unwrap().schema.to_string(),
        "@tatolab/camera/CameraConfig@2.1.0"
    );
    // Output port carries the EXTERNAL dep's version (core's 1.4.0), not the
    // importer's (2.1.0) — external-ref resolution worked at publish time.
    assert_eq!(
        camera_proc.outputs[0].schema,
        CatalogSchemaRef::Schema(ident("core", "VideoFrame", SemVer::new(1, 4, 0)))
    );
    let sink = &cam.processors[1];
    assert_eq!(sink.runtime, CatalogRuntime::Python);
    assert_eq!(sink.entrypoint.as_deref(), Some("src.sink:Sink"));
    assert_eq!(sink.inputs[0].read_mode.as_deref(), Some("skip_to_latest"));

    // 3. The aggregate carries one line per processor; core (schema-only)
    //    contributes none.
    let index = client.fetch_processor_index().unwrap();
    let names: Vec<(String, String)> = index
        .iter()
        .map(|l| (l.package.to_string(), l.processor.name.clone()))
        .collect();
    assert!(names.contains(&("@tatolab/camera".into(), "Camera".into())));
    assert!(names.contains(&("@tatolab/camera".into(), "Sink".into())));
    assert!(
        !names.iter().any(|(p, _)| p == "@tatolab/core"),
        "schema-only core contributes no aggregate lines"
    );
    assert_eq!(index.len(), 2, "exactly two processors published");

    // 4. Owned JTDs resolve from the OWNING package's dir: core owns VideoFrame,
    //    camera owns CameraConfig.
    let vf = client
        .fetch_schema_type_definition(&ident("core", "VideoFrame", SemVer::new(1, 4, 0)))
        .unwrap()
        .expect("core's VideoFrame JTD");
    assert_eq!(vf["metadata"]["type"], "VideoFrame");
    let cc = client
        .fetch_schema_type_definition(&ident("camera", "CameraConfig", SemVer::new(2, 1, 0)))
        .unwrap()
        .expect("camera's CameraConfig JTD");
    assert_eq!(cc["metadata"]["type"], "CameraConfig");

    // 5. Republish camera at the SAME version — the aggregate must NOT gain
    //    duplicate lines. (Mentally revert the `retain` dedup in
    //    `merge_catalog_index_lines` and this jumps from 2 to 4.)
    publish_package(&tree, &camera, &siblings);
    let after = client.fetch_processor_index().unwrap();
    assert_eq!(
        after.len(),
        2,
        "republish must not duplicate aggregate lines"
    );
    // The on-disk aggregate itself has no dupes (not just the parsed view).
    let raw = std::fs::read(tree.join(CATALOG_INDEX_PATH)).unwrap();
    assert_eq!(parse_catalog_index_ndjson(&raw).len(), 2);
}

/// A republish that DROPS a processor must drop that processor's stale aggregate
/// line — the dedup key is `(package, version)`, not the whole line. (Mentally
/// revert to a full-line-equality dedup and Sink's line survives, failing the
/// `== 1` assertion.)
#[test]
fn republish_with_fewer_processors_drops_the_stale_aggregate_line() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("packages");
    let tree = tmp.path().join("registry");
    let (core, camera) = author_core_and_camera(&src);
    let siblings = build_sibling_versions(&[core.clone(), camera.clone()]).unwrap();

    publish_package(&tree, &core, &siblings);
    publish_package(&tree, &camera, &siblings);
    let client = CatalogClient::new(format!("file://{}", tree.display()), None);
    assert_eq!(client.fetch_processor_index().unwrap().len(), 2);

    // Re-author camera with only the Camera processor, rebuild the sibling
    // universe from the new manifest, and republish.
    write(&camera, "streamlib.yaml", CAMERA_MANIFEST_ONE_PROCESSOR);
    let siblings = build_sibling_versions(&[core, camera.clone()]).unwrap();
    publish_package(&tree, &camera, &siblings);

    let index = client.fetch_processor_index().unwrap();
    assert_eq!(index.len(), 1, "the dropped Sink processor's line is gone");
    assert_eq!(index[0].processor.name, "Camera");
}

/// The publish path honors the version-key asymmetry a `-dev.N` release needs:
/// the per-package catalog is keyed by the FULL prerelease version, while the
/// owned schema JTD is keyed by the RELEASE-CORE version (schema idents are
/// release-core by invariant). A publisher whose JTD sat under the full
/// prerelease dir would be silently unfetchable by the release-core ident.
#[test]
fn prerelease_publish_keys_catalog_by_full_but_jtd_by_release_core() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("packages");
    let tree = tmp.path().join("registry");

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
    let siblings = build_sibling_versions(&[widget.clone()]).unwrap();
    publish_package(&tree, &widget, &siblings);

    let client = CatalogClient::new(format!("file://{}", tree.display()), None);

    // Per-package catalog under the FULL prerelease version dir.
    assert!(
        tree.join("slpkg/widget/2.1.0-dev.3/widget.catalog.json")
            .is_file()
    );
    let full_ver: SemVer = "2.1.0-dev.3".parse().unwrap();
    let catalog = client
        .fetch_package_catalog(&pkg_ref("widget"), &full_ver)
        .unwrap()
        .expect("catalog fetchable by the FULL prerelease version");
    let cfg_ident = catalog.processors[0]
        .config
        .as_ref()
        .unwrap()
        .schema
        .clone();
    // The config schema ident is release-core (prerelease stripped).
    assert_eq!(cfg_ident.to_string(), "@tatolab/widget/WidgetConfig@2.1.0");

    // JTD under the RELEASE-CORE dir, fetchable by the release-core ident.
    assert!(
        tree.join("slpkg/widget/2.1.0/schemas/WidgetConfig.jtd.json")
            .is_file()
    );
    let jtd = client
        .fetch_schema_type_definition(&cfg_ident)
        .unwrap()
        .expect("JTD fetchable by the release-core ident for a -dev.N publisher");
    assert_eq!(jtd["metadata"]["type"], "WidgetConfig");
}
