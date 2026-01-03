// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Runtime initialization hook for Python support.
//!
//! This hook runs at StreamRuntime::new() to build/cache the streamlib-python
//! wheel before any processors start.

use std::path::Path;
use streamlib::core::{RuntimeInitHook, RuntimeInitHookRegistration};
use streamlib::{Result, StreamError};

use crate::wheel_cache::{is_dev_mode, WheelCache};

/// Python runtime initialization hook.
///
/// When linked with streamlib-python, this hook is automatically registered
/// via inventory and runs at StreamRuntime::new() time.
///
/// In dev mode: Builds the streamlib-python wheel if source has changed.
/// In production: Verifies the pre-built wheel exists.
#[derive(Default)]
pub struct PythonRuntimeInitHook;

impl RuntimeInitHook for PythonRuntimeInitHook {
    fn name(&self) -> &'static str {
        "Python"
    }

    fn on_runtime_init(&self, streamlib_home: &Path) -> Result<()> {
        // Check if uv is available (required for venv management)
        check_uv_available()?;

        let wheels_dir = streamlib_home.join("cache").join("wheels");
        let wheel_cache = WheelCache::new(wheels_dir);

        if is_dev_mode() {
            // Dev mode: build wheel if source changed
            tracing::info!("PythonRuntimeInitHook: Dev mode - ensuring wheel is built");
            wheel_cache.ensure_wheel()?;
        } else {
            // Production: verify wheel exists
            tracing::info!("PythonRuntimeInitHook: Production mode - verifying wheel exists");
            wheel_cache.verify_wheel_exists()?;
        }

        // Ensure shared UV cache directory exists
        let uv_cache_dir = streamlib_home.join("cache").join("uv");
        std::fs::create_dir_all(&uv_cache_dir).map_err(|e| {
            StreamError::Configuration(format!(
                "Failed to create UV cache directory '{}': {}",
                uv_cache_dir.display(),
                e
            ))
        })?;

        tracing::info!("PythonRuntimeInitHook: Initialization complete");
        Ok(())
    }
}

/// Check if uv is available on the system.
fn check_uv_available() -> Result<()> {
    use std::process::Command;

    let output = Command::new("uv").arg("--version").output().map_err(|e| {
        StreamError::Configuration(format!(
            "uv is not installed or not in PATH. Please install uv: https://docs.astral.sh/uv/\nError: {}",
            e
        ))
    })?;

    if !output.status.success() {
        return Err(StreamError::Configuration(
            "uv is installed but returned an error. Please check your uv installation.".into(),
        ));
    }

    let version = String::from_utf8_lossy(&output.stdout);
    tracing::debug!("PythonRuntimeInitHook: Found {}", version.trim());

    Ok(())
}

// Register the hook via inventory.
// This runs at link time when this crate is linked into an executable.
inventory::submit! {
    RuntimeInitHookRegistration::new::<PythonRuntimeInitHook>()
}
