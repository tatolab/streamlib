// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Plugin loader for dynamically loading Rust processor plugins.

use std::path::Path;

use libloading::Library;
use streamlib::core::processors::PROCESSOR_REGISTRY;
use streamlib::core::StreamError;
use streamlib::Result;
use streamlib_plugin_abi::{PluginDeclaration, STREAMLIB_ABI_VERSION};

/// Plugin loader that manages dynamic library loading and processor registration.
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
    pub fn load_plugin(&mut self, path: &Path) -> Result<usize> {
        // Load the dynamic library
        let lib = unsafe {
            Library::new(path).map_err(|e| {
                StreamError::Configuration(format!(
                    "Failed to load plugin library '{}': {}",
                    path.display(),
                    e
                ))
            })?
        };

        // Get the plugin declaration symbol
        let decl: &PluginDeclaration = unsafe {
            let symbol = lib
                .get::<*const PluginDeclaration>(b"STREAMLIB_PLUGIN\0")
                .map_err(|e| {
                    StreamError::Configuration(format!(
                        "Plugin '{}' missing STREAMLIB_PLUGIN symbol: {}",
                        path.display(),
                        e
                    ))
                })?;
            &**symbol
        };

        // Verify ABI version
        if decl.abi_version != STREAMLIB_ABI_VERSION {
            return Err(StreamError::Configuration(format!(
                "ABI version mismatch for '{}': plugin has v{}, expected v{}",
                path.display(),
                decl.abi_version,
                STREAMLIB_ABI_VERSION
            )));
        }

        // Count processors before registration
        let before_count = PROCESSOR_REGISTRY.list_registered().len();

        // Call the plugin's registration function with host's registry
        (decl.register)(&PROCESSOR_REGISTRY);

        // Count processors after registration
        let after_count = PROCESSOR_REGISTRY.list_registered().len();
        let registered_count = after_count - before_count;

        // Keep library alive
        self.loaded_libraries.push(lib);

        Ok(registered_count)
    }
}
