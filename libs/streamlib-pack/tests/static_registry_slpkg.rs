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
    crates_missing_from_release, Org, Package, PackageRef, RegistryClient, RegistryConfig,
    ReleaseManifest, ReleaseManifestMember, SemVer, SemVerRange,
};

fn file_config(dir: &std::path::Path) -> RegistryConfig {
    RegistryConfig {
        base_url: format!("file://{}", dir.display()),
        token: None,
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
    let slpkg = root.join("slpkg");
    std::fs::create_dir_all(&slpkg).unwrap();
    let cfg = file_config(&slpkg);
    let client = RegistryClient::new(&cfg);

    client
        .upload_slpkg(&pkg_ref("camera"), SemVer::new(1, 0, 0), b"camera-slpkg-bytes")
        .unwrap();
    client
        .upload_slpkg(&pkg_ref("display"), SemVer::new(1, 0, 0), b"display-slpkg-bytes")
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
    client.upload_release_manifest("tatolab", &manifest).unwrap();
    slpkg
}

#[test]
fn complete_tree_resolves_offline_and_completeness_passes() {
    let root = tempfile::tempdir().unwrap();
    let slpkg = emit_complete_tree(root.path());
    let cfg = file_config(&slpkg);
    let client = RegistryClient::new(&cfg);

    // The consumer lists releases + fetches the manifest with NO daemon, NO token.
    assert_eq!(client.list_release_versions("tatolab").unwrap(), vec![SemVer::new(0, 5, 1)]);
    let manifest = client.fetch_release_manifest("tatolab", "0.5.1").unwrap().unwrap();

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
    let (bytes, url) = client.download_slpkg(&pkg_ref("camera"), SemVer::new(1, 0, 0)).unwrap();
    assert_eq!(bytes, b"camera-slpkg-bytes");
    assert!(url.ends_with("/generic/camera/1.0.0/camera.slpkg"), "url: {url}");
}

#[test]
fn truncated_release_manifest_is_rejected_by_completeness_check() {
    let root = tempfile::tempdir().unwrap();
    let slpkg = emit_complete_tree(root.path());
    let cfg = file_config(&slpkg);
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

    let manifest = client.fetch_release_manifest("tatolab", "0.5.1").unwrap().unwrap();
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
    let slpkg = emit_complete_tree(root.path());
    let cfg = file_config(&slpkg);
    let client = RegistryClient::new(&cfg);

    // Truncate the TREE: remove display's .slpkg after the manifest claimed it.
    std::fs::remove_dir_all(slpkg.join("display")).unwrap();

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
    assert!(client.list_versions(&pkg_ref("display")).unwrap().is_empty());
}
