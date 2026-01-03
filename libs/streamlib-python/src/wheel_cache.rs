// Copyright (c) 2025 Jonathan Fontanez
// SPDX-License-Identifier: BUSL-1.1

//! Wheel cache for streamlib-python.
//!
//! Builds the wheel once and caches it based on source hash.
//! Avoids rebuilding for every processor instance.

use std::path::{Path, PathBuf};
use std::process::Command;
use streamlib::{Result, StreamError};

/// Manages the streamlib-python wheel cache.
pub struct WheelCache {
    /// Directory for wheel storage (e.g., ~/.streamlib/cache/wheels/)
    wheels_dir: PathBuf,
}

impl WheelCache {
    pub fn new(wheels_dir: PathBuf) -> Self {
        Self { wheels_dir }
    }

    /// Ensure wheel exists and is up-to-date. Returns path to wheel.
    pub fn ensure_wheel(&self) -> Result<PathBuf> {
        tracing::trace!("WheelCache: ensure_wheel() called");
        tracing::trace!(
            "WheelCache: Wheels directory: {}",
            self.wheels_dir.display()
        );

        std::fs::create_dir_all(&self.wheels_dir).map_err(|e| {
            StreamError::Configuration(format!(
                "Failed to create wheels directory '{}': {}",
                self.wheels_dir.display(),
                e
            ))
        })?;

        let hash_path = self.wheels_dir.join("streamlib.hash");
        let current_hash = self.compute_source_hash()?;

        // Check if we need to rebuild
        let existing_wheel = self.find_wheel();
        tracing::trace!(
            "WheelCache: Existing wheel: {}",
            existing_wheel
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "None".to_string())
        );
        tracing::trace!("WheelCache: Hash file exists: {}", hash_path.exists());

        let (needs_rebuild, reason) = match (&existing_wheel, hash_path.exists()) {
            (None, _) => (true, "no existing wheel found"),
            (Some(_), false) => (true, "no hash file found"),
            (Some(_), true) => {
                let stored_hash = std::fs::read_to_string(&hash_path).unwrap_or_default();
                let stored_hash = stored_hash.trim();
                tracing::trace!("WheelCache: Stored hash:  {}", stored_hash);
                tracing::trace!("WheelCache: Current hash: {}", current_hash);
                if stored_hash != current_hash {
                    (true, "hash mismatch (source changed)")
                } else {
                    (false, "hash match (source unchanged)")
                }
            }
        };

        tracing::trace!(
            "WheelCache: Rebuild decision: {} (reason: {})",
            if needs_rebuild {
                "REBUILD"
            } else {
                "USE CACHE"
            },
            reason
        );

        if needs_rebuild {
            // Remove old wheel if exists
            if let Some(ref old_wheel) = existing_wheel {
                tracing::trace!("WheelCache: Removing old wheel: {}", old_wheel.display());
                let _ = std::fs::remove_file(old_wheel);
            }

            tracing::info!(
                "WheelCache: Building streamlib-python wheel (hash: {})",
                current_hash
            );
            self.build_wheel()?;
            std::fs::write(&hash_path, &current_hash)
                .map_err(|e| StreamError::Runtime(format!("Failed to write hash file: {}", e)))?;
            tracing::trace!("WheelCache: Hash file updated");
        } else {
            tracing::info!("WheelCache: Using cached streamlib-python wheel");
        }

        // Return the actual wheel path
        let wheel_path = self
            .find_wheel()
            .ok_or_else(|| StreamError::Runtime("Wheel not found after build".into()))?;
        tracing::trace!("WheelCache: Returning wheel path: {}", wheel_path.display());
        Ok(wheel_path)
    }

    /// Find the streamlib wheel in the cache directory.
    /// Matches pattern: streamlib-*.whl
    pub fn find_wheel(&self) -> Option<PathBuf> {
        let entries = std::fs::read_dir(&self.wheels_dir).ok()?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("streamlib-") && name.ends_with(".whl") {
                return Some(entry.path());
            }
        }
        None
    }

    /// Verify wheel exists (production mode).
    pub fn verify_wheel_exists(&self) -> Result<PathBuf> {
        self.find_wheel().ok_or_else(|| {
            StreamError::Configuration(
                "streamlib wheel not found. In production, the wheel must be pre-built.".into(),
            )
        })
    }

    fn compute_source_hash(&self) -> Result<String> {
        let source_path = find_streamlib_python_source().ok_or_else(|| {
            StreamError::Configuration("Cannot find streamlib-python source".into())
        })?;

        tracing::trace!(
            "WheelCache: Computing source hash for '{}'",
            source_path.display()
        );

        // Simple approach: always hash file contents
        // This is faster than git commands (~5ms vs ~17ms) and avoids
        // unnecessary rebuilds when committing (different hash algorithms)
        let hash = self.hash_source_files(&source_path)?;
        tracing::trace!("WheelCache: Computed hash: {}", hash);
        Ok(hash)
    }

    fn hash_source_files(&self, source_path: &Path) -> Result<String> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        tracing::trace!(
            "WheelCache: Hashing source files in '{}'",
            source_path.display()
        );

        let mut hasher = DefaultHasher::new();
        let mut files_hashed = 0;

        // Hash config files
        for file in &["Cargo.toml", "pyproject.toml"] {
            let path = source_path.join(file);
            if path.exists() {
                if let Ok(content) = std::fs::read(&path) {
                    file.hash(&mut hasher);
                    content.hash(&mut hasher);
                    files_hashed += 1;
                    tracing::trace!("WheelCache:   Hashed: {} ({} bytes)", file, content.len());
                }
            }
        }

        // Recursively hash all .rs files in src/
        let src_dir = source_path.join("src");
        if src_dir.exists() {
            files_hashed +=
                Self::hash_directory_recursive(&src_dir, source_path, "rs", &mut hasher)?;
        }

        // Recursively hash all .py files in python/
        let python_dir = source_path.join("python");
        if python_dir.exists() {
            files_hashed +=
                Self::hash_directory_recursive(&python_dir, source_path, "py", &mut hasher)?;
        }

        let hash = hasher.finish();
        let result = format!("{:016x}", hash);
        tracing::trace!(
            "WheelCache: File hash complete: {} files -> {}",
            files_hashed,
            result
        );
        Ok(result)
    }

    /// Recursively hash all files with given extension in a directory.
    fn hash_directory_recursive(
        dir: &Path,
        base_path: &Path,
        extension: &str,
        hasher: &mut std::collections::hash_map::DefaultHasher,
    ) -> Result<usize> {
        use std::hash::Hash;

        let mut files_hashed = 0;

        let entries = std::fs::read_dir(dir).map_err(|e| {
            StreamError::Runtime(format!(
                "Failed to read directory '{}': {}",
                dir.display(),
                e
            ))
        })?;

        // Collect and sort entries for deterministic ordering
        let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();

        for path in paths {
            if path.is_dir() {
                // Recurse into subdirectories
                files_hashed +=
                    Self::hash_directory_recursive(&path, base_path, extension, hasher)?;
            } else if path.extension().is_some_and(|ext| ext == extension) {
                if let Ok(content) = std::fs::read(&path) {
                    let rel_path = path.strip_prefix(base_path).unwrap_or(&path);
                    // Hash relative path (for determinism) and content
                    rel_path.to_string_lossy().hash(hasher);
                    content.hash(hasher);
                    files_hashed += 1;
                    tracing::trace!(
                        "WheelCache:   Hashed: {} ({} bytes)",
                        rel_path.display(),
                        content.len()
                    );
                }
            }
        }

        Ok(files_hashed)
    }

    fn build_wheel(&self) -> Result<()> {
        let source_path = find_streamlib_python_source().ok_or_else(|| {
            StreamError::Configuration("Cannot find streamlib-python source".into())
        })?;

        tracing::info!(
            "WheelCache: Running maturin build in '{}'",
            source_path.display()
        );

        let output = Command::new("maturin")
            .args([
                "build",
                "--release",
                "-o",
                self.wheels_dir.to_str().unwrap(),
            ])
            .current_dir(&source_path)
            .output()
            .map_err(|e| StreamError::Runtime(format!("Failed to run maturin: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            return Err(StreamError::Runtime(format!(
                "maturin build failed:\nstdout: {}\nstderr: {}",
                stdout, stderr
            )));
        }

        tracing::info!("WheelCache: Wheel built successfully");
        Ok(())
    }
}

/// Find the path to streamlib-python source (for development mode).
pub fn find_streamlib_python_source() -> Option<PathBuf> {
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

        // We might BE in streamlib-python
        let path = PathBuf::from(&manifest_dir);
        if path.join("pyproject.toml").exists()
            && path.file_name().is_some_and(|n| n == "streamlib-python")
        {
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

/// Check if we're in development mode (source available).
pub fn is_dev_mode() -> bool {
    // Check for explicit env var first
    if let Ok(val) = std::env::var("STREAMLIB_DEV_MODE") {
        return val == "1" || val.to_lowercase() == "true";
    }
    // Otherwise: dev mode if source directory exists
    find_streamlib_python_source().is_some()
}
