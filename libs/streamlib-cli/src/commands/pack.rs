// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use zip::write::FileOptions;
use zip::ZipWriter;

use streamlib::engine_internal::core::ProjectConfig;
use streamlib_cargo_build::{
    collect_host_dylibs_in_lib, host_dylib_extension, host_target_triple,
    read_cargo_package_name, run_cargo_build_release,
};
use streamlib_idents::Manifest;

/// Pack a processor package into a .slpkg bundle.
///
/// When the package declares Rust runtime processors and `<dir>/lib/`
/// has no host-OS dylib, `pack` invokes `cargo build --release -p <name>`
/// against the package's `Cargo.toml` and bundles the produced cdylib.
/// Pass `no_build = true` to disable this auto-build and require `lib/`
/// to be pre-populated.
pub fn pack(package_dir: &Path, output: Option<&Path>, no_build: bool) -> Result<()> {
    // 1. Load and validate streamlib.yaml
    let config = ProjectConfig::load(package_dir).context("Failed to load streamlib.yaml")?;

    let package = config
        .package
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("streamlib.yaml missing [package] section"))?;

    // Schema-only packages are first-class — `@tatolab/core` is the canonical
    // example, declaring four wire-stable types (`VideoFrame`, `AudioFrame`,
    // `EncodedVideoFrame`, `EncodedAudioFrame`) and zero processors. The
    // pack/install/consume cycle has to work for these or canonical-form
    // dependency declarations against `@tatolab/core` can't resolve through
    // the installed-package cache. A package is valid when it owns at least
    // one schema OR one processor (covering both ends — pure-schema packs
    // like core and pure-processor packs alike).
    let schema_files = collect_schema_files(package_dir)
        .context("Failed to enumerate the package's schema files")?;
    if config.processors.is_empty() && schema_files.is_empty() {
        anyhow::bail!(
            "streamlib.yaml at {} declares no processors AND no schemas. \
             A publishable package must own at least one of either.",
            package_dir.display()
        );
    }

    // Reject path-flavor `patch:` entries — patches are dev-time overrides
    // and don't generalize to a published artifact (paths are relative to
    // the consumer's source tree). Mirrors `npm publish` / `cargo publish`
    // rejecting path-flavor deps; the dev removes the path patch (or
    // converts it to a git/registry override) before publishing.
    reject_path_patches_for_pack(package_dir)?;

    // 2. Determine output filename
    let output_filename = format!("{}-{}.slpkg", package.name, package.version);
    let output_path = output
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| package_dir.join(&output_filename));

    // 3. Collect files to bundle
    let mut files_to_bundle: Vec<(String, std::path::PathBuf)> = Vec::new();

    // Required: streamlib.yaml
    files_to_bundle.push((
        "streamlib.yaml".to_string(),
        package_dir.join("streamlib.yaml"),
    ));

    // Optional: pyproject.toml (Python)
    let pyproject = package_dir.join("pyproject.toml");
    if pyproject.exists() {
        files_to_bundle.push(("pyproject.toml".to_string(), pyproject));
    }

    // Optional: deno.json (TypeScript)
    let deno_json = package_dir.join("deno.json");
    if deno_json.exists() {
        files_to_bundle.push(("deno.json".to_string(), deno_json));
    }

    // Bundle every schema YAML the package owns. Schema-only packages
    // (e.g. `@tatolab/core`) need this; processor-bearing packages do
    // too because the runtime reads schemas from disk during package
    // load. Path strings inside the zip mirror the on-disk relative
    // shape so `Manifest::load` against the extracted slpkg sees the
    // same `schemas:` list it saw at pack time.
    for schema_rel in &schema_files {
        let abs = package_dir.join(schema_rel);
        if !abs.exists() {
            anyhow::bail!(
                "Schema file declared in streamlib.yaml not found: {}",
                abs.display()
            );
        }
        let entry_name = schema_rel.to_string_lossy().replace('\\', "/");
        files_to_bundle.push((entry_name, abs));
    }

    // Collect source files based on processor entrypoints
    // Python: "module:Class" → module.py
    // TypeScript: "file.ts:Class" → file.ts
    for proc in &config.processors {
        if let Some(entrypoint) = &proc.entrypoint {
            let source_file = match proc.runtime.language {
                streamlib_processor_schema::ProcessorLanguage::Python => {
                    // "grayscale_processor:GrayscaleProcessor" → "grayscale_processor.py"
                    let module = entrypoint.split(':').next().unwrap_or(entrypoint);
                    format!("{}.py", module)
                }
                streamlib_processor_schema::ProcessorLanguage::TypeScript => {
                    // "halftone_processor.ts:HalftoneProcessor" → "halftone_processor.ts"
                    entrypoint
                        .split(':')
                        .next()
                        .unwrap_or(entrypoint)
                        .to_string()
                }
                streamlib_processor_schema::ProcessorLanguage::Rust => {
                    continue; // Rust processors don't have source files to bundle
                }
            };

            let source_path = package_dir.join(&source_file);
            if source_path.exists() {
                files_to_bundle.push((source_file, source_path));
            } else {
                anyhow::bail!(
                    "Processor '{}' entrypoint file not found: {}",
                    proc.name,
                    source_path.display()
                );
            }
        }
    }

    // Collect dylib files for Rust runtime processors.
    //
    // Resolution order:
    //   1. If `<dir>/lib/` already carries one or more host-OS dylibs,
    //      bundle them as-is (preserves the pre-existing pre-populated
    //      flow; user-supplied artifacts win over auto-build).
    //   2. Otherwise, when `no_build` is unset, invoke
    //      `cargo build --release -p <name>` against the package's
    //      `Cargo.toml` and bundle the produced cdylib.
    //   3. Otherwise (no_build is set), bail with an actionable error
    //      pointing the user at the cargo command they'd need to run.
    let has_rust_processors = config.processors.iter().any(|p| {
        matches!(
            p.runtime.language,
            streamlib_processor_schema::ProcessorLanguage::Rust
        )
    });
    if has_rust_processors {
        // Per-triple layout inside the archive (`lib/<triple>/<filename>`)
        // so the same .slpkg can carry artifacts for multiple host triples
        // and the loader picks the right one at runtime. This pack writes
        // only the current host's triple; a future fat-archive workflow
        // can add others without changing the load path.
        let host_triple = host_target_triple();
        let lib_dir = package_dir.join("lib");
        let triple_dir = lib_dir.join(host_triple);
        let dylib_ext = host_dylib_extension();

        // Populated layout on disk is also triple-keyed
        // (`lib/<triple>/...`); the archive mirrors that shape so the
        // load path is symmetric. Authors invoking `cargo build`
        // themselves write into `lib/<triple>/`; the auto-build branch
        // below writes the same way.
        let prebuilt = collect_host_dylibs_in_lib(&triple_dir, dylib_ext)?;

        if !prebuilt.is_empty() {
            for path in prebuilt {
                let filename = path
                    .file_name()
                    .expect("dylib path must have filename")
                    .to_string_lossy()
                    .into_owned();
                files_to_bundle.push((format!("lib/{}/{}", host_triple, filename), path));
            }
        } else if no_build {
            let cargo_hint = read_cargo_package_name(package_dir)
                .map(|name| format!("cargo build --release -p {n}", n = name))
                .unwrap_or_else(|_| "cargo build --release -p <name>".to_string());
            anyhow::bail!(
                "Package at {} declares Rust runtime processors but {} contains no \
                 host-OS dylib (`*.{}`) for triple `{}` and `--no-build` was specified. \
                 Either run `{}` to populate lib/{}/ first, \
                 or omit `--no-build` to let pack invoke cargo automatically.",
                package_dir.display(),
                triple_dir.display(),
                dylib_ext,
                host_triple,
                cargo_hint,
                host_triple,
            );
        } else {
            let cargo_name = read_cargo_package_name(package_dir).with_context(|| {
                format!(
                    "Package at {} declares Rust runtime processors but pack \
                     could not determine the Cargo crate name to build",
                    package_dir.display()
                )
            })?;
            let built = run_cargo_build_release(package_dir, &cargo_name, dylib_ext)?;
            let filename = built
                .file_name()
                .expect("cargo-produced dylib must have filename")
                .to_string_lossy()
                .into_owned();
            files_to_bundle.push((format!("lib/{}/{}", host_triple, filename), built));
        }
    }

    // 4. Create ZIP archive
    let file = File::create(&output_path)
        .with_context(|| format!("Failed to create {}", output_path.display()))?;
    let mut zip = ZipWriter::new(file);
    let options = FileOptions::<()>::default().compression_method(zip::CompressionMethod::Deflated);

    let mut added_files = std::collections::HashSet::new();
    for (name, path) in files_to_bundle {
        if added_files.contains(&name) {
            continue; // Skip duplicates
        }
        added_files.insert(name.clone());

        let mut contents = Vec::new();
        File::open(&path)
            .with_context(|| format!("Failed to open {}", path.display()))?
            .read_to_end(&mut contents)?;

        zip.start_file(&name, options)?;
        zip.write_all(&contents)?;
    }

    zip.finish()?;

    println!("Created: {}", output_path.display());
    println!("  Package: {} v{}", package.name, package.version);
    if !schema_files.is_empty() {
        println!("  Schemas: {}", schema_files.len());
        for schema in &schema_files {
            println!("    - {}", schema.display());
        }
    }
    if !config.processors.is_empty() {
        println!("  Processors: {}", config.processors.len());
        for proc in &config.processors {
            println!("    - {}", proc.name);
        }
    }

    Ok(())
}

/// Reject `patch:` entries with a `path:` flavor at pack time. The
/// resulting error names every offending entry so the dev can fix the
/// manifest in one pass.
fn reject_path_patches_for_pack(package_dir: &Path) -> Result<()> {
    let manifest_path = package_dir.join(Manifest::FILE_NAME);
    if !manifest_path.exists() {
        return Ok(());
    }
    let body = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let manifest: Manifest = serde_yaml::from_str(&body)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;
    if manifest.patch.is_empty() {
        return Ok(());
    }
    let path_offenders: Vec<String> = manifest
        .patch
        .iter()
        .filter_map(|(dep_ref, spec)| match spec {
            streamlib_idents::DependencySpec::Path(p) => {
                Some(format!("`{}` → `{}`", dep_ref, p.path.display()))
            }
            _ => None,
        })
        .collect();
    if path_offenders.is_empty() {
        return Ok(());
    }
    anyhow::bail!(
        "{} carries path-flavor `patch:` entries which are dev-time \
         overrides and not publishable: {}. Path patches don't \
         generalize to a published artifact (paths are relative to the \
         consumer's source tree). Remove the offending entries — or \
         convert them to a git/registry override — before packing.",
        manifest_path.display(),
        path_offenders.join(", "),
    );
}

/// Discover the schema YAML files this package owns. Two modes (mirrors
/// [`streamlib_idents::resolver`]'s discovery — the resolver and the
/// pack command must agree on what "owns" means):
///
/// 1. **Explicit** — `schemas: [...]` in the manifest. Each entry is a
///    relative path under the package dir.
/// 2. **Implicit** — every `*.yaml` / `*.yml` file in `schemas/`.
fn collect_schema_files(package_dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let manifest_path = package_dir.join(Manifest::FILE_NAME);
    if !manifest_path.exists() {
        return Ok(Vec::new());
    }
    let body = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
    let manifest: Manifest = serde_yaml::from_str(&body)
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;

    if let Some(declared) = manifest.schemas {
        // Local entries contribute their `file:` path; External entries are
        // imports from declared dependencies and not packed with this
        // package.
        let mut files: Vec<std::path::PathBuf> = declared
            .into_values()
            .filter_map(|entry| match entry {
                streamlib_idents::SchemaEntry::Local { file } => Some(file),
                streamlib_idents::SchemaEntry::External { .. } => None,
            })
            .collect();
        files.sort();
        return Ok(files);
    }

    let schemas_dir = package_dir.join("schemas");
    if !schemas_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&schemas_dir)
        .with_context(|| format!("Failed to read schemas dir: {}", schemas_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let ext = path.extension().and_then(|s| s.to_str());
        if matches!(ext, Some("yaml") | Some("yml")) {
            // Store relative to package_dir so the zip entry name matches
            // the manifest's `schemas:` shape after extraction.
            let rel = path
                .strip_prefix(package_dir)
                .unwrap_or(&path)
                .to_path_buf();
            files.push(rel);
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_yaml(dir: &Path, body: &str) {
        std::fs::write(dir.join("streamlib.yaml"), body).unwrap();
    }

    /// Minimal but valid `streamlib.yaml` declaring one Rust runtime
    /// processor. Used by tests that exercise the auto-build /
    /// populated-lib branches and need to traverse the full pack flow.
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

    fn write_cargo_toml(dir: &Path, name: &str) {
        let body = format!(
            r#"
[package]
name = "{}"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]
"#,
            name
        );
        std::fs::write(dir.join("Cargo.toml"), body).unwrap();
    }

    #[test]
    fn pack_rejects_path_flavor_patch_entries() {
        // Path-flavor patches are dev-time only — `streamlib pack` must
        // reject them. Mirrors `npm publish` / `cargo publish` rejecting
        // path-flavor deps. Mentally reverting the
        // `reject_path_patches_for_pack` call would let pack succeed and
        // ship a yaml that breaks at customer install time when the path
        // doesn't resolve in their cache.
        let dir = tempdir().unwrap();
        write_yaml(
            dir.path(),
            r#"
package:
  org: tatolab
  name: foo
  version: 1.0.0
dependencies:
  "@tatolab/core": "^1.0.0"
patch:
  "@tatolab/core":
    path: ../../../packages/core
"#,
        );
        let err = reject_path_patches_for_pack(dir.path())
            .expect_err("pack must reject path-flavor patch entries");
        let msg = format!("{err}");
        assert!(
            msg.contains("@tatolab/core"),
            "error must surface the offending dep ref, got: {msg}"
        );
        assert!(
            msg.contains("path-flavor") || msg.contains("not publishable"),
            "error must explain why path patches are rejected, got: {msg}"
        );
    }

    #[test]
    fn pack_accepts_yamls_with_no_patch_block() {
        // Customer-shape yaml: declares deps canonically, no `patch:`
        // block. This is the wire-form a customer's slpkg carries.
        let dir = tempdir().unwrap();
        write_yaml(
            dir.path(),
            r#"
package:
  org: tatolab
  name: foo
  version: 1.0.0
dependencies:
  "@tatolab/core": "^1.0.0"
"#,
        );
        reject_path_patches_for_pack(dir.path())
            .expect("yaml without `patch:` block must pack cleanly");
    }

    #[test]
    fn pack_with_no_build_and_empty_lib_returns_actionable_error() {
        // Rust runtime processors declared + lib/<triple>/ empty +
        // --no-build set must error with a message pointing the user at
        // the cargo command they'd need to run AND naming the host
        // triple subdir so the user knows where the artifact must land.
        // Reverting the no_build branch would silently invoke cargo and
        // fail later (or worse, succeed with the wrong artifact in CI).
        let dir = tempdir().unwrap();
        write_yaml(dir.path(), RUST_PLUGIN_YAML);
        write_cargo_toml(dir.path(), "test-plugin");
        std::fs::create_dir_all(dir.path().join("lib").join(host_target_triple())).unwrap();

        let err = pack(dir.path(), None, /* no_build */ true)
            .expect_err("--no-build with empty lib/<triple>/ must error");
        let msg = format!("{err}");
        assert!(
            msg.contains("--no-build"),
            "error must surface the offending flag, got: {msg}"
        );
        assert!(
            msg.contains("cargo build --release -p test-plugin"),
            "error must suggest the exact cargo command using the Cargo crate name from Cargo.toml, got: {msg}"
        );
        assert!(
            msg.contains(host_target_triple()),
            "error must surface the host triple so the user knows which lib/<triple>/ to populate, got: {msg}"
        );
    }

    #[test]
    fn pack_with_populated_lib_does_not_invoke_cargo_build() {
        // Pre-populated lib/<triple>/ flow is preserved verbatim: pack
        // picks up the host-OS dylib(s) and never reaches the auto-build
        // branch. The proof that cargo wasn't invoked: the tempdir is
        // outside any workspace, so a stray `cargo build -p test-plugin`
        // would fail to locate the crate and the test would error. Test
        // passing == cargo never ran. The dylib lands inside the zip
        // under `lib/<triple>/<filename>` so the loader on a matching
        // host can resolve unambiguously.
        let dir = tempdir().unwrap();
        write_yaml(dir.path(), RUST_PLUGIN_YAML);
        // Intentionally NO Cargo.toml — auto-build branch would fail
        // before invoking cargo, but the populated-lib branch should
        // skip Cargo.toml entirely.
        let triple_dir = dir.path().join("lib").join(host_target_triple());
        std::fs::create_dir_all(&triple_dir).unwrap();
        let host_ext = host_dylib_extension();
        let dylib_name = format!("libtest_plugin.{}", host_ext);
        std::fs::write(triple_dir.join(&dylib_name), b"fake-dylib-bytes").unwrap();

        let output = dir.path().join("out.slpkg");
        pack(dir.path(), Some(&output), /* no_build */ false)
            .expect("populated lib/<triple>/ must pack without invoking cargo");

        assert!(output.exists(), "expected slpkg at {}", output.display());
        // Verify the dylib landed inside the zip under
        // `lib/<triple>/<filename>` — the wire-format contract for the
        // triple-keyed layout the loader resolves against.
        let zip_bytes = std::fs::read(&output).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).unwrap();
        let entry_name = format!("lib/{}/{}", host_target_triple(), dylib_name);
        zip.by_name(&entry_name)
            .unwrap_or_else(|_| panic!("slpkg missing {} entry", entry_name));
        // Negative: the legacy flat `lib/<filename>` layout must NOT
        // appear — the new contract is triple-keyed, period.
        let legacy_entry = format!("lib/{}", dylib_name);
        assert!(
            zip.by_name(&legacy_entry).is_err(),
            "slpkg must not carry legacy flat `{}` entry alongside the triple-keyed one",
            legacy_entry
        );
    }

    #[test]
    fn pack_schemas_only_emits_no_lib_entries() {
        // Schemas-only packages (`@tatolab/core`, `@tatolab/escalate`)
        // have no Rust cdylib and the resulting slpkg must NOT contain a
        // `lib/` directory — neither flat nor triple-keyed. Mentally
        // reverting the `has_rust_processors` gate around the lib branch
        // would either error here (no Cargo.toml present) or silently
        // ship an empty lib dir; both shapes are wrong and the loader
        // would mis-resolve the package as a Rust-impl one.
        let dir = tempdir().unwrap();
        write_yaml(
            dir.path(),
            r#"
package:
  org: tatolab
  name: schemas-only
  version: 0.1.0
schemas:
  TestSchema:
    file: schemas/test_schema.yaml
"#,
        );
        let schemas_dir = dir.path().join("schemas");
        std::fs::create_dir(&schemas_dir).unwrap();
        std::fs::write(
            schemas_dir.join("test_schema.yaml"),
            "metadata:\n  type: TestSchema\n  max_payload_bytes: 1024\n",
        )
        .unwrap();

        let output = dir.path().join("schemas-only.slpkg");
        pack(dir.path(), Some(&output), /* no_build */ false)
            .expect("schemas-only package must pack without lib/ requirement");

        let zip_bytes = std::fs::read(&output).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).unwrap();
        for i in 0..zip.len() {
            let entry = zip.by_index(i).unwrap();
            assert!(
                !entry.name().starts_with("lib/"),
                "schemas-only slpkg must not contain any lib/ entries, got: {}",
                entry.name()
            );
        }
        // Sanity: the schema yaml itself is present.
        zip.by_name("schemas/test_schema.yaml")
            .expect("schemas-only slpkg must carry its declared schema yaml");
    }

    #[test]
    fn pack_accepts_git_flavor_patch_entries() {
        // Git patches are public content — a customer can resolve a git
        // ref the same way the dev can. Pack permits them.
        let dir = tempdir().unwrap();
        write_yaml(
            dir.path(),
            r#"
package:
  org: tatolab
  name: foo
  version: 1.0.0
dependencies:
  "@tatolab/core": "^1.0.0"
patch:
  "@tatolab/core":
    git: https://github.com/tatolab/core-fork
    rev: abc123def456
"#,
        );
        reject_path_patches_for_pack(dir.path())
            .expect("git-flavor patches must pack cleanly (public content)");
    }
}
