// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Package management commands.

use anyhow::{Context, Result};
use streamlib::core::config::{InstalledPackageEntry, InstalledPackageManifest, ProjectConfig};
use streamlib::core::runtime::extract_slpkg_to_cache;

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

    // For Python processors, ensure venvs are created
    for processor in &config.processors {
        if matches!(
            processor.runtime.language,
            streamlib_codegen_shared::ProcessorLanguage::Python
        ) {
            println!("  Setting up Python venv for {}...", processor.name);
            streamlib::core::compiler::compiler_ops::ensure_processor_venv(
                &processor.name,
                &cache_dir,
            )
            .map_err(|e| anyhow::anyhow!("Failed to create venv for {}: {}", processor.name, e))?;
        }
    }

    // Add to installed packages manifest
    let mut manifest = InstalledPackageManifest::load()
        .map_err(|e| anyhow::anyhow!("Failed to load packages manifest: {}", e))?;

    let entry = InstalledPackageEntry {
        name: package.name.clone(),
        version: package.version.clone(),
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

/// Remove an installed package.
pub fn remove(name: &str) -> Result<()> {
    let mut manifest = InstalledPackageManifest::load()
        .map_err(|e| anyhow::anyhow!("Failed to load packages manifest: {}", e))?;

    let entry = match manifest.remove_by_name(name) {
        Some(e) => e,
        None => {
            anyhow::bail!("Package '{}' is not installed.", name);
        }
    };

    // Delete cache directory
    let cache_dir = streamlib::core::streamlib_home::get_cached_package_dir(&entry.cache_dir);
    if cache_dir.exists() {
        std::fs::remove_dir_all(&cache_dir)
            .with_context(|| format!("Failed to remove cache dir {}", cache_dir.display()))?;
        println!("Removed cache: {}", cache_dir.display());
    }

    // Save manifest
    manifest
        .save()
        .map_err(|e| anyhow::anyhow!("Failed to save packages manifest: {}", e))?;

    println!("Removed package '{}'.", name);

    Ok(())
}
