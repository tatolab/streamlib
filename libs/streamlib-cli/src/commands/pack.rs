// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::path::Path;

use anyhow::{Context, Result};
use streamlib_pack::{
    assemble_artifact, AssembleOptions, AssembleTarget, CargoProfile, PackEventSink, PackStream,
    PathDepPolicy,
};

/// Pack a processor package into a `.slpkg` bundle.
///
/// Thin wrapper over [`streamlib_pack::assemble_artifact`] — the same
/// assembly routine the runtime build orchestrator uses, targeting a
/// compressed `.slpkg` with publishing semantics (path-flavor `patch:`
/// entries rejected) and a release Rust build.
///
/// Rust processors are compiled (`cargo build --release` → cdylib);
/// Python ships as full source (no wheel — the runtime runs it from
/// source); Deno ships its entrypoint. Pass `no_build = true` to require
/// a pre-built `lib/<triple>/` cdylib rather than invoking cargo.
pub fn pack(package_dir: &Path, output: Option<&Path>, no_build: bool) -> Result<()> {
    // Default output filename is `<name>-<version>.slpkg` in the package
    // dir; read the manifest to resolve it before assembly runs.
    let output_path = match output {
        Some(p) => p.to_path_buf(),
        None => {
            let config = streamlib_cargo_build::read_minimal_project_config(package_dir)
                .context("Failed to read streamlib.yaml")?
                .ok_or_else(|| {
                    anyhow::anyhow!("no streamlib.yaml at {}", package_dir.display())
                })?;
            let package = config.package.as_ref().ok_or_else(|| {
                anyhow::anyhow!("streamlib.yaml missing [package] section")
            })?;
            package_dir.join(format!("{}-{}.slpkg", package.name.as_str(), package.version))
        }
    };

    let outcome = assemble_artifact(
        package_dir,
        &AssembleTarget::Slpkg(output_path.clone()),
        &AssembleOptions {
            no_build,
            profile: CargoProfile::Release,
            path_deps: PathDepPolicy::RejectPathPatches,
        },
        &StdoutPackSink,
    )?;

    println!("Created: {}", output_path.display());
    println!(
        "  Package: {} v{}",
        outcome.package_name, outcome.package_version
    );
    if outcome.schemas > 0 {
        println!("  Schemas: {}", outcome.schemas);
    }
    if outcome.processors > 0 {
        println!("  Processors: {}", outcome.processors);
    }
    if outcome.python_wheels > 0 {
        println!("  Python wheels: {}", outcome.python_wheels);
    }
    Ok(())
}

/// Forwards assembly build-tool output to the terminal so a cold
/// `cargo build` / `uv build` doesn't appear hung.
struct StdoutPackSink;

impl PackEventSink for StdoutPackSink {
    fn started(&self, language: &str) {
        println!("  Building {language}…");
    }
    fn line(&self, _stream: PackStream, line: &str) {
        println!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use streamlib_cargo_build::{host_dylib_extension, host_target_triple};
    use tempfile::tempdir;

    fn write_yaml(dir: &Path, body: &str) {
        std::fs::write(dir.join("streamlib.yaml"), body).unwrap();
    }

    const RUST_PLUGIN_YAML: &str = r#"
package:
  org: tatolab
  name: test-plugin
  version: 0.1.0
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
"#;

    #[test]
    fn pack_with_populated_lib_writes_slpkg_with_triple_keyed_cdylib() {
        // End-to-end smoke for the CLI wrapper: a pre-populated
        // lib/<triple>/ packs into a .slpkg carrying the cdylib at the
        // triple-keyed path, without invoking cargo (the tempdir is
        // outside any workspace, so a stray cargo build would fail).
        let dir = tempdir().unwrap();
        write_yaml(dir.path(), RUST_PLUGIN_YAML);
        let triple_dir = dir.path().join("lib").join(host_target_triple());
        std::fs::create_dir_all(&triple_dir).unwrap();
        let dylib_name = format!("libtest_plugin.{}", host_dylib_extension());
        std::fs::write(triple_dir.join(&dylib_name), b"fake-dylib-bytes").unwrap();

        let output = dir.path().join("out.slpkg");
        pack(dir.path(), Some(&output), /* no_build */ false)
            .expect("populated lib/<triple>/ must pack without invoking cargo");

        assert!(output.exists(), "expected slpkg at {}", output.display());
        let zip_bytes = std::fs::read(&output).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).unwrap();
        let entry_name = format!("lib/{}/{}", host_target_triple(), dylib_name);
        zip.by_name(&entry_name)
            .unwrap_or_else(|_| panic!("slpkg missing {entry_name} entry"));
    }

    #[test]
    fn pack_rejects_path_flavor_patch_entries() {
        // The CLI uses PathDepPolicy::RejectPathPatches, so a path-flavor
        // patch must fail the pack. Mentally reverting that policy choice
        // would let pack ship a yaml that breaks at customer install time.
        let dir = tempdir().unwrap();
        write_yaml(
            dir.path(),
            r#"
package:
  org: tatolab
  name: foo
  version: 1.0.0
schemas:
  T:
    file: schemas/t.yaml
dependencies:
  "@tatolab/core": "^1.0.0"
patch:
  "@tatolab/core":
    path: ../../../packages/core
"#,
        );
        std::fs::create_dir(dir.path().join("schemas")).unwrap();
        std::fs::write(
            dir.path().join("schemas/t.yaml"),
            "metadata:\n  type: T\n  max_payload_bytes: 16\n",
        )
        .unwrap();
        let err = pack(dir.path(), Some(&dir.path().join("o.slpkg")), false)
            .expect_err("path-flavor patch must be rejected by the CLI pack");
        let msg = format!("{err}");
        assert!(
            msg.contains("@tatolab/core") && (msg.contains("path-flavor") || msg.contains("not publishable")),
            "error must explain the rejected path patch, got: {msg}"
        );
    }
}
