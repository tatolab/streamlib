// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Dynamic plugin loading for StreamLib CLI.
//!
//! Loads processor plugins from dynamic libraries (.dylib/.so/.dll) at runtime.
//! Plugins use the same `#[streamlib::processor]` macro as built-in processors -
//! the only difference is registration mechanism (dynamic vs inventory).

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use libloading::Library;
use streamlib::core::processors::PROCESSOR_REGISTRY;
use streamlib_plugin_abi::{PluginDeclaration, STREAMLIB_ABI_VERSION};

/// Plugin loader that manages dynamic library loading and processor registration.
///
/// Keeps loaded libraries alive for the duration of the runtime to prevent
/// use-after-free when processor code is called.
pub struct PluginLoader {
    /// Loaded libraries - must remain alive while processors are in use.
    loaded_libraries: Vec<Library>,
}

impl PluginLoader {
    pub fn new() -> Self {
        Self {
            loaded_libraries: Vec::new(),
        }
    }

    /// Load a plugin from a dynamic library file.
    ///
    /// The plugin must export a `STREAMLIB_PLUGIN` symbol of type [`PluginDeclaration`].
    /// The plugin's registration function is called immediately, registering all
    /// processors with the global `PROCESSOR_REGISTRY`.
    ///
    /// # Arguments
    /// * `path` - Path to the .dylib/.so/.dll file
    ///
    /// # Returns
    /// Number of processors registered from the plugin.
    ///
    /// # Errors
    /// - Library failed to load
    /// - Missing `STREAMLIB_PLUGIN` symbol
    /// - ABI version mismatch
    pub fn load_plugin(&mut self, path: &Path) -> Result<usize> {
        // Load the dynamic library
        let lib = unsafe {
            Library::new(path)
                .with_context(|| format!("Failed to load plugin library: {}", path.display()))?
        };

        // Get the plugin declaration symbol
        let decl: &PluginDeclaration = unsafe {
            let symbol = lib
                .get::<*const PluginDeclaration>(b"STREAMLIB_PLUGIN\0")
                .with_context(|| {
                    format!(
                        "Plugin '{}' missing STREAMLIB_PLUGIN symbol. \
                         Ensure the plugin uses the export_plugin! macro.",
                        path.display()
                    )
                })?;
            &**symbol
        };

        // Verify ABI version
        if decl.abi_version != STREAMLIB_ABI_VERSION {
            return Err(anyhow!(
                "ABI version mismatch for '{}': plugin has v{}, CLI expects v{}. \
                 Rebuild the plugin with a compatible streamlib-plugin-abi version.",
                path.display(),
                decl.abi_version,
                STREAMLIB_ABI_VERSION
            ));
        }

        // Count processors before registration
        let before_count = PROCESSOR_REGISTRY.list_registered().len();

        // Call the plugin's registration function with host's registry
        // This ensures processors register with the host's registry, not a duplicate
        (decl.register)(&*PROCESSOR_REGISTRY);

        // Count processors after registration
        let after_count = PROCESSOR_REGISTRY.list_registered().len();
        let registered_count = after_count - before_count;

        // Keep library alive - dropping it would unload the code
        self.loaded_libraries.push(lib);

        Ok(registered_count)
    }

    /// Load all plugins from a directory.
    ///
    /// Loads all files with plugin extensions (.dylib on macOS, .so on Linux,
    /// .dll on Windows). Failures for individual plugins are logged but don't
    /// stop loading of other plugins.
    ///
    /// # Arguments
    /// * `dir` - Directory containing plugin files
    ///
    /// # Returns
    /// Total number of processors registered from all successfully loaded plugins.
    pub fn load_plugin_dir(&mut self, dir: &Path) -> Result<usize> {
        let mut total_registered = 0;

        let entries = std::fs::read_dir(dir)
            .with_context(|| format!("Failed to read plugin directory: {}", dir.display()))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if is_plugin_library(&path) {
                match self.load_plugin(&path) {
                    Ok(count) => {
                        tracing::info!(
                            "Loaded plugin '{}': {} processor(s) registered",
                            path.display(),
                            count
                        );
                        total_registered += count;
                    }
                    Err(e) => {
                        tracing::warn!("Failed to load plugin '{}': {}", path.display(), e);
                    }
                }
            }
        }

        Ok(total_registered)
    }

    /// Returns the number of loaded plugin libraries.
    #[allow(dead_code)]
    pub fn loaded_count(&self) -> usize {
        self.loaded_libraries.len()
    }
}

impl Default for PluginLoader {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a file is a plugin library based on its extension.
fn is_plugin_library(path: &Path) -> bool {
    let extension = path.extension().and_then(|e| e.to_str());
    match extension {
        Some("dylib") => cfg!(target_os = "macos"),
        Some("so") => cfg!(target_os = "linux"),
        Some("dll") => cfg!(target_os = "windows"),
        _ => false,
    }
}
