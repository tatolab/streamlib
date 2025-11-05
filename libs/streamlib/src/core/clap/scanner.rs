
use clack_host::bundle::PluginBundle;

use crate::core::{Result, StreamError};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ClapPluginInfo {
    pub path: PathBuf,

    pub id: String,

    pub name: String,

    pub vendor: String,

    pub version: String,

    pub description: String,

    pub features: Vec<String>,
}

pub struct ClapScanner;

impl ClapScanner {
    pub fn scan_system_plugins() -> Result<Vec<ClapPluginInfo>> {
        let paths = Self::get_system_paths();
        let mut all_plugins = Vec::new();

        for path in paths {
            match Self::scan_directory(&path) {
                Ok(plugins) => all_plugins.extend(plugins),
                Err(e) => {
                    tracing::debug!("Failed to scan directory {:?}: {}", path, e);
                    // Continue scanning other directories
                }
            }
        }

        Ok(all_plugins)
    }

    fn get_system_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();

        #[cfg(target_os = "macos")]
        {
            // macOS paths
            if let Some(home) = std::env::var_os("HOME") {
                paths.push(PathBuf::from(home).join("Library/Audio/Plug-Ins/CLAP"));
            }
            paths.push(PathBuf::from("/Library/Audio/Plug-Ins/CLAP"));
        }

        #[cfg(target_os = "linux")]
        {
            // Linux paths
            if let Some(home) = std::env::var_os("HOME") {
                paths.push(PathBuf::from(home).join(".clap"));
            }
            paths.push(PathBuf::from("/usr/lib/clap"));
            paths.push(PathBuf::from("/usr/local/lib/clap"));
        }

        #[cfg(target_os = "windows")]
        {
            // Windows paths
            if let Some(common_files) = std::env::var_os("CommonProgramFiles") {
                paths.push(PathBuf::from(common_files).join("CLAP"));
            }
            if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
                paths.push(PathBuf::from(local_app_data).join("Programs/Common/CLAP"));
            }
        }

        paths
    }

    pub fn scan_directory<P: AsRef<Path>>(path: P) -> Result<Vec<ClapPluginInfo>> {
        let path = path.as_ref();

        if !path.exists() {
            return Ok(Vec::new());
        }

        let mut plugins = Vec::new();

        for entry in std::fs::read_dir(path)
            .map_err(|e| StreamError::Configuration(format!("Failed to read directory {:?}: {}", path, e)))?
        {
            let entry = entry
                .map_err(|e| StreamError::Configuration(format!("Failed to read entry: {}", e)))?;
            let entry_path = entry.path();

            // Check if it's a CLAP bundle
            if Self::is_clap_bundle(&entry_path) {
                match Self::scan_plugin_bundle(&entry_path) {
                    Ok(bundle_plugins) => plugins.extend(bundle_plugins),
                    Err(e) => {
                        tracing::debug!("Failed to scan bundle {:?}: {}", entry_path, e);
                        // Continue with other plugins
                    }
                }
            }
        }

        Ok(plugins)
    }

    fn is_clap_bundle(path: &Path) -> bool {
        // CLAP bundles end with .clap extension
        path.extension().and_then(|s| s.to_str()) == Some("clap")
    }

    fn scan_plugin_bundle(path: &Path) -> Result<Vec<ClapPluginInfo>> {
        // Get the actual binary path within the bundle
        let binary_path = Self::get_bundle_binary_path(path)?;

        // Load the plugin bundle
        // SAFETY: Loading CLAP plugins is inherently unsafe as it loads dynamic libraries
        let bundle = unsafe {
            PluginBundle::load(&binary_path)
                .map_err(|e| StreamError::Configuration(format!("Failed to load bundle {:?}: {:?}", path, e)))?
        };

        // Get plugin factory
        let factory = bundle.get_plugin_factory()
            .ok_or_else(|| StreamError::Configuration("Plugin has no factory".into()))?;

        // Iterate through all plugins in the bundle
        let mut plugins = Vec::new();

        for desc in factory.plugin_descriptors() {
            plugins.push(ClapPluginInfo {
                path: path.to_path_buf(),
                id: desc.id()
                    .and_then(|id| id.to_str().ok())
                    .unwrap_or("unknown")
                    .to_string(),
                name: desc.name()
                    .and_then(|n| n.to_str().ok())
                    .unwrap_or("Unknown")
                    .to_string(),
                vendor: desc.vendor()
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("Unknown")
                    .to_string(),
                version: desc.version()
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("Unknown")
                    .to_string(),
                description: desc.description()
                    .and_then(|d| d.to_str().ok())
                    .unwrap_or("")
                    .to_string(),
                // Features are optional metadata - leave empty for now
                // TODO: Parse features() properly when needed
                features: Vec::new(),
            });
        }

        Ok(plugins)
    }

    pub fn get_bundle_binary_path(bundle_path: &Path) -> Result<PathBuf> {
        #[cfg(target_os = "macos")]
        {
            // If the path is already a file (binary), return it as-is
            if bundle_path.is_file() {
                return Ok(bundle_path.to_path_buf());
            }

            // If the path doesn't end with .clap, assume it's already a binary path
            // (even if it doesn't exist yet - let the plugin loader handle the error)
            if bundle_path.extension().and_then(|s| s.to_str()) != Some("clap") {
                return Ok(bundle_path.to_path_buf());
            }

            // It's a bundle directory - construct the binary path
            let binary_name = bundle_path
                .file_stem()
                .ok_or_else(|| StreamError::Configuration("Invalid bundle path".into()))?;

            let binary_path = bundle_path
                .join("Contents")
                .join("MacOS")
                .join(binary_name);

            if binary_path.exists() {
                Ok(binary_path)
            } else {
                Err(StreamError::Configuration(
                    format!("Binary not found in bundle: {:?}", binary_path)
                ))
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            // On Linux/Windows, the .clap file is the binary itself
            Ok(bundle_path.to_path_buf())
        }
    }
}
