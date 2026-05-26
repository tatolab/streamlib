// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

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

    // Collect source files based on processor entrypoints.
    //
    // - Python: `.py` stays at archive root so the package's
    //   `pyproject.toml` (also at root) finds it via the build backend's
    //   default `packages = ["."]` shape. Pre-built wheels land under
    //   `python/wheels/` in the block further down.
    // - TypeScript: `.ts` moves to `deno/<module>.ts` for layout symmetry
    //   with `python/wheels/`. The Deno subprocess runner resolves
    //   entrypoints against `deno/` first and falls back to archive root
    //   for legacy slpkgs.
    // - Rust: no source bundled (the cdylib is the artifact).
    for proc in &config.processors {
        if let Some(entrypoint) = &proc.entrypoint {
            let (source_file, archive_path) = match proc.runtime.language {
                streamlib_processor_schema::ProcessorLanguage::Python => {
                    // "grayscale_processor:GrayscaleProcessor" → "grayscale_processor.py"
                    let module = entrypoint.split(':').next().unwrap_or(entrypoint);
                    let source = format!("{}.py", module);
                    let archive = source.clone();
                    (source, archive)
                }
                streamlib_processor_schema::ProcessorLanguage::TypeScript => {
                    // "halftone_processor.ts:HalftoneProcessor" → "halftone_processor.ts"
                    let source = entrypoint
                        .split(':')
                        .next()
                        .unwrap_or(entrypoint)
                        .to_string();
                    let archive = format!("deno/{}", source);
                    (source, archive)
                }
                streamlib_processor_schema::ProcessorLanguage::Rust => {
                    continue; // Rust processors don't have source files to bundle
                }
            };

            let source_path = package_dir.join(&source_file);
            if source_path.exists() {
                files_to_bundle.push((archive_path, source_path));
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

    // Collect or build the Python wheel for packages with Python runtime
    // processors.
    //
    // Resolution mirrors the Rust dylib branch:
    //
    //   1. If `<dir>/python/wheels/` already carries `*.whl`, bundle as-is
    //      (pre-populated flow; CI / multi-platform-matrix builds win).
    //   2. Otherwise, when `no_build` is unset AND `pyproject.toml` is
    //      present, invoke `uv build --wheel --out-dir <tmp>` against
    //      `<package_dir>` and bundle the produced wheel(s).
    //   3. Otherwise (no_build is set OR no pyproject.toml), surface an
    //      actionable error / skip.
    //
    // Wheels self-tag their target in the filename (`py3-none-any` for
    // pure-Python, `cp312-cp312-manylinux_2_17_x86_64` for native), so
    // unlike Rust dylibs the archive doesn't need a per-triple subdir —
    // the loader glob-matches the right wheel against the install
    // machine's interpreter.
    //
    // Bundled wheels are load-bearing for container deploys: the runtime
    // container can `uv pip install <wheel>` (binary install — no build
    // backend like hatchling / maturin needed at install time) instead of
    // `uv pip install -e <project_path>` (which runs the package's
    // declared build backend on the install machine).
    let has_python_processors = config.processors.iter().any(|p| {
        matches!(
            p.runtime.language,
            streamlib_processor_schema::ProcessorLanguage::Python
        )
    });
    let mut python_wheels_bundled = 0usize;
    if has_python_processors {
        let wheels_dir = package_dir.join("python").join("wheels");
        let prebuilt = collect_wheels_in_dir(&wheels_dir)?;

        if !prebuilt.is_empty() {
            for path in prebuilt {
                let filename = path
                    .file_name()
                    .expect("wheel path must have filename")
                    .to_string_lossy()
                    .into_owned();
                files_to_bundle.push((format!("python/wheels/{}", filename), path));
                python_wheels_bundled += 1;
            }
        } else if no_build {
            anyhow::bail!(
                "Package at {} declares Python runtime processors but {} contains no \
                 pre-built wheel (`*.whl`) and `--no-build` was specified. \
                 Either run `uv build --wheel --out-dir {}` to populate the wheels \
                 directory first, or omit `--no-build` to let pack invoke uv automatically.",
                package_dir.display(),
                wheels_dir.display(),
                wheels_dir.display(),
            );
        } else {
            let pyproject = package_dir.join("pyproject.toml");
            if !pyproject.exists() {
                // No pyproject.toml — pack ships the source `.py` files
                // (already bundled above) and falls back to source-install
                // at load time. Same shape as a Python package without a
                // build backend.
                tracing::info!(
                    "Package at {} declares Python processors but has no \
                     pyproject.toml; skipping wheel build — load-time source-install \
                     remains the only path.",
                    package_dir.display()
                );
            } else {
                let built_wheels = run_uv_build_wheel(package_dir)?;
                for path in built_wheels {
                    let filename = path
                        .file_name()
                        .expect("wheel path must have filename")
                        .to_string_lossy()
                        .into_owned();
                    files_to_bundle.push((format!("python/wheels/{}", filename), path));
                    python_wheels_bundled += 1;
                }
            }
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
    if python_wheels_bundled > 0 {
        println!("  Python wheels: {}", python_wheels_bundled);
    }

    Ok(())
}

/// Enumerate `*.whl` files in `wheels_dir`. Returns empty when the
/// directory does not exist or carries no wheels — the caller decides
/// whether that's an error.
fn collect_wheels_in_dir(wheels_dir: &Path) -> Result<Vec<PathBuf>> {
    if !wheels_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir(wheels_dir)
        .with_context(|| format!("Failed to read wheels directory: {}", wheels_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "whl") {
            found.push(path);
        }
    }
    found.sort();
    Ok(found)
}

/// Invoke `uv build --wheel --out-dir <tmp>` against `package_dir`
/// and return the produced wheel path(s) on disk inside the tempdir.
///
/// The tempdir is leaked intentionally for the duration of the pack
/// command — its contents are read into the zip archive before pack
/// returns. The OS reclaims `/tmp/` on the next reboot; for the rare
/// long-lived `streamlib pack` process this is acceptable since the
/// wheel itself is tiny (~10 KB pure-Python, single-digit MB native).
///
/// Falls back to `python -m build --wheel --outdir <tmp>` when `uv`
/// is not on PATH, with an error pointing at the install command.
fn run_uv_build_wheel(package_dir: &Path) -> Result<Vec<PathBuf>> {
    let out_dir = tempfile::tempdir()
        .context("Failed to create tempdir for `uv build --wheel` output")?
        // `TempDir::keep()` leaks the dir so the wheel files outlive
        // the `TempDir` guard's drop; the OS reclaims `/tmp/` on reboot.
        .keep();

    tracing::info!(
        "Building Python wheel for {} (uv build --wheel --out-dir {})",
        package_dir.display(),
        out_dir.display(),
    );

    let uv_available = Command::new("uv")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let output = if uv_available {
        Command::new("uv")
            .arg("build")
            .arg("--wheel")
            .arg("--out-dir")
            .arg(&out_dir)
            .arg(package_dir)
            .output()
            .with_context(|| {
                format!(
                    "Failed to invoke `uv build --wheel` in {}",
                    package_dir.display()
                )
            })?
    } else {
        // Fallback to `python -m build` so build hosts without `uv` still
        // work. `build` is the canonical PEP 517 frontend; it's a smaller
        // dep but a more typical dev-machine baseline.
        tracing::info!(
            "uv not found on PATH; falling back to `python -m build --wheel` \
             (install uv via `curl -LsSf https://astral.sh/uv/install.sh | sh` \
             for the fast path)"
        );
        Command::new("python")
            .arg("-m")
            .arg("build")
            .arg("--wheel")
            .arg("--outdir")
            .arg(&out_dir)
            .arg(package_dir)
            .output()
            .with_context(|| {
                format!(
                    "Failed to invoke `python -m build --wheel` in {}. \
                     Install uv (curl -LsSf https://astral.sh/uv/install.sh | sh) \
                     or `pip install build` to provide the wheel-build toolchain.",
                    package_dir.display()
                )
            })?
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        anyhow::bail!(
            "wheel build failed for {} (see above). stderr: {}\nstdout: {}",
            package_dir.display(),
            stderr.trim(),
            stdout.trim(),
        );
    }

    let wheels = collect_wheels_in_dir(&out_dir)?;
    if wheels.is_empty() {
        anyhow::bail!(
            "wheel build for {} produced no `*.whl` in {}. \
             Confirm the package's pyproject.toml declares a working build backend.",
            package_dir.display(),
            out_dir.display(),
        );
    }
    Ok(wheels)
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

    /// Minimal but valid `streamlib.yaml` declaring one Python runtime
    /// processor. Used by tests that exercise the wheel-bundling branches.
    const PYTHON_PLUGIN_YAML: &str = r#"
package:
  org: tatolab
  name: py-plugin
  version: 0.1.0
processors:
  - name: PyProc
    version: 1.0.0
    description: "py"
    runtime: python
    execution: manual
    entrypoint: "py_proc:PyProc"
    inputs: []
    outputs: []
"#;

    /// Minimal but valid `streamlib.yaml` declaring one TypeScript / Deno
    /// runtime processor. Used by tests that exercise the `deno/` layout.
    const DENO_PLUGIN_YAML: &str = r#"
package:
  org: tatolab
  name: ts-plugin
  version: 0.1.0
processors:
  - name: TsProc
    version: 1.0.0
    description: "ts"
    runtime: deno
    execution: manual
    entrypoint: "ts_proc.ts:default"
    inputs: []
    outputs: []
"#;

    #[test]
    fn pack_python_with_prebuilt_wheel_bundles_under_python_wheels() {
        // Pre-populated `python/wheels/<wheel>.whl` flow — the customer
        // / CI / multi-platform-matrix build path. Pack picks the wheel
        // up as-is and ships it inside the zip under the same path so
        // the loader's `python/wheels/` glob finds it. Mentally reverting
        // the wheel-bundling block would leave the slpkg with the .py
        // source only, and `ensure_processor_venv` would fall back to
        // `uv pip install -e <project_path>` at load time — defeating
        // the no-toolchain-at-install-time contract this PR is about.
        let dir = tempdir().unwrap();
        write_yaml(dir.path(), PYTHON_PLUGIN_YAML);
        // Processor entrypoint source — required by pack so the slpkg
        // is loadable when source-install fallback runs.
        std::fs::write(dir.path().join("py_proc.py"), b"# stub").unwrap();
        // Pre-built wheel.
        let wheels_dir = dir.path().join("python").join("wheels");
        std::fs::create_dir_all(&wheels_dir).unwrap();
        let wheel_filename = "py_plugin-0.1.0-py3-none-any.whl";
        std::fs::write(wheels_dir.join(wheel_filename), b"PK\x03\x04fake-wheel-bytes").unwrap();

        let output = dir.path().join("py.slpkg");
        pack(dir.path(), Some(&output), /* no_build */ true)
            .expect("populated python/wheels/ must pack with --no-build");

        let zip_bytes = std::fs::read(&output).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).unwrap();
        let entry_name = format!("python/wheels/{}", wheel_filename);
        zip.by_name(&entry_name)
            .unwrap_or_else(|_| panic!("slpkg missing {} entry", entry_name));
    }

    #[test]
    fn pack_python_with_no_build_and_empty_wheels_returns_actionable_error() {
        // Python runtime processors declared + empty python/wheels/ +
        // --no-build → actionable error naming the wheel-build command
        // the user should run. Mirrors the Rust dylib path's `--no-build`
        // contract. Reverting the no_build/Python branch would let pack
        // succeed with no wheel (relying on source-install at load time)
        // — silently undermining the no-toolchain-at-install-time guarantee
        // the `--no-build` flag exists to enforce.
        let dir = tempdir().unwrap();
        write_yaml(dir.path(), PYTHON_PLUGIN_YAML);
        std::fs::write(dir.path().join("py_proc.py"), b"# stub").unwrap();
        std::fs::create_dir_all(dir.path().join("python").join("wheels")).unwrap();

        let err = pack(dir.path(), None, /* no_build */ true)
            .expect_err("--no-build with empty wheels dir must error");
        let msg = format!("{err}");
        assert!(
            msg.contains("--no-build"),
            "error must surface the offending flag, got: {msg}"
        );
        assert!(
            msg.contains("uv build --wheel"),
            "error must suggest the wheel-build command, got: {msg}"
        );
        assert!(
            msg.contains("python/wheels"),
            "error must name the python/wheels dir so the user knows where to populate, got: {msg}"
        );
    }

    #[test]
    fn pack_deno_lands_source_under_deno_subdir() {
        // TypeScript processor source lands at `deno/<module>.ts` inside
        // the zip — NOT at archive root. The Deno subprocess runner
        // resolves the entrypoint against `deno/` first and falls back
        // to archive root for legacy slpkgs; this test pins the new
        // layout. Reverting the per-language archive-path switch in the
        // entrypoint loop would silently regress every Deno slpkg back
        // to the archive-root layout and break layout symmetry with the
        // Python wheels block.
        let dir = tempdir().unwrap();
        write_yaml(dir.path(), DENO_PLUGIN_YAML);
        std::fs::write(dir.path().join("ts_proc.ts"), b"export default class {}").unwrap();

        let output = dir.path().join("ts.slpkg");
        pack(dir.path(), Some(&output), /* no_build */ false)
            .expect("Deno-only package must pack");

        let zip_bytes = std::fs::read(&output).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).unwrap();
        zip.by_name("deno/ts_proc.ts")
            .expect("Deno source must land under deno/<module>.ts");
        // Negative: the legacy archive-root layout must NOT appear; the
        // contract is `deno/<module>.ts`, period.
        assert!(
            zip.by_name("ts_proc.ts").is_err(),
            "slpkg must not carry legacy archive-root ts_proc.ts alongside deno/ts_proc.ts"
        );
    }

    #[test]
    fn pack_schemas_only_emits_no_python_or_deno_subdirs() {
        // Schemas-only packages (`@tatolab/core`, `@tatolab/escalate`)
        // have no Python or Deno processors → pack must NOT emit a
        // `python/` or `deno/` directory inside the slpkg. The smoke
        // value is that nothing spuriously sneaks in: ambient files in
        // the package_dir (a stray `python/` left from a previous
        // build, a `deno/vendor/` folder, etc.) must not get bundled
        // when the manifest declares no Python/Deno processors. The
        // per-processor source loop and the wheel-bundling block both
        // gate on the manifest's runtime languages, so a schemas-only
        // pack must produce a slpkg with `lib/`, `python/`, and `deno/`
        // all absent — the loader's runtime detection looks at the
        // manifest, not at directory presence, so a stray subdir
        // wouldn't break loading, but bundling unrelated files in a
        // schemas-only pack is a wire-format integrity bug worth
        // catching.
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
            .expect("schemas-only package must pack");

        let zip_bytes = std::fs::read(&output).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).unwrap();
        for i in 0..zip.len() {
            let entry = zip.by_index(i).unwrap();
            assert!(
                !entry.name().starts_with("python/"),
                "schemas-only slpkg must not contain any python/ entries, got: {}",
                entry.name()
            );
            assert!(
                !entry.name().starts_with("deno/"),
                "schemas-only slpkg must not contain any deno/ entries, got: {}",
                entry.name()
            );
        }
    }

    #[test]
    fn collect_wheels_in_dir_filters_to_whl_extension() {
        // Helper invariant: only files ending in `.whl` are returned.
        // Mentally reverting the extension filter would slurp every
        // file in `python/wheels/` and ship junk inside the slpkg
        // (sdist tarballs, .gitkeep, README.md, etc.).
        let dir = tempdir().unwrap();
        let wheels = dir.path().join("python").join("wheels");
        std::fs::create_dir_all(&wheels).unwrap();
        std::fs::write(
            wheels.join("foo-0.1.0-py3-none-any.whl"),
            b"wheel-bytes",
        )
        .unwrap();
        std::fs::write(wheels.join("foo-0.1.0.tar.gz"), b"sdist-bytes").unwrap();
        std::fs::write(wheels.join(".gitkeep"), b"").unwrap();

        let found = collect_wheels_in_dir(&wheels).unwrap();
        assert_eq!(found.len(), 1, "only wheel files should match, got: {:?}", found);
        assert!(found[0].ends_with("foo-0.1.0-py3-none-any.whl"));
    }

    #[test]
    fn collect_wheels_in_dir_returns_empty_when_dir_missing() {
        let dir = tempdir().unwrap();
        let wheels = dir.path().join("python").join("wheels");
        // Intentionally not creating the dir.
        let found = collect_wheels_in_dir(&wheels).unwrap();
        assert!(found.is_empty(), "missing dir must return empty list, got: {:?}", found);
    }
}
