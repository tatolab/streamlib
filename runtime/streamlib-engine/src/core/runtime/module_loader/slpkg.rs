// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use crate::core::{Error, Result};

/// Extract a .slpkg ZIP archive to the package cache.
/// The cache slot is keyed by the embedded streamlib.yaml's `@org/name`
/// identity + version under the running host's toolchain context (triple,
/// plugin-ABI, profile) — the same slot the build orchestrator stages into.
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

    let cache_dir = crate::core::streamlib_home::get_cached_package_dir_for_slot(
        package.org.as_str(),
        package.name.as_str(),
        package.version,
        &super::processor_registration::host_package_cache_slot_context(),
    );

    // Reuse the already-read bytes — extract into the derived cache slot.
    extract_zip_bytes_to_dir(&slpkg_bytes, &cache_dir, slpkg_path)?;
    Ok(cache_dir)
}

/// Extract every entry of the in-memory `.slpkg` ZIP `slpkg_bytes` into
/// `dest_dir` (cleared first, always-overwrite), rejecting path-traversal
/// entries. Delegates to the one canonical extractor in
/// [`streamlib_idents::archive`]. `source_label` names the archive in
/// `tracing` / error text only.
fn extract_zip_bytes_to_dir(
    slpkg_bytes: &[u8],
    dest_dir: &std::path::Path,
    source_label: &std::path::Path,
) -> Result<()> {
    streamlib_idents::archive::extract_zip_bytes_to_dir(
        slpkg_bytes,
        dest_dir,
        &source_label.display().to_string(),
    )
    .map_err(|e| Error::Configuration(format!("Failed to extract .slpkg archive: {e}")))
}
