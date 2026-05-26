// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{Error, Result};

/// Extract a .slpkg ZIP archive to the package cache.
/// Cache key is `{name}-{version}` from the embedded streamlib.yaml.
/// Always overwrites on load.
pub fn extract_slpkg_to_cache(slpkg_path: &std::path::Path) -> Result<std::path::PathBuf> {
    use crate::core::config::ProjectConfig;

    let slpkg_bytes = std::fs::read(slpkg_path).map_err(|e| {
        Error::Configuration(format!("Failed to read {}: {}", slpkg_path.display(), e))
    })?;

    let cursor = std::io::Cursor::new(&slpkg_bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| Error::Configuration(format!("Failed to open .slpkg archive: {}", e)))?;

    // Read streamlib.yaml from archive to get name + version
    let manifest_yaml = {
        let mut manifest_file = archive.by_name(ProjectConfig::FILE_NAME).map_err(|e| {
            Error::Configuration(format!(
                ".slpkg archive missing {}: {}",
                ProjectConfig::FILE_NAME,
                e
            ))
        })?;
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut manifest_file, &mut contents)
            .map_err(|e| Error::Configuration(format!("Failed to read manifest: {}", e)))?;
        contents
    };

    let config: ProjectConfig = serde_yaml::from_str(&manifest_yaml)
        .map_err(|e| Error::Configuration(format!("Failed to parse manifest: {}", e)))?;

    let package = config.package.as_ref().ok_or_else(|| {
        Error::Configuration("streamlib.yaml missing [package] section".to_string())
    })?;

    let cache_key = format!("{}-{}", package.name, package.version);
    let cache_dir = crate::core::streamlib_home::get_cached_package_dir(&cache_key);

    // Always overwrite
    if cache_dir.exists() {
        std::fs::remove_dir_all(&cache_dir)
            .map_err(|e| Error::Configuration(format!("Failed to clear cache dir: {}", e)))?;
    }

    tracing::info!(
        "Extracting {} to {}",
        slpkg_path.display(),
        cache_dir.display()
    );
    std::fs::create_dir_all(&cache_dir)
        .map_err(|e| Error::Configuration(format!("Failed to create cache dir: {}", e)))?;

    // Re-open archive (cursor consumed by manifest read)
    let cursor = std::io::Cursor::new(&slpkg_bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| {
        Error::Configuration(format!("Failed to re-open .slpkg archive: {}", e))
    })?;

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| {
            Error::Configuration(format!("Failed to read archive entry: {}", e))
        })?;

        let file_name = file.name().to_string();

        // Security: reject path traversal
        if file_name.contains("..") || file_name.starts_with('/') {
            return Err(Error::Configuration(format!(
                "Invalid path in .slpkg archive: {}",
                file_name
            )));
        }

        let output_path = cache_dir.join(&file_name);

        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::Configuration(format!("Failed to create directory: {}", e))
            })?;
        }

        let mut output_file = std::fs::File::create(&output_path).map_err(|e| {
            Error::Configuration(format!("Failed to create {}: {}", output_path.display(), e))
        })?;

        std::io::copy(&mut file, &mut output_file).map_err(|e| {
            Error::Configuration(format!("Failed to extract {}: {}", file_name, e))
        })?;
    }

    Ok(cache_dir)
}
