// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Tree-shaking integration test for `generate_from_resolved` (#767, Stage 4).
//!
//! Sets up an in-tempdir manifest pair: a root with a Local schema (`Foo`)
//! and an External entry (`BarFromDep`) imported by bare name from a
//! declared dep. The dep ALSO declares a second Local schema
//! (`NotImported`) that the root never references.
//!
//! After running codegen against the root, the output dir must contain
//! `foo.rs` and `bar_from_dep.rs` but NOT `not_imported.rs` — proving the
//! tree-shaker honors the root's declared `schemas:` map and does not
//! emit every schema reachable through the dep graph.
//!
//! Skipped (with a clear stderr message) when `jtd-codegen` v0.4.1 is not
//! on PATH — there is no value in running the rest of the test framework
//! against a missing binary.

mod common;

use std::fs;
use std::path::Path;

use common::{collect_files, skip_unless_jtd_codegen_available};
use streamlib_jtd_codegen::{GenerateOptions, RuntimeTarget, generate};
use tempfile::TempDir;

const ROOT_MANIFEST: &str = r#"
package:
  org: example
  name: root
  version: 1.0.0
dependencies:
  "@example/dep":
    path: ../dep
schemas:
  Foo:
    file: schemas/foo.yaml
  BarFromDep:
    package: "@example/dep"
"#;

const DEP_MANIFEST: &str = r#"
package:
  org: example
  name: dep
  version: 1.0.0
schemas:
  BarFromDep:
    file: schemas/bar.yaml
  NotImported:
    file: schemas/not_imported.yaml
"#;

const FOO_SCHEMA: &str = r#"
metadata:
  type: Foo
optionalProperties:
  field_a:
    type: string
"#;

const BAR_SCHEMA: &str = r#"
metadata:
  type: BarFromDep
optionalProperties:
  field_b:
    type: int32
"#;

const NOT_IMPORTED_SCHEMA: &str = r#"
metadata:
  type: NotImported
optionalProperties:
  field_c:
    type: boolean
"#;

#[test]
fn tree_shake_emits_only_root_declared_schemas() {
    let test_name = "tree_shake_emits_only_root_declared_schemas";
    if skip_unless_jtd_codegen_available(test_name) {
        return;
    }

    let tmp = TempDir::new().expect("temp dir");
    let root_dir = tmp.path().join("root");
    let dep_dir = tmp.path().join("dep");

    write_project(
        &root_dir,
        ROOT_MANIFEST,
        &[("schemas/foo.yaml", FOO_SCHEMA)],
    );
    write_project(
        &dep_dir,
        DEP_MANIFEST,
        &[
            ("schemas/bar.yaml", BAR_SCHEMA),
            ("schemas/not_imported.yaml", NOT_IMPORTED_SCHEMA),
        ],
    );

    let output = TempDir::new().expect("output temp dir");

    generate(GenerateOptions {
        runtime: RuntimeTarget::Rust,
        output: output.path().to_path_buf(),
        project_dir: Some(root_dir.clone()),
        schema_file: None,
        schema_dir: None,
        workspace_root: tmp.path().to_path_buf(),
        write_lockfile: false,
    })
    .expect("generate root codegen");

    let files: Vec<String> = collect_files(output.path(), &[])
        .into_iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    let has_file = |needle: &str| files.iter().any(|f| f.ends_with(needle));

    assert!(
        has_file("foo.rs"),
        "expected foo.rs (root-owned Local entry) in output; got files: {:?}",
        files
    );
    assert!(
        has_file("bar_from_dep.rs"),
        "expected bar_from_dep.rs (External entry resolved via dep walk) in output; got files: {:?}",
        files
    );
    assert!(
        !has_file("not_imported.rs"),
        "tree-shaking failure: not_imported.rs leaked into output even though root never declares it; got files: {:?}",
        files
    );

    // Sanity: the dep's BarFromDep schema lands under the dep's
    // <org>__<package>/ subdir (example__dep/), not under the root's. This
    // locks in that External entries carry the OWNING package's context,
    // not the root's.
    assert!(
        files
            .iter()
            .any(|f| f.contains("example__dep") && f.ends_with("bar_from_dep.rs")),
        "expected bar_from_dep.rs under example__dep/ subdir (External owner context); got files: {:?}",
        files
    );
    assert!(
        files
            .iter()
            .any(|f| f.contains("example__root") && f.ends_with("foo.rs")),
        "expected foo.rs under example__root/ subdir (root owner context); got files: {:?}",
        files
    );
}

fn write_project(dir: &Path, manifest: &str, schemas: &[(&str, &str)]) {
    fs::create_dir_all(dir).expect("create project dir");
    fs::write(dir.join("streamlib.yaml"), manifest).expect("write streamlib.yaml");
    for (rel, body) in schemas {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create schema parent");
        }
        fs::write(&path, body).expect("write schema");
    }
}
