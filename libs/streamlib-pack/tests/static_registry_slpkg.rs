// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Integration: a `.slpkg` static tree emitted over `file://` resolves a
//! consumer offline, and a TRUNCATED tree (a member missing from the release
//! or its `.slpkg` removed) is REJECTED by the consumer-side completeness
//! check — not silently half-resolved. This is the daemon-free negative gate
//! from the static-registry issue, exercised against the exact tree layout
//! `emit_static_registry` produces (`<out>/slpkg/<pkg>/<ver>/<pkg>.slpkg` +
//! `<out>/slpkg/streamlib-release/<V>/manifest.json`).

use streamlib_idents::{
    Org, Package, PackageRef, RegistryClient, RegistryConfig, ReleaseManifest,
    ReleaseManifestMember, SemVer, SemVerRange, crates_missing_from_release,
};

/// A `file://` registry config rooted at the tree root (the dir holding
/// `slpkg/`) — the client prepends `slpkg/` itself.
fn file_config(tree_root: &std::path::Path) -> RegistryConfig {
    RegistryConfig {
        base_url: format!("file://{}", tree_root.display()),
    }
}

fn pkg_ref(name: &str) -> PackageRef {
    PackageRef::new(Org::new("tatolab").unwrap(), Package::new(name).unwrap())
}

fn req(name: &str, range: &str) -> (String, SemVerRange) {
    (name.to_string(), SemVerRange::from_str(range).unwrap())
}

/// Build a COMPLETE static slpkg tree: two packages + a release manifest that
/// lists the crate closure and both packages. Returns the slpkg dir.
fn emit_complete_tree(root: &std::path::Path) -> std::path::PathBuf {
    let cfg = file_config(root);
    let client = RegistryClient::new(&cfg);

    client
        .upload_slpkg(
            &pkg_ref("camera"),
            SemVer::new(1, 0, 0),
            b"camera-slpkg-bytes",
        )
        .unwrap();
    client
        .upload_slpkg(
            &pkg_ref("display"),
            SemVer::new(1, 0, 0),
            b"display-slpkg-bytes",
        )
        .unwrap();

    let mut manifest = ReleaseManifest::new(
        "0.5.1",
        vec![
            ReleaseManifestMember::new("streamlib-plugin-sdk", "0.5.1"),
            ReleaseManifestMember::new("vulkan-jpeg", "0.5.1"),
        ],
    );
    manifest.python = Some("0.5.1".to_string());
    manifest.packages = vec![
        ReleaseManifestMember::new("@tatolab/camera", "1.0.0"),
        ReleaseManifestMember::new("@tatolab/display", "1.0.0"),
    ];
    // Written LAST — the completion marker.
    client
        .upload_release_manifest("tatolab", &manifest)
        .unwrap();
    root.to_path_buf()
}

#[test]
fn complete_tree_resolves_offline_and_completeness_passes() {
    let root = tempfile::tempdir().unwrap();
    let tree = emit_complete_tree(root.path());
    let cfg = file_config(&tree);
    let client = RegistryClient::new(&cfg);

    // The consumer lists releases + fetches the manifest with NO daemon, NO token.
    assert_eq!(
        client.list_release_versions("tatolab").unwrap(),
        vec![SemVer::new(0, 5, 1)]
    );
    let manifest = client
        .fetch_release_manifest("tatolab", "0.5.1")
        .unwrap()
        .unwrap();

    // Consumer's direct crate pins are all satisfied → complete release.
    let pins = vec![
        req("streamlib-plugin-sdk", "^0.5.0"),
        req("vulkan-jpeg", "^0.5.0"),
    ];
    assert!(
        crates_missing_from_release(&manifest, &pins).is_empty(),
        "a complete release must satisfy every direct pin"
    );

    // Both package .slpkgs download from the file:// store.
    let (bytes, url) = client
        .download_slpkg(&pkg_ref("camera"), SemVer::new(1, 0, 0))
        .unwrap();
    assert_eq!(bytes, b"camera-slpkg-bytes");
    assert!(
        url.ends_with("/slpkg/camera/1.0.0/camera.slpkg"),
        "url: {url}"
    );
}

#[test]
fn truncated_release_manifest_is_rejected_by_completeness_check() {
    let root = tempfile::tempdir().unwrap();
    let tree = emit_complete_tree(root.path());
    let cfg = file_config(&tree);
    let client = RegistryClient::new(&cfg);

    // Simulate a partial release: overwrite the manifest with one that OMITS
    // `vulkan-jpeg` (the historical closure foot-gun). This is what a
    // mid-publish / truncated tree looks like to a consumer.
    let mut partial = ReleaseManifest::new(
        "0.5.1",
        vec![ReleaseManifestMember::new("streamlib-plugin-sdk", "0.5.1")],
    );
    partial.packages = vec![ReleaseManifestMember::new("@tatolab/camera", "1.0.0")];
    client.upload_release_manifest("tatolab", &partial).unwrap();

    let manifest = client
        .fetch_release_manifest("tatolab", "0.5.1")
        .unwrap()
        .unwrap();
    let pins = vec![
        req("streamlib-plugin-sdk", "^0.5.0"),
        req("vulkan-jpeg", "^0.5.0"),
    ];
    let missing = crates_missing_from_release(&manifest, &pins);
    assert_eq!(
        missing,
        vec!["vulkan-jpeg@^0.5.0".to_string()],
        "the completeness check must NAME the truncated member, not silently pass"
    );
}

#[test]
fn removed_slpkg_from_tree_is_rejected_at_download() {
    let root = tempfile::tempdir().unwrap();
    let tree = emit_complete_tree(root.path());
    let cfg = file_config(&tree);
    let client = RegistryClient::new(&cfg);

    // Truncate the TREE: remove display's .slpkg after the manifest claimed it.
    std::fs::remove_dir_all(tree.join("slpkg").join("display")).unwrap();

    // The manifest still lists it (stale/partial), but the artifact is gone —
    // the consumer's download fails loudly rather than half-resolving.
    let err = client
        .download_slpkg(&pkg_ref("display"), SemVer::new(1, 0, 0))
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("display"),
        "download of a truncated member must fail naming it; got: {msg}"
    );
    // list_versions for the removed package is now empty (no version dir).
    assert!(
        client
            .list_versions(&pkg_ref("display"))
            .unwrap()
            .is_empty()
    );
}

/// The truncation gate proven against the manifest `emit_static_registry`
/// ACTUALLY writes (not a hand-built one): emit a minimal fake workspace's
/// slpkg ecosystem through the real emit path, then truncate the emitted
/// tree and assert the consumer-side checks reject it.
#[test]
fn emitted_tree_truncation_is_rejected_by_consumer_checks() {
    use streamlib_pack::static_registry::{EmitEcosystems, EmitOptions, emit_static_registry};

    // Minimal fake workspace: empty cargo workspace + one schemas-only
    // package (no cargo build at assemble time).
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("ws");
    std::fs::create_dir_all(ws.join("packages/demopkg/schemas")).unwrap();
    // cargo metadata rejects an empty virtual workspace — give it one stub
    // member whose name is outside the release closure (not `streamlib*`).
    std::fs::write(
        ws.join("Cargo.toml"),
        "[workspace]\nmembers = [\"stub\"]\n\n[workspace.package]\nversion = \"0.9.0\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(ws.join("stub/src")).unwrap();
    std::fs::write(
        ws.join("stub/Cargo.toml"),
        "[package]\nname = \"stub\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    std::fs::write(ws.join("stub/src/lib.rs"), "").unwrap();
    std::fs::write(
        ws.join("packages/demopkg/streamlib.yaml"),
        "package:\n  org: tatolab\n  name: demopkg\n  version: 1.0.0\n\
         \nschemas:\n  DemoFrame:\n    file: schemas/demo_frame.yaml\n",
    )
    .unwrap();
    std::fs::write(
        ws.join("packages/demopkg/schemas/demo_frame.yaml"),
        "metadata:\n  type: DemoFrame\n  description: \"demo\"\n\
         properties:\n  value:\n    type: uint32\n",
    )
    .unwrap();

    let out = root.path().join("registry");
    emit_static_registry(&EmitOptions {
        workspace_root: ws.clone(),
        out: out.clone(),
        base_url: "http://127.0.0.1:9".into(), // unused by the slpkg ecosystem
        dev: None,
        ecosystems: EmitEcosystems {
            cargo_fork: false,
            cargo_closure: false,
            pypi: false,
            npm: false,
            slpkg: true,
        },
    })
    .expect("slpkg-only emit against the fake workspace must succeed");

    // The REAL emitted manifest is fetchable + lists the package.
    let cfg = file_config(&out);
    let client = RegistryClient::new(&cfg);
    assert_eq!(
        client.list_release_versions("tatolab").unwrap(),
        vec![SemVer::new(0, 9, 0)]
    );
    let manifest = client
        .fetch_release_manifest("tatolab", "0.9.0")
        .unwrap()
        .unwrap();
    assert!(
        manifest
            .packages
            .iter()
            .any(|m| m.name == "@tatolab/demopkg" && m.version == "1.0.0"),
        "the emitted manifest must list the assembled package; got {:?}",
        manifest.packages
    );
    // And the .slpkg artifact itself downloads.
    let (bytes, _) = client
        .download_slpkg(&pkg_ref("demopkg"), SemVer::new(1, 0, 0))
        .unwrap();
    assert!(!bytes.is_empty());

    // TRUNCATE the emitted tree: remove the package's artifacts post-flip.
    std::fs::remove_dir_all(out.join("slpkg").join("demopkg")).unwrap();

    // The manifest still claims the member, but the tree can't serve it —
    // the consumer fails loudly at download instead of half-resolving.
    let manifest = client
        .fetch_release_manifest("tatolab", "0.9.0")
        .unwrap()
        .unwrap();
    assert!(
        manifest
            .packages
            .iter()
            .any(|m| m.name == "@tatolab/demopkg")
    );
    let err = client
        .download_slpkg(&pkg_ref("demopkg"), SemVer::new(1, 0, 0))
        .unwrap_err();
    assert!(
        err.to_string().contains("demopkg"),
        "truncated member must be named: {err}"
    );
    assert!(
        client
            .list_versions(&pkg_ref("demopkg"))
            .unwrap()
            .is_empty()
    );
}
