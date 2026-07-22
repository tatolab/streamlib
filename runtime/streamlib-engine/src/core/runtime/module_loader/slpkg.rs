// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

use streamlib_idents::PackageRef;

use crate::core::streamlib_home::installed_package_slot_dir;
use crate::core::{Error, Result};

/// Extract a .slpkg ZIP archive to the co-located `streamlib_modules/@org/name`
/// slot derived from the embedded `streamlib.yaml`'s `@org/name`. `app_modules_root`
/// is threaded to the slot deriver (the app whose `streamlib_modules/` a future
/// relocation prefers). Always overwrites on load.
pub fn extract_slpkg_to_cache(
    slpkg_path: &std::path::Path,
    app_modules_root: Option<&std::path::Path>,
) -> Result<std::path::PathBuf> {
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

    let pkg_ref = PackageRef::new(package.org.clone(), package.name.clone());
    let cache_dir = installed_package_slot_dir(app_modules_root, &pkg_ref);

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
