// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! End-to-end: a registry-cached crate's codegen resolves a schema
//! dependency from a (hermetic `file://`) static generic store and
//! generates bindings — the exact pipeline `build.rs` drives, minus the
//! cargo build.
//!
//! This is the faithful E2E for issue #1116's "a registry-cached
//! `streamlib-engine` codegen resolves `@tatolab/escalate` from the static registry and
//! builds." The consumer manifest declares the dep as a bare semver range
//! with **no `patch:` override** — exactly the published shape after the
//! cargo-publish path-strip. Before #1116 the resolver returned
//! `RegistryNotImplemented` here; now it lists → selects-highest-in-range →
//! fetches + extracts the schema package's `.slpkg`, and codegen emits the
//! imported type under the dep's owning-package context.
//!
//! Skipped (with a clear stderr message) when `jtd-codegen` is not on PATH.

mod common;

use std::fs;
use std::io::Write;
use std::path::Path;

use common::{collect_files, skip_unless_jtd_codegen_available};
use streamlib_idents::{RegistryConfig, ResolverOptions, resolve_with};
use streamlib_jtd_codegen::{RuntimeTarget, generate_from_resolved};
use tempfile::TempDir;

/// Build a schema-package `.slpkg` at `<root>/slpkg/<name>/<version>/<name>.slpkg`
/// laid out the way the `file://` registry transport expects (base URL is the
/// tree root; the client prepends `slpkg/`).
fn write_slpkg(tree_root: &Path, name: &str, version: &str, schema_type: &str) {
    let dir = tree_root.join("slpkg").join(name).join(version);
    fs::create_dir_all(&dir).expect("create mirror version dir");
    let archive = dir.join(format!("{name}.slpkg"));
    let mut zip = zip::ZipWriter::new(fs::File::create(&archive).expect("create slpkg"));
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("streamlib.yaml", opts).unwrap();
    zip.write_all(
        format!(
            "package:\n  org: tatolab\n  name: {name}\n  version: {version}\nschemas:\n  {schema_type}:\n    file: schemas/{schema_type}.yaml\n"
        )
        .as_bytes(),
    )
    .unwrap();
    zip.start_file(format!("schemas/{schema_type}.yaml"), opts)
        .unwrap();
    zip.write_all(
        format!(
            "metadata:\n  type: {schema_type}\noptionalProperties:\n  request_id:\n    type: string\n"
        )
        .as_bytes(),
    )
    .unwrap();
    zip.finish().unwrap();
}

#[test]
fn registry_resolved_schema_codegen_emits_imported_type() {
    let test_name = "registry_resolved_schema_codegen_emits_imported_type";
    if skip_unless_jtd_codegen_available(test_name) {
        return;
    }

    let tmp = TempDir::new().expect("temp dir");
    let mirror = tmp.path().join("mirror");

    // Three versions in the registry; the ^1.0.0 range must select 1.2.0.
    write_slpkg(&mirror, "escalate", "1.0.0", "EscalateRequest");
    write_slpkg(&mirror, "escalate", "1.2.0", "EscalateRequest");
    write_slpkg(&mirror, "escalate", "2.0.0", "EscalateRequest");

    // Consumer manifest: bare registry range + External schema import, NO
    // path patch — the published shape after the cargo-publish strip.
    let root_dir = tmp.path().join("consumer");
    fs::create_dir_all(&root_dir).unwrap();
    fs::write(
        root_dir.join("streamlib.yaml"),
        r#"
package:
  org: tatolab
  name: consumer
  version: 0.4.30
dependencies:
  "@tatolab/escalate": "^1.0.0"
schemas:
  EscalateRequest:
    package: "@tatolab/escalate"
"#,
    )
    .unwrap();

    let resolved = resolve_with(
        &root_dir,
        &ResolverOptions {
            cache_dir: Some(tmp.path().join("cache")),
            registry: Some(RegistryConfig {
                base_url: format!("file://{}", mirror.display()),
            }),
            link_checkout: None,
        },
    )
    .expect("registry resolution must succeed without a path patch");

    // Highest-in-range version selected from the registry.
    let escalate = resolved
        .packages
        .get("@tatolab/escalate")
        .expect("escalate resolved from registry");
    assert_eq!(
        escalate
            .manifest
            .package
            .as_ref()
            .unwrap()
            .version
            .to_string(),
        "1.2.0"
    );

    let output = TempDir::new().expect("output temp dir");
    generate_from_resolved(&resolved, RuntimeTarget::Rust, output.path())
        .expect("codegen from registry-resolved packages");

    let files: Vec<String> = collect_files(output.path(), &[])
        .into_iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    assert!(
        files
            .iter()
            .any(|f| f.contains("tatolab__escalate") && f.ends_with("escalate_request.rs")),
        "expected escalate_request.rs under tatolab__escalate/ (registry-resolved External owner context); got files: {files:?}"
    );
}

/// The faithful standalone-consumption E2E for issue #1307: a consumer
/// manifest carries the EXACT shape `streamlib-engine`'s `streamlib.yaml`
/// ships — a bare registry range in `dependencies:` PLUS a monorepo-relative
/// `patch: { path }` dev override PLUS an `External` schema import. For a
/// standalone consumer the patch target does not exist, so the resolver must
/// fall back to resolving the declared version from the registry and codegen
/// must still emit the imported type. This is the #1276 docker container's
/// runtime path minus the cargo build. Before the fix, the absent dev path
/// patch made the resolve fail with `PathDependencyNotFound`.
#[test]
fn registry_resolved_codegen_falls_back_when_dev_path_patch_absent() {
    let test_name = "registry_resolved_codegen_falls_back_when_dev_path_patch_absent";
    if skip_unless_jtd_codegen_available(test_name) {
        return;
    }

    let tmp = TempDir::new().expect("temp dir");
    let mirror = tmp.path().join("mirror");
    write_slpkg(&mirror, "escalate", "1.0.0", "EscalateRequest");
    write_slpkg(&mirror, "escalate", "1.2.0", "EscalateRequest");

    // Consumer manifest with the engine's exact shape: bare range dep +
    // dev-time path patch (whose target is absent for a standalone consumer)
    // + External schema import.
    let root_dir = tmp.path().join("consumer");
    fs::create_dir_all(&root_dir).unwrap();
    fs::write(
        root_dir.join("streamlib.yaml"),
        r#"
package:
  org: tatolab
  name: consumer
  version: 0.6.0
dependencies:
  "@tatolab/escalate": "^1.0.0"
patch:
  "@tatolab/escalate":
    path: ../packages/escalate
schemas:
  EscalateRequest:
    package: "@tatolab/escalate"
"#,
    )
    .unwrap();

    let resolved = resolve_with(
        &root_dir,
        &ResolverOptions {
            cache_dir: Some(tmp.path().join("cache")),
            registry: Some(RegistryConfig {
                base_url: format!("file://{}", mirror.display()),
            }),
            link_checkout: None,
        },
    )
    .expect("absent dev path patch must fall back to registry resolution");

    let escalate = resolved
        .packages
        .get("@tatolab/escalate")
        .expect("escalate resolved from registry despite the path patch");
    assert_eq!(
        escalate
            .manifest
            .package
            .as_ref()
            .unwrap()
            .version
            .to_string(),
        "1.2.0"
    );

    let output = TempDir::new().expect("output temp dir");
    generate_from_resolved(&resolved, RuntimeTarget::Rust, output.path())
        .expect("codegen from registry-resolved packages");

    let files: Vec<String> = collect_files(output.path(), &[])
        .into_iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    assert!(
        files
            .iter()
            .any(|f| f.contains("tatolab__escalate") && f.ends_with("escalate_request.rs")),
        "expected escalate_request.rs under tatolab__escalate/ after path-patch fallback; got files: {files:?}"
    );
}
