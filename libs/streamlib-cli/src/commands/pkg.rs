// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Package management commands.
//!
//! Single-package adoption (installing a published package, removing one) lives
//! in the top-level `streamlib add` / `streamlib remove` verbs
//! ([`super::add`]); `pkg` here is scoped to authoring artifacts of THIS
//! package — build, publish, clean, inspect — plus `list`.

use std::path::Path;

use anyhow::{Context, Result};
use streamlib::engine_internal::core::ProjectConfig;
use streamlib::engine_internal::core::InstalledPackageManifest;
use streamlib_idents::{RegistryClient, RegistryConfig};
use streamlib_pack::{assemble_artifact, AssembleOptions, AssembleTarget, CargoProfile, PathDepPolicy};

/// Build THIS package (the current working directory) into a source-only
/// `.slpkg`. Pure source bundling — no compilation, no prebuilt cdylib,
/// nothing path-related (the assembler refuses a path dep / path patch). The
/// artifact is a hand-off bundle; the consumer builds it from source.
pub fn build(output: Option<&Path>) -> Result<()> {
    let package_dir = std::env::current_dir().context("resolve current working directory")?;
    // Early friendly check; the load-bearing guard runs again inside
    // `assemble_artifact`'s Slpkg branch (streamlib-pack owns the seam).
    streamlib_pack::link_marker::ensure_no_active_link_for_pack(&package_dir)?;
    let output_path = resolve_slpkg_output(&package_dir, output)?;
    let outcome = assemble_source_slpkg(&package_dir, &output_path)?;
    println!("Built source-only package: {}", output_path.display());
    println!("  {} v{}", outcome.package_name, outcome.package_version);
    if outcome.schemas > 0 {
        println!("  Schemas: {}", outcome.schemas);
    }
    if outcome.processors > 0 {
        println!("  Processors: {}", outcome.processors);
    }
    Ok(())
}

/// Publish THIS package (the current working directory) into the static
/// registry tree's `.slpkg` generic store. Always repacks a fresh source-only
/// `.slpkg` to a temp file (never trusts a pre-existing artifact), then writes
/// it by version and refreshes the package's version index. The registry tree
/// root comes from `STREAMLIB_REGISTRY_URL` and must be a `file://` tree —
/// publishing writes files; a static HTTP mount is read-only.
pub fn publish() -> Result<()> {
    let package_dir = std::env::current_dir().context("resolve current working directory")?;
    // Early friendly check; the load-bearing guard runs again inside
    // `assemble_artifact`'s Slpkg branch (streamlib-pack owns the seam).
    streamlib_pack::link_marker::ensure_no_active_link_for_pack(&package_dir)?;
    // Lightweight manifest read — package metadata only, NO dependency
    // resolution (which would require the registry just to read name/version).
    let config = streamlib_cargo_build::read_minimal_project_config(&package_dir)
        .context("Failed to read streamlib.yaml")?
        .ok_or_else(|| anyhow::anyhow!("no streamlib.yaml at {}", package_dir.display()))?;
    let package = config
        .package
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("streamlib.yaml missing [package] section"))?;

    let registry = RegistryConfig::from_env().ok_or_else(|| {
        anyhow::anyhow!(
            "registry not configured: set STREAMLIB_REGISTRY_URL to a file:// registry tree \
             (e.g. file:///path/to/registry-tree) to publish"
        )
    })?;

    // Always repack fresh into a temp file — publish never trusts a
    // pre-existing artifact (pack runs independently, at any time).
    let tmp = tempfile::Builder::new()
        .prefix("streamlib-publish-")
        .suffix(".slpkg")
        .tempfile()
        .context("create temp .slpkg")?;
    let outcome = assemble_source_slpkg(&package_dir, tmp.path())?;
    let bytes = std::fs::read(tmp.path()).context("read packed .slpkg")?;

    let pkg_ref =
        streamlib_idents::PackageRef::new(package.org.clone(), package.name.clone());
    let client = RegistryClient::new(&registry);
    println!(
        "Publishing {} v{} ({} bytes) to {}…",
        outcome.package_name,
        outcome.package_version,
        bytes.len(),
        registry.base_url
    );
    let url = client
        .upload_slpkg(&pkg_ref, package.version, &bytes)
        .map_err(|e| anyhow::anyhow!("upload failed: {}", e))?;
    println!("Published → {url}");
    Ok(())
}

/// Remove THIS package's build/pack artifacts from the current working
/// directory: any `*.slpkg`, the prebuilt `lib/` dir, and the generated
/// `_generated_/` wire-vocabulary trees (root + `python/`). All are
/// regenerated on the next build/pack.
pub fn clean() -> Result<()> {
    let dir = std::env::current_dir().context("resolve current working directory")?;
    let mut removed: Vec<String> = Vec::new();

    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("slpkg") {
                if std::fs::remove_file(&p).is_ok() {
                    removed.push(p.file_name().unwrap_or_default().to_string_lossy().into_owned());
                }
            }
        }
    }
    let lib = dir.join("lib");
    if lib.is_dir() && std::fs::remove_dir_all(&lib).is_ok() {
        removed.push("lib/".to_string());
    }
    for cand in [dir.join("_generated_"), dir.join("python").join("_generated_")] {
        if cand.is_dir() && std::fs::remove_dir_all(&cand).is_ok() {
            let rel = cand.strip_prefix(&dir).unwrap_or(&cand);
            removed.push(format!("{}/", rel.display()));
        }
    }

    if removed.is_empty() {
        println!("Nothing to clean.");
    } else {
        println!("Removed: {}", removed.join(", "));
    }
    Ok(())
}

/// Resolve the default `.slpkg` output path (`{name}-{version}.slpkg` in the
/// package dir) when `--output` isn't given.
fn resolve_slpkg_output(package_dir: &Path, output: Option<&Path>) -> Result<std::path::PathBuf> {
    match output {
        Some(p) => Ok(p.to_path_buf()),
        None => {
            let config = streamlib_cargo_build::read_minimal_project_config(package_dir)
                .context("Failed to read streamlib.yaml")?
                .ok_or_else(|| anyhow::anyhow!("no streamlib.yaml at {}", package_dir.display()))?;
            let package = config
                .package
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("streamlib.yaml missing [package] section"))?;
            Ok(package_dir.join(format!("{}-{}.slpkg", package.name.as_str(), package.version)))
        }
    }
}

/// Assemble a source-only `.slpkg` at `output_path`. The `Slpkg` target makes
/// `assemble_artifact` ship source only (no cdylib build) and enforce the
/// no-path contract; `no_build` / `profile` are inert on this path.
fn assemble_source_slpkg(
    package_dir: &Path,
    output_path: &Path,
) -> Result<streamlib_pack::AssembleOutcome> {
    assemble_artifact(
        package_dir,
        &AssembleTarget::Slpkg(output_path.to_path_buf()),
        &AssembleOptions {
            no_build: false,
            profile: CargoProfile::Release,
            path_deps: PathDepPolicy::RejectPathPatches,
        },
        &(),
    )
    .map_err(|e| anyhow::anyhow!("pack failed: {}", e))
}

/// Inspect a .slpkg package without installing it.
pub fn inspect(path: &std::path::Path) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("File not found: {}", path.display());
    }

    let file =
        std::fs::File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .with_context(|| format!("Failed to read ZIP archive: {}", path.display()))?;

    // Find and read streamlib.yaml from the archive
    let yaml_content = {
        let mut entry = archive
            .by_name("streamlib.yaml")
            .with_context(|| "Package missing streamlib.yaml")?;
        let mut buf = String::new();
        std::io::Read::read_to_string(&mut entry, &mut buf)?;
        buf
    };

    let config: ProjectConfig =
        serde_yaml::from_str(&yaml_content).with_context(|| "Failed to parse streamlib.yaml")?;

    let package = config
        .package
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Package missing [package] section"))?;

    println!("Package: {} v{}", package.name, package.version);
    if let Some(desc) = &package.description {
        println!("Description: {}", desc);
    }
    if let Some(sv) = &package.streamlib_version {
        println!("Requires: streamlib {}", sv);
    }

    if !config.processors.is_empty() {
        println!();
        println!("Processors ({}):", config.processors.len());
        for proc in &config.processors {
            println!("  {} v{}", proc.name, proc.version);
            if let Some(desc) = &proc.description {
                println!("    {}", desc);
            }
            println!("    Runtime:   {:?}", proc.runtime.language);
            println!("    Execution: {:?}", proc.execution);
            if !proc.inputs.is_empty() {
                println!("    Inputs:");
                for input in &proc.inputs {
                    println!("      - {} ({})", input.name, input.schema);
                }
            }
            if !proc.outputs.is_empty() {
                println!("    Outputs:");
                for output in &proc.outputs {
                    println!("      - {} ({})", output.name, output.schema);
                }
            }
            if let Some(config_ref) = &proc.config {
                println!("    Config:    {} ({})", config_ref.name, config_ref.schema);
            }
        }
    }

    // List files in archive
    println!();
    println!("Files ({}):", archive.len());
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            println!("  {}", entry.name());
        }
    }

    Ok(())
}

/// List installed packages.
pub fn list() -> Result<()> {
    let manifest = InstalledPackageManifest::load()
        .map_err(|e| anyhow::anyhow!("Failed to load packages manifest: {}", e))?;

    if manifest.packages.is_empty() {
        println!("No packages installed.");
        println!();
        println!("Add a package with:");
        println!("  streamlib add @org/name          # from the registry");
        println!("  streamlib add ./path/to.slpkg    # from a local artifact");
        return Ok(());
    }

    println!("Installed packages ({}):\n", manifest.packages.len());

    for pkg in &manifest.packages {
        println!("  {} v{}", pkg.name, pkg.version);
        if let Some(desc) = &pkg.description {
            println!("    {}", desc);
        }
        println!("    Installed: {}", pkg.installed_at);
        println!("    Source:    {}", pkg.installed_from);
        println!();
    }

    Ok(())
}

