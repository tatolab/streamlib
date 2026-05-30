// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Package management commands.

use anyhow::{Context, Result};
use streamlib::engine_internal::core::{InstalledPackageEntry, InstalledPackageManifest, ProjectConfig};
use streamlib::sdk::runtime::{
    extract_slpkg_to_cache, host_target_triple, BuildEvent, BuildEventSink, BuildOrchestrator,
    BuildPolicy, BuildRequest, BuildSource, BuildStream,
};
use streamlib::sdk::PolyglotBuildOrchestrator;

/// Routes the orchestrator's build diagnostics to the CLI's stdout/stderr
/// during `pkg install` (the engine default sink re-emits via `tracing`,
/// but the CLI prints progress directly for the interactive install flow).
struct CliBuildSink;

impl BuildEventSink for CliBuildSink {
    fn emit(&self, event: BuildEvent) {
        match event {
            BuildEvent::Started { language } => {
                println!("    [{language}] build started");
            }
            BuildEvent::Line { stream, line } => match stream {
                BuildStream::Stdout => println!("    {line}"),
                BuildStream::Stderr => eprintln!("    {line}"),
            },
            BuildEvent::Finished { language } => {
                println!("    [{language}] build finished");
            }
            // `BuildEvent` is `#[non_exhaustive]`; a future variant prints nothing.
            _ => {}
        }
    }
}

/// Install a .slpkg package from a local path or HTTP URL.
pub async fn install(source: &str) -> Result<()> {
    let slpkg_path = if source.starts_with("http://") || source.starts_with("https://") {
        // Download to temp file
        println!("Downloading {}...", source);
        let response = reqwest::get(source)
            .await
            .with_context(|| format!("Failed to download {}", source))?;

        if !response.status().is_success() {
            anyhow::bail!("Download failed: HTTP {} for {}", response.status(), source);
        }

        let bytes = response
            .bytes()
            .await
            .with_context(|| "Failed to read response body")?;

        let temp_dir = std::env::temp_dir();
        let temp_path = temp_dir.join("streamlib-pkg-download.slpkg");
        std::fs::write(&temp_path, &bytes)
            .with_context(|| format!("Failed to write temp file {}", temp_path.display()))?;
        temp_path
    } else {
        let path = std::path::PathBuf::from(source);
        if !path.exists() {
            anyhow::bail!("File not found: {}", source);
        }
        path
    };

    println!("Installing {}...", slpkg_path.display());

    // Extract to cache
    let cache_dir = extract_slpkg_to_cache(&slpkg_path)
        .map_err(|e| anyhow::anyhow!("Failed to extract package: {}", e))?;

    // Load project config from extracted cache to get metadata
    let config = ProjectConfig::load(&cache_dir)
        .map_err(|e| anyhow::anyhow!("Failed to load package config: {}", e))?;

    let package = config
        .package
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Package missing [package] section in streamlib.yaml"))?;

    // Provision the package's runtime via the build orchestrator — the same
    // `materialize` path module-load drives at runtime. For a Python package
    // this re-stages the extracted artifact and provisions its self-contained
    // venv at `{staged_package_dir}/.venv/bin/python` as the tail of the
    // materialize step. A package with no Python runtime is a no-op on the
    // venv tail; a Rust package re-builds its cdylib (cargo short-circuits
    // when clean). `AlwaysBuild` forces the venv provision regardless of any
    // prior stale staging.
    let has_python = config.processors.iter().any(|p| {
        matches!(
            p.runtime.language,
            streamlib_processor_schema::ProcessorLanguage::Python
        )
    });
    if has_python {
        println!("  Provisioning Python venv via the build orchestrator...");
    }
    let request = BuildRequest {
        package: streamlib_processor_schema::PackageRef::new(
            package.org.clone(),
            package.name.clone(),
        ),
        source: BuildSource::PackageDir(cache_dir.clone()),
        policy: BuildPolicy::AlwaysBuild,
        host_triple: host_target_triple().to_string(),
    };
    let orchestrator = PolyglotBuildOrchestrator::default();
    let sink = CliBuildSink;
    let staged = orchestrator
        .materialize(&request, &sink)
        .map_err(|e| anyhow::anyhow!("Failed to materialize package: {}", e))?;
    // The orchestrator stages into the package cache slot
    // (`cache/packages/<name>-<version>/`), the same slot
    // `extract_slpkg_to_cache` wrote to. This is order-safe: `materialize`
    // fully reads the source (assemble into a temp dir + provision the venv
    // there) before its closing `atomic_swap` wipes-and-replaces the slot,
    // so the extracted source is consumed before it's overwritten. Keep
    // using the returned staged path below.
    let cache_dir = staged.staged_dir;

    // Add to installed packages manifest. Identity is the canonical
    // `@org/name` PackageRef — the typed-key contract from #717 means
    // entries with the same short name from different orgs no longer
    // collide.
    let mut manifest = InstalledPackageManifest::load()
        .map_err(|e| anyhow::anyhow!("Failed to load packages manifest: {}", e))?;

    let entry = InstalledPackageEntry {
        name: streamlib_processor_schema::PackageRef::new(
            package.org.clone(),
            package.name.clone(),
        ),
        version: package.version,
        description: package.description.clone(),
        installed_from: source.to_string(),
        installed_at: chrono::Utc::now().to_rfc3339(),
        cache_dir: cache_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string(),
    };

    manifest.add(entry);
    manifest
        .save()
        .map_err(|e| anyhow::anyhow!("Failed to save packages manifest: {}", e))?;

    println!();
    println!("Installed {} v{}", package.name, package.version);
    if let Some(desc) = &package.description {
        println!("  {}", desc);
    }
    println!("  Cache: {}", cache_dir.display());

    // Clean up temp file if we downloaded
    if source.starts_with("http://") || source.starts_with("https://") {
        let _ = std::fs::remove_file(&slpkg_path);
    }

    Ok(())
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
        println!("Install a package with:");
        println!("  streamlib pkg install <path-to.slpkg>");
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

/// Remove an installed package. The argument must be the canonical
/// `@org/name` form — the typed-key contract introduced in #717 means
/// bare short names like `core` no longer disambiguate which package to
/// remove.
pub fn remove(name: &str) -> Result<()> {
    let package_ref = parse_canonical_package_ref(name)?;

    let mut manifest = InstalledPackageManifest::load()
        .map_err(|e| anyhow::anyhow!("Failed to load packages manifest: {}", e))?;

    let entry = match manifest.remove_by_ref(&package_ref) {
        Some(e) => e,
        None => {
            anyhow::bail!("Package '{}' is not installed.", package_ref);
        }
    };

    // Delete cache directory
    let cache_dir = streamlib::engine_internal::core::get_cached_package_dir(&entry.cache_dir);
    if cache_dir.exists() {
        std::fs::remove_dir_all(&cache_dir)
            .with_context(|| format!("Failed to remove cache dir {}", cache_dir.display()))?;
        println!("Removed cache: {}", cache_dir.display());
    }

    // Save manifest
    manifest
        .save()
        .map_err(|e| anyhow::anyhow!("Failed to save packages manifest: {}", e))?;

    println!("Removed package '{}'.", package_ref);

    Ok(())
}

/// Convert a CLI-supplied canonical-form string (`@org/name`) into a
/// typed [`streamlib_processor_schema::PackageRef`] via the official
/// Deserialize path. Wraps the round-trip with a CLI-friendly error so
/// users see "expected `@org/name`" rather than a serde parse error.
fn parse_canonical_package_ref(arg: &str) -> Result<streamlib_processor_schema::PackageRef> {
    serde_yaml::from_value::<streamlib_processor_schema::PackageRef>(serde_yaml::Value::String(
        arg.to_string(),
    ))
    .with_context(|| {
        format!(
            "Invalid canonical package reference '{}'. Expected `@org/name` form (e.g. `@tatolab/core`).",
            arg
        )
    })
}
