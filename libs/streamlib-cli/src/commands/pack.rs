// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use zip::write::FileOptions;
use zip::ZipWriter;

use streamlib::engine_internal::core::config::ProjectConfig;
use streamlib_idents::Manifest;

/// Pack a processor package into a .slpkg bundle.
pub fn pack(package_dir: &Path, output: Option<&Path>) -> Result<()> {
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

    // Collect dylib files for Rust runtime processors
    let has_rust_processors = config.processors.iter().any(|p| {
        matches!(
            p.runtime.language,
            streamlib_processor_schema::ProcessorLanguage::Rust
        )
    });
    if has_rust_processors {
        let lib_dir = package_dir.join("lib");
        let dylib_ext = if cfg!(target_os = "macos") {
            "dylib"
        } else if cfg!(target_os = "windows") {
            "dll"
        } else {
            "so"
        };

        if lib_dir.is_dir() {
            for entry in std::fs::read_dir(&lib_dir)
                .with_context(|| format!("Failed to read lib/ directory: {}", lib_dir.display()))?
            {
                let entry = entry?;
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == dylib_ext) {
                    let filename = path
                        .file_name()
                        .expect("dylib path must have filename")
                        .to_string_lossy();
                    files_to_bundle.push((format!("lib/{}", filename), path));
                }
            }
        } else {
            anyhow::bail!(
                "Rust runtime processors declared but no lib/ directory found at {}",
                lib_dir.display()
            );
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
        return Ok(declared);
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
