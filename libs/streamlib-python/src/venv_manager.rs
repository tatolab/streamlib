// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Virtual environment manager for Python processors.
//!
//! Handles creation, dependency installation, and cleanup of isolated
//! Python virtual environments using `uv`.

use std::path::{Path, PathBuf};
use std::process::Command;
use streamlib::{Result, StreamError};

/// Manages isolated Python virtual environments for processor instances.
pub struct VenvManager {
    /// Base cache directory for venvs (e.g., ~/.cache/streamlib/python-venvs/)
    cache_dir: PathBuf,
    /// Unique identifier for this processor instance
    instance_id: String,
    /// Path to the created venv (set after ensure_venv)
    venv_path: Option<PathBuf>,
}

impl VenvManager {
    /// Create a new VenvManager for a processor instance.
    pub fn new(instance_id: &str) -> Result<Self> {
        let cache_dir = Self::get_cache_dir()?;

        Ok(Self {
            cache_dir,
            instance_id: instance_id.to_string(),
            venv_path: None,
        })
    }

    /// Get the cache directory for venvs.
    fn get_cache_dir() -> Result<PathBuf> {
        let cache_base = dirs::cache_dir().ok_or_else(|| {
            StreamError::Configuration("Could not determine cache directory".into())
        })?;
        Ok(cache_base.join("streamlib").join("python-venvs"))
    }

    /// Check if `uv` is available on the system.
    pub fn check_uv_available() -> Result<()> {
        let output = Command::new("uv")
            .arg("--version")
            .output()
            .map_err(|e| {
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
        tracing::debug!("VenvManager: Found {}", version.trim());

        Ok(())
    }

    /// Ensure a venv exists for this processor instance.
    /// Creates the venv and installs dependencies if needed.
    pub fn ensure_venv(&mut self, project_path: &Path) -> Result<PathBuf> {
        // Check uv is available
        Self::check_uv_available()?;

        // Create venv path based on instance ID
        let venv_path = self.cache_dir.join(&self.instance_id);

        // Create parent directories
        std::fs::create_dir_all(&self.cache_dir).map_err(|e| {
            StreamError::Configuration(format!(
                "Failed to create venv cache directory '{}': {}",
                self.cache_dir.display(),
                e
            ))
        })?;

        // Create the venv
        self.create_venv(&venv_path)?;

        // Install project dependencies
        self.install_project_deps(&venv_path, project_path)?;

        // Inject streamlib-python
        self.inject_streamlib(&venv_path)?;

        self.venv_path = Some(venv_path.clone());
        Ok(venv_path)
    }

    /// Create a new virtual environment.
    fn create_venv(&self, venv_path: &Path) -> Result<()> {
        tracing::info!("VenvManager: Creating venv at '{}'", venv_path.display());

        let output = Command::new("uv")
            .args(["venv", venv_path.to_str().unwrap(), "--python", "3.12"])
            .output()
            .map_err(|e| StreamError::Runtime(format!("Failed to run 'uv venv': {}", e)))?;

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

        let output = Command::new("uv")
            .args([
                "pip",
                "install",
                "-e",
                project_path.to_str().unwrap(),
                "--python",
                python_path.to_str().unwrap(),
            ])
            .output()
            .map_err(|e| StreamError::Runtime(format!("Failed to run 'uv pip install': {}", e)))?;

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

    /// Inject streamlib-python into the venv.
    fn inject_streamlib(&self, venv_path: &Path) -> Result<()> {
        tracing::info!("VenvManager: Installing streamlib-python");

        let python_path = self.get_python_path(venv_path);

        // Try to find streamlib-python in the workspace (development mode)
        let streamlib_python_path = self.find_streamlib_python_path();

        let output = if let Some(lib_path) = streamlib_python_path {
            // Development mode: editable install from source
            tracing::debug!(
                "VenvManager: Using editable install from '{}'",
                lib_path.display()
            );

            Command::new("uv")
                .args([
                    "pip",
                    "install",
                    "-e",
                    lib_path.to_str().unwrap(),
                    "--python",
                    python_path.to_str().unwrap(),
                ])
                .output()
                .map_err(|e| {
                    StreamError::Runtime(format!("Failed to run 'uv pip install': {}", e))
                })?
        } else {
            // Production mode: install from PyPI (future)
            tracing::debug!("VenvManager: Installing streamlib from PyPI");

            Command::new("uv")
                .args([
                    "pip",
                    "install",
                    "streamlib",
                    "--python",
                    python_path.to_str().unwrap(),
                ])
                .output()
                .map_err(|e| {
                    StreamError::Runtime(format!("Failed to run 'uv pip install': {}", e))
                })?
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(StreamError::Runtime(format!(
                "Failed to install streamlib-python: {}",
                stderr
            )));
        }

        tracing::debug!("VenvManager: streamlib-python installed");
        Ok(())
    }

    /// Find the path to streamlib-python source (for development mode).
    fn find_streamlib_python_path(&self) -> Option<PathBuf> {
        // Try relative to CARGO_MANIFEST_DIR if available
        if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
            // From examples: ../../libs/streamlib-python
            let path = PathBuf::from(&manifest_dir).join("../../libs/streamlib-python");
            if path.join("pyproject.toml").exists() {
                return Some(path.canonicalize().unwrap_or(path));
            }

            // From within libs: ../streamlib-python
            let path = PathBuf::from(&manifest_dir).join("../streamlib-python");
            if path.join("pyproject.toml").exists() {
                return Some(path.canonicalize().unwrap_or(path));
            }
        }

        // Try relative to current exe
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(exe_dir) = exe_path.parent() {
                // Walk up looking for libs/streamlib-python
                let mut current = exe_dir.to_path_buf();
                for _ in 0..10 {
                    let candidate = current.join("libs/streamlib-python");
                    if candidate.join("pyproject.toml").exists() {
                        return Some(candidate);
                    }
                    if !current.pop() {
                        break;
                    }
                }
            }
        }

        None
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
        if let Some(ref venv_path) = self.venv_path {
            if venv_path.exists() {
                tracing::info!("VenvManager: Cleaning up venv at '{}'", venv_path.display());

                std::fs::remove_dir_all(venv_path).map_err(|e| {
                    StreamError::Runtime(format!(
                        "Failed to remove venv '{}': {}",
                        venv_path.display(),
                        e
                    ))
                })?;

                tracing::debug!("VenvManager: Venv removed");
            }
        }

        self.venv_path = None;
        Ok(())
    }

    /// Get the venv path if created.
    pub fn venv_path(&self) -> Option<&Path> {
        self.venv_path.as_deref()
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
