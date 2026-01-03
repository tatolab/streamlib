// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Virtual environment manager for Python processors.
//!
//! Handles creation, dependency installation, and cleanup of isolated
//! Python virtual environments using `uv`.

use std::path::{Path, PathBuf};
use std::process::Command;
use streamlib::core::{ProcessorUniqueId, RuntimeUniqueId};
use streamlib::{Result, StreamError};

/// Manages isolated Python virtual environments for processor instances.
pub struct VenvManager {
    /// Path to this processor's venv
    venv_path: PathBuf,
    /// Shared UV cache directory
    uv_cache_dir: PathBuf,
    /// Directory containing pre-built streamlib-python wheel
    wheels_dir: PathBuf,
    /// Whether the venv has been created
    initialized: bool,
}

impl VenvManager {
    /// Create a new VenvManager for a processor instance.
    ///
    /// Uses STREAMLIB_HOME-based paths:
    /// - Venv: ~/.streamlib/runtimes/{runtime_id}/processors/{processor_id}/venv
    /// - UV cache: ~/.streamlib/cache/uv (shared across all processors)
    /// - Wheel: ~/.streamlib/cache/wheels/streamlib-*.whl (pre-built by init hook)
    pub fn new(runtime_id: &RuntimeUniqueId, processor_id: &ProcessorUniqueId) -> Result<Self> {
        let streamlib_home = streamlib::core::get_streamlib_home();

        let venv_path = streamlib_home
            .join("runtimes")
            .join(runtime_id.as_str())
            .join("processors")
            .join(processor_id.as_str())
            .join("venv");

        let uv_cache_dir = streamlib_home.join("cache").join("uv");
        let wheels_dir = streamlib_home.join("cache").join("wheels");

        Ok(Self {
            venv_path,
            uv_cache_dir,
            wheels_dir,
            initialized: false,
        })
    }

    /// Find the streamlib wheel in the cache directory.
    fn find_wheel(&self) -> Option<PathBuf> {
        let entries = std::fs::read_dir(&self.wheels_dir).ok()?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("streamlib-") && name.ends_with(".whl") {
                return Some(entry.path());
            }
        }
        None
    }

    /// Run a uv command with shared UV_CACHE_DIR.
    fn run_uv(&self, args: &[&str]) -> Result<std::process::Output> {
        Command::new("uv")
            .args(args)
            .env("UV_CACHE_DIR", &self.uv_cache_dir)
            .output()
            .map_err(|e| StreamError::Runtime(format!("Failed to run uv: {}", e)))
    }

    /// Ensure a venv exists for this processor instance.
    /// Creates the venv and installs dependencies if needed.
    pub fn ensure_venv(&mut self, project_path: &Path) -> Result<PathBuf> {
        // Create parent directories
        if let Some(parent) = self.venv_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                StreamError::Configuration(format!(
                    "Failed to create venv directory '{}': {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        // Create the venv
        self.create_venv(&self.venv_path.clone())?;

        // Install project dependencies
        self.install_project_deps(&self.venv_path.clone(), project_path)?;

        // Inject streamlib-python from pre-built wheel
        self.inject_streamlib(&self.venv_path.clone())?;

        self.initialized = true;
        Ok(self.venv_path.clone())
    }

    /// Create a new virtual environment.
    fn create_venv(&self, venv_path: &Path) -> Result<()> {
        tracing::info!("VenvManager: Creating venv at '{}'", venv_path.display());

        let output = self.run_uv(&["venv", venv_path.to_str().unwrap(), "--python", "3.12"])?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(StreamError::Runtime(format!(
                "Failed to create venv: {}",
                stderr
            )));
        }

        tracing::debug!("VenvManager: Venv created successfully");
        Ok(())
    }

    /// Install project dependencies from pyproject.toml.
    fn install_project_deps(&self, venv_path: &Path, project_path: &Path) -> Result<()> {
        let pyproject_path = project_path.join("pyproject.toml");

        // Check if pyproject.toml exists
        if !pyproject_path.exists() {
            tracing::debug!(
                "VenvManager: No pyproject.toml found at '{}', skipping dependency installation",
                pyproject_path.display()
            );
            return Ok(());
        }

        tracing::info!(
            "VenvManager: Installing project dependencies from '{}'",
            pyproject_path.display()
        );

        // Use uv pip install with the venv's Python
        let python_path = self.get_python_path(venv_path);
        let python_str = python_path.to_str().unwrap();
        let project_str = project_path.to_str().unwrap();

        let output = self.run_uv(&["pip", "install", "-e", project_str, "--python", python_str])?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(StreamError::Runtime(format!(
                "Failed to install project dependencies: {}",
                stderr
            )));
        }

        tracing::debug!("VenvManager: Project dependencies installed");
        Ok(())
    }

    /// Inject streamlib-python into the venv from pre-built wheel.
    fn inject_streamlib(&self, venv_path: &Path) -> Result<()> {
        // Find wheel (should have been built by PythonRuntimeInitHook)
        let wheel_path = self.find_wheel().ok_or_else(|| {
            StreamError::Configuration(format!(
                "streamlib-python wheel not found in '{}'. \
                 The wheel should be pre-built by the Python init hook.",
                self.wheels_dir.display()
            ))
        })?;

        tracing::info!(
            "VenvManager: Installing streamlib-python from wheel '{}'",
            wheel_path.display()
        );

        let python_path = self.get_python_path(venv_path);
        let python_str = python_path.to_str().unwrap();
        let wheel_str = wheel_path.to_str().unwrap();

        let output = self.run_uv(&["pip", "install", wheel_str, "--python", python_str])?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(StreamError::Runtime(format!(
                "Failed to install streamlib-python from wheel: {}",
                stderr
            )));
        }

        tracing::debug!("VenvManager: streamlib-python installed from wheel");
        Ok(())
    }

    /// Get the path to Python executable in the venv.
    fn get_python_path(&self, venv_path: &Path) -> PathBuf {
        #[cfg(windows)]
        {
            venv_path.join("Scripts").join("python.exe")
        }
        #[cfg(not(windows))]
        {
            venv_path.join("bin").join("python")
        }
    }

    /// Get the site-packages directory for the venv.
    pub fn get_site_packages(&self, venv_path: &Path) -> Result<PathBuf> {
        // Find the site-packages directory
        let lib_path = venv_path.join("lib");

        if !lib_path.exists() {
            return Err(StreamError::Runtime(format!(
                "Venv lib directory not found: {}",
                lib_path.display()
            )));
        }

        // Find python3.X directory
        for entry in std::fs::read_dir(&lib_path)
            .map_err(|e| StreamError::Runtime(format!("Failed to read venv lib: {}", e)))?
        {
            let entry =
                entry.map_err(|e| StreamError::Runtime(format!("Failed to read entry: {}", e)))?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("python") {
                let site_packages = entry.path().join("site-packages");
                if site_packages.exists() {
                    return Ok(site_packages);
                }
            }
        }

        Err(StreamError::Runtime(format!(
            "Could not find site-packages in venv: {}",
            venv_path.display()
        )))
    }

    /// Clean up the venv (call on teardown).
    pub fn cleanup(&mut self) -> Result<()> {
        if !self.initialized {
            return Ok(());
        }

        if self.venv_path.exists() {
            tracing::info!(
                "VenvManager: Cleaning up venv at '{}'",
                self.venv_path.display()
            );

            std::fs::remove_dir_all(&self.venv_path).map_err(|e| {
                StreamError::Runtime(format!(
                    "Failed to remove venv '{}': {}",
                    self.venv_path.display(),
                    e
                ))
            })?;

            tracing::debug!("VenvManager: Venv removed");
        }

        self.initialized = false;
        Ok(())
    }

    /// Get the venv path.
    pub fn venv_path(&self) -> &Path {
        &self.venv_path
    }
}

impl Drop for VenvManager {
    fn drop(&mut self) {
        // Best-effort cleanup on drop
        if let Err(e) = self.cleanup() {
            tracing::warn!("VenvManager: Cleanup failed during drop: {}", e);
        }
    }
}
